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
use alloy_sol_types::SolCall;
use call_crypto::keccak256;
use call_governance::precompile::IProtocolGovernance;
use call_precompile::GOVERNANCE_ADDRESS;

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
fn read_proposal_status<S: call_evm::backend::ProtocolStorage>(evm: &S, proposal_id: u64) -> u8 {
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

/// Governance proposal full lifecycle: submit -> vote -> queue -> auto-execute.
/// Uses EVM transactions calling the governance precompile at 0x203.
#[test]
fn test_governance_proposal_full_lifecycle() {
    let mut node = TestNode::new();

    let (_proposer_secret, proposer) = test_keypair();
    let (_voter_secret, voter_addr) = test_keypair();

    // Stake a validator so there is a proposer
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        let _ = consensus
            .stake_validator(provider.state_mut(), voter_addr, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Seed EVM storage for proposer fees + deposit (10_000 CALL)
    // Also seed native EVM balance for gas payment
    {
        let mut provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(), call_protocol::CALL_ASSET_ID, proposer, one_million_call() * 3,
        );
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(), call_protocol::CALL_ASSET_ID, voter_addr, one_million_call(),
        );
        provider.state_mut().set_balance(proposer, call_primitives::U256::from(100_000_000_000u128));
        provider.state_mut().set_balance(voter_addr, call_primitives::U256::from(100_000_000_000u128));
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    let proposal_id = 1u64;

    // Step 1: Submit proposal (proposal_id = 1)
    // Needs ~600k gas: 17 SSTOREs (~22k each) + SLOADs + dispatch overhead.
    let submit_data = IProtocolGovernance::submitProposalCall {
        proposalType: 0, // ParameterChange
        title: "My Proposal".into(),
        description: "Double the max block size".into(),
        executionData: alloy_primitives::Bytes::from_static(b""),
    }
    .abi_encode();
    let submit_tx = call_evm::EvmTransaction {
        caller: proposer,
        nonce: 0,
        gas_limit: 5_000_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(GOVERNANCE_ADDRESS).into_array()[..20]
        )),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(submit_data),
        chain_id: 1,
    };
    node.insert_evm_tx(submit_tx);
    let result = node.produce_block(1_000_000);
    assert!(result.is_some());

    // Proposal starts Pending (review_period = 10 blocks).
    // Produce empty blocks until it becomes Active.
    for i in 0..15 {
        node.produce_block(1_000_001 + i);
    }

    // Verify proposal is Active (status = 1)
    {
        let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        assert_eq!(
            read_proposal_status(provider.state(), proposal_id),
            1,
            "proposal should be Active after review period"
        );
    }

    // Step 2: Vote yes
    let mut vote_args = vec![0u8; 64];
    vote_args[24..32].copy_from_slice(&proposal_id.to_be_bytes());
    vote_args[63] = 1; // Yes = 1
    let vote_tx = call_evm::EvmTransaction {
        caller: voter_addr,
        nonce: 0,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(GOVERNANCE_ADDRESS).into_array()[..20]
        )),
        value: call_primitives::U256::ZERO,
        data: gov_calldata(&[0xb0, 0x40, 0xd1, 0x66], &vote_args),
        chain_id: 1,
    };
    node.insert_evm_tx(vote_tx);
    node.produce_block(1_000_020);

    // Step 3: Queue proposal (one yes vote = 100% quorum)
    let mut queue_args = vec![0u8; 32];
    queue_args[24..32].copy_from_slice(&proposal_id.to_be_bytes());
    let queue_tx = call_evm::EvmTransaction {
        caller: proposer,
        nonce: 1,
        gas_limit: 500_000,
        gas_price: 10,
        to: Some(call_primitives::Address::from_slice(
            &alloy_primitives::Address::from(GOVERNANCE_ADDRESS).into_array()[..20]
        )),
        value: call_primitives::U256::ZERO,
        data: gov_calldata(&[0x92, 0x6c, 0x46, 0xb2], &queue_args),
        chain_id: 1,
    };
    node.insert_evm_tx(queue_tx);
    node.produce_block(1_000_021);

    // Verify proposal is Queued (status = 2)
    {
        let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        assert_eq!(
            read_proposal_status(provider.state(), proposal_id),
            2,
            "proposal should be Queued"
        );
    }

    // Advance past timelock (timelock = 100 blocks).
    // GovernanceAdvancer auto-executes once execution_block is reached.
    for i in 0..110 {
        node.produce_block(1_000_022 + i);
    }

    // Verify proposal is Executed (status = 3)
    {
        let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        assert_eq!(
            read_proposal_status(provider.state(), proposal_id),
            3,
            "proposal should be Executed after full lifecycle"
        );
    }
}
