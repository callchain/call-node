//! E2E test: Governance proposal lifecycle via TestNode harness
//!
//! Validates that governance instructions (submit, vote, queue, execute)
//! are correctly processed during block production.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::{Address, ValidatorId};
use call_protocol::instructions::Instruction;
use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

/// Governance proposal full lifecycle: submit → vote → queue → execute.
#[test]
fn test_governance_proposal_full_lifecycle() {
    let mut node = TestNode::new();

    let (proposer_secret, proposer) = test_keypair();
    let (validator_secret, validator_addr) = test_keypair();

    // Register validator in consensus
    let val_id: ValidatorId = {
        let mut consensus = node.consensus.write().unwrap();
        let id = consensus
            .stake_validator(validator_addr, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset();
        id
    };

    // Also register in validator state manager (used for block execution validators list)
    {
        let mut validator_mgr = node.state.validator_state.write().unwrap();
        validator_mgr.register_validator_from_stake(
            val_id,
            call_consensus::ValidatorStake {
                validator_id: val_id,
                address: validator_addr,
                ed25519_pubkey: [1u8; 32],
                staked_call: one_million_call(),
                self_stake: one_million_call(),
                delegated_call: 0,
                rewards: 0,
                slash_history: vec![],
                unbonding_start: None,
                bls_pubkey: [0u8; 48],
            },
        );
    }

    // Fund proposer with enough for deposit + gas
    {
        node.state
            .balance_state
            .write()
            .unwrap()
            .balances
            .set_balance(0, proposer, one_million_call() * 3)
            .unwrap();
    }

    // Register validator in governance manager and use short periods for testing
    {
        let mut gov = node.state.governance.write().unwrap();
        gov.register_validator(1, validator_addr);
        gov.config.review_period_blocks = 5;
        gov.config.voting_period_blocks = 10;
        gov.config.timelock_period_blocks = 5;
        gov.config.execution_timeout_blocks = 100;
    }

    // Step 1: Submit proposal
    let proposal_tx = sign_tx(
        &proposer_secret,
        ProtocolTransaction {
            sender: proposer,
            nonce: 1,
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
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        },
    );
    node.insert_tx(proposal_tx);
    node.produce_block(1_000_000);

    // Proposal should exist with Pending state
    let proposal_id = {
        let gov = node.state.governance.read().unwrap();
        let proposals: Vec<u64> = gov.get_all_proposals().iter().map(|p| *p.0).collect();
        assert!(!proposals.is_empty(), "proposal should have been created");
        proposals[0]
    };

    // Step 2: Vote yes from validator
    let vote_tx = sign_tx(
        &validator_secret,
        ProtocolTransaction {
            sender: validator_addr,
            nonce: 1,
            instructions: vec![Instruction::GovernanceVote {
                proposal_id,
                vote: call_governance::Vote::Yes,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        },
    );
    // Advance past review period (review_period = 5)
    for i in 0..6 {
        node.produce_block(1_000_000 + (1 + i) * 250);
    }

    // Step 2: Vote yes from validator
    node.insert_tx(vote_tx);
    node.produce_block(1_000_000 + 7 * 250);

    // Advance past voting period (voting_period = 10)
    for i in 0..10 {
        node.produce_block(1_000_000 + (8 + i) * 250);
    }

    // Step 3: Queue proposal
    let queue_tx = sign_tx(
        &proposer_secret,
        ProtocolTransaction {
            sender: proposer,
            nonce: 2,
            instructions: vec![Instruction::GovernanceQueue { proposal_id }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        },
    );
    node.insert_tx(queue_tx);
    node.produce_block(1_000_000 + 18 * 250);

    // Advance past timelock (timelock_period = 5)
    for i in 0..6 {
        node.produce_block(1_000_000 + (19 + i) * 250);
    }

    // Step 4: Execute proposal
    let exec_tx = sign_tx(
        &proposer_secret,
        ProtocolTransaction {
            sender: proposer,
            nonce: 3,
            instructions: vec![Instruction::GovernanceExecute { proposal_id }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        },
    );
    node.insert_tx(exec_tx);
    node.produce_block(1_000_000 + 25 * 250);

    // Verify proposal is executed
    let gov = node.state.governance.read().unwrap();
    let proposal = gov.get_proposal(proposal_id).expect("proposal should exist");
    assert_eq!(
        proposal.state,
        call_governance::ProposalState::Executed,
        "proposal should be executed after full lifecycle"
    );
}
