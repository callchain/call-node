//! E2E test: Governance proposal lifecycle via TestNode harness
//!
//! Validates that governance instructions (submit, vote, queue, execute)
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
use call_protocol::instructions::Instruction;
use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};

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

/// Governance proposal full lifecycle: submit → vote → queue → execute.
/// EVM governance uses hardcoded constants:
///   - PROPOSAL_DEPOSIT = 10_000 CALL
///   - GOV_TIMELOCK_BLOCKS = 100 blocks
///   - GOV_QUORUM_BPS = 3_333 (33.33%)
#[test]
fn test_governance_proposal_full_lifecycle() {
    let mut node = TestNode::new();

    let (proposer_secret, proposer) = test_keypair();
    let (voter_secret, voter_addr) = test_keypair();

    // Stake a validator so there is a proposer
    {
        let mut consensus = node.consensus.write().unwrap();
        let _ = consensus
            .stake_validator(voter_addr, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset();
    }

    // Seed EVM storage for proposer fees + deposit (10_000 CALL)
    {
        let mut evm = node.state.evm_state.write().unwrap();
        call_consensus::exec::evm_instructions::seed_balance(
            &mut *evm, call_protocol::CALL_ASSET_ID, proposer, one_million_call() * 3,
        );
        call_consensus::exec::evm_instructions::seed_balance(
            &mut *evm, call_protocol::CALL_ASSET_ID, voter_addr, one_million_call(),
        );
    }

    // Step 1: Submit proposal (proposal_id = 1)
    let proposal_tx = sign_tx(
        &proposer_secret,
        ProtocolTransaction {
            sender: proposer,
            nonce: 0,
            instructions: vec![Instruction::GovernanceSubmitProposal {
                proposal_type: call_governance::ProposalType::ParameterChange {
                    param_id: "max_block_size".into(),
                    new_value: "10000000".into(),
                },
                title: "Increase block size".into(),
                description: "Double the max block size".into(),
                execution_data: vec![],
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            max_priority_fee: 1,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        },
    );
    node.insert_tx(proposal_tx);
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
    let vote_tx = sign_tx(
        &voter_secret,
        ProtocolTransaction {
            sender: voter_addr,
            nonce: 0,
            instructions: vec![Instruction::GovernanceVote {
                proposal_id,
                vote: call_governance::Vote::Yes,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            max_priority_fee: 1,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        },
    );
    node.insert_tx(vote_tx);
    node.produce_block(1_000_001);

    // Step 3: Queue proposal (one yes vote = 100% quorum)
    let queue_tx = sign_tx(
        &proposer_secret,
        ProtocolTransaction {
            sender: proposer,
            nonce: 1,
            instructions: vec![Instruction::GovernanceQueue { proposal_id }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            max_priority_fee: 1,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        },
    );
    node.insert_tx(queue_tx);
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
    let exec_tx = sign_tx(
        &proposer_secret,
        ProtocolTransaction {
            sender: proposer,
            nonce: 2,
            instructions: vec![Instruction::GovernanceExecute { proposal_id }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            max_priority_fee: 1,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        },
    );
    node.insert_tx(exec_tx);
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
