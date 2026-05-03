//! E2E test: Governance proposal lifecycle via TestNode harness
//!
//! Validates that governance precompile calls (submit, vote, queue, execute)
//! are correctly processed during block production.
//!
//! Governance now writes to EVM storage (GOVERNANCE_ADDRESS 0x203), so this
//! test verifies state by reading EVM slots directly.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use alloy_primitives::U256;
use call_crypto::keccak256;
use call_precompiles::GOVERNANCE_ADDRESS;

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

/// Compute a governance proposal storage slot: keccak256(b"proposal" || id_be || suffix)
fn gov_slot(proposal_id: u64, suffix: &[u8]) -> U256 {
    let mut data = Vec::new();
    data.extend_from_slice(b"proposal");
    data.extend_from_slice(&proposal_id.to_be_bytes());
    data.extend_from_slice(suffix);
    let hash = keccak256(&data);
    U256::from_be_slice(&hash.0)
}

/// Read a proposal status byte from EVM storage.
/// Status values: 0=None, 1=Active, 2=Queued, 3=Executed.
fn read_proposal_status(evm: &call_evm::EvmState, proposal_id: u64) -> u8 {
    let slot = gov_slot(proposal_id, b"status");
    evm.get_storage(&GOVERNANCE_ADDRESS, slot).to_be_bytes::<32>()[31]
}

/// Build EVM transaction calldata for governance precompile.
fn gov_calldata(selector: &[u8; 4], args: &[u8]) -> call_evm::Bytes {
    let mut data = Vec::with_capacity(4 + args.len());
    data.extend_from_slice(selector);
    data.extend_from_slice(args);
    call_evm::Bytes::from(data)
}

/// Governance proposal full lifecycle: submit -> vote -> queue -> execute.
/// Uses EVM transactions calling the governance precompile at 0x203.
#[test]
fn test_governance_proposal_full_lifecycle() {
    let mut node = TestNode::new();

    let (proposer_secret, proposer) = test_keypair();
    let (_voter_secret, voter_addr) = test_keypair();

    // Stake a validator so there is a proposer
    {
        let mut evm_state = node.state.evm_state.write().unwrap();
        let mut consensus = node.consensus.write().unwrap();
        let _ = consensus
            .stake_validator(&mut evm_state, voter_addr, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset(&evm_state);
    }

    // Seed EVM storage for proposer fees + deposit (10_000 CALL)
    // Also seed native EVM balance for gas payment
    {
        let mut evm = node.state.evm_state.write().unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            &mut *evm, call_protocol::CALL_ASSET_ID, proposer, one_million_call() * 3,
        );
        call_consensus::exec::state_accessors::seed_balance(
            &mut *evm, call_protocol::CALL_ASSET_ID, voter_addr, one_million_call(),
        );
        evm.set_balance(proposer, call_primitives::U256::from(100_000_000_000u128));
        evm.set_balance(voter_addr, call_primitives::U256::from(100_000_000_000u128));
    }

    // Step 1: Submit proposal (proposal_id = 1)
    let submit_tx = call_evm::EvmTransaction {
        caller: proposer,
        nonce: 0,
        gas_limit: 300_000,
        gas_price: 1,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(GOVERNANCE_ADDRESS).into_array()[..20]
        )),
        value: call_primitives::U256::ZERO,
        data: gov_calldata(
            &[0x13, 0x16, 0x9e, 0x1c],
            &{
                let mut args = Vec::with_capacity(96);
                args.extend_from_slice(b"My Proposal_____________________");
                args.extend_from_slice(b"Double the max block size_______");
                args.extend_from_slice(&[0xDDu8; 32]);
                args
            },
        ),
        chain_id: 1,
    };
    node.insert_evm_tx(submit_tx);
    let result = node.produce_block(1_000_000);
    assert!(result.is_some());

    let proposal_id = 1u64;

    // Verify proposal is Active (status = 1)
    {
        let evm = node.state.evm_state.read().unwrap();
        assert_eq!(
            read_proposal_status(&*evm, proposal_id),
            1,
            "proposal should be Active after submission"
        );
    }

    // Step 2: Vote yes
    let mut vote_args = vec![0u8; 64];
    vote_args[24..32].copy_from_slice(&proposal_id.to_be_bytes());
    vote_args[63] = 1; // Yes = 1
    let vote_tx = call_evm::EvmTransaction {
        caller: voter_addr,
        nonce: 0,
        gas_limit: 100_000,
        gas_price: 1,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(GOVERNANCE_ADDRESS).into_array()[..20]
        )),
        value: call_primitives::U256::ZERO,
        data: gov_calldata(&[0x02, 0x3d, 0x03, 0x3b], &vote_args),
        chain_id: 1,
    };
    node.insert_evm_tx(vote_tx);
    node.produce_block(1_000_001);

    // Step 3: Queue proposal (one yes vote = 100% quorum)
    let mut queue_args = vec![0u8; 32];
    queue_args[24..32].copy_from_slice(&proposal_id.to_be_bytes());
    let queue_tx = call_evm::EvmTransaction {
        caller: proposer,
        nonce: 1,
        gas_limit: 100_000,
        gas_price: 1,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(GOVERNANCE_ADDRESS).into_array()[..20]
        )),
        value: call_primitives::U256::ZERO,
        data: gov_calldata(&[0xfe, 0x72, 0xd0, 0x10], &queue_args),
        chain_id: 1,
    };
    node.insert_evm_tx(queue_tx);
    node.produce_block(1_000_002);

    // Verify proposal is Queued (status = 2)
    {
        let evm = node.state.evm_state.read().unwrap();
        assert_eq!(
            read_proposal_status(&*evm, proposal_id),
            2,
            "proposal should be Queued"
        );
    }

    // Advance past timelock (GOV_TIMELOCK_BLOCKS = 100)
    for _ in 0..101 {
        node.produce_block(1_000_003);
    }

    // Step 4: Execute proposal
    let mut exec_args = vec![0u8; 32];
    exec_args[24..32].copy_from_slice(&proposal_id.to_be_bytes());
    let exec_tx = call_evm::EvmTransaction {
        caller: proposer,
        nonce: 2,
        gas_limit: 100_000,
        gas_price: 1,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(GOVERNANCE_ADDRESS).into_array()[..20]
        )),
        value: call_primitives::U256::ZERO,
        data: gov_calldata(&[0x50, 0xf7, 0x01, 0xf4], &exec_args),
        chain_id: 1,
    };
    node.insert_evm_tx(exec_tx);
    node.produce_block(1_000_004);

    // Verify proposal is Executed (status = 3)
    {
        let evm = node.state.evm_state.read().unwrap();
        assert_eq!(
            read_proposal_status(&*evm, proposal_id),
            3,
            "proposal should be Executed after full lifecycle"
        );
    }
}
