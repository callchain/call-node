use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::*;

// ── Proposal Submit & Vote ────────────────────────────────────────

#[test]
fn test_proposal_submit_and_vote() {
    let mut mgr = make_manager_with_validators(3);

    // Give proposer enough balance for deposit
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "max_block_size".into(),
                new_value: "10000000".into(),
            },
            "Increase block size".into(),
            "Double the max block size".into(),
            vec![],
        )
        .unwrap();

    assert_eq!(id, 0);

    // Deposit was deducted
    let balance = mgr.call_balances.get(&proposer).unwrap();
    assert_eq!(*balance, mgr.config.proposal_deposit);

    // Advance to voting period
    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

    // Validators vote yes
    mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
    mgr.vote(id, test_addr(2), Vote::Yes).unwrap();
    mgr.vote(id, test_addr(3), Vote::No).unwrap();

    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.voting_power_yes, 2); // 2 validators voted yes
    assert_eq!(proposal.voting_power_no, 1);
}

// ── Quorum Pass ───────────────────────────────────────────────────

#[test]
fn test_proposal_passes_quorum() {
    let mut mgr = make_manager_with_validators(3);

    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test proposal".into(),
            vec![],
        )
        .unwrap();

    // Advance past voting
    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);

    // All 3 validators voted yes during voting (we'll simulate by directly setting)
    {
        let p = mgr.proposals.get_mut(&id).unwrap();
        p.voting_power_yes = 3;
    }

    // Queue should pass and move to queued
    mgr.queue_proposal(id).unwrap();

    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.state, ProposalState::Queued);
    assert!(proposal.execution_block.is_some());
}

// ── Quorum Fail ───────────────────────────────────────────────────

#[test]
fn test_proposal_fails_quorum() {
    let mut mgr = make_manager_with_validators(3);

    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test proposal".into(),
            vec![],
        )
        .unwrap();

    // Advance past voting without any votes
    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);

    let result = mgr.queue_proposal(id);
    assert!(result.is_err());

    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.state, ProposalState::Defeated);
}

// ── Timelock Execution ────────────────────────────────────────────

#[test]
fn test_proposal_timelock_execution() {
    let mut mgr = make_manager_with_validators(3);

    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test proposal".into(),
            vec![],
        )
        .unwrap();

    // Set votes, advance past voting
    {
        let p = mgr.proposals.get_mut(&id).unwrap();
        p.voting_power_yes = 3;
    }
    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
    mgr.queue_proposal(id).unwrap();

    // Cannot execute before timelock
    let result = mgr.execute_proposal(id, proposer);
    assert!(result.is_err());

    // Advance past timelock
    let exec_block = mgr.get_proposal(id).unwrap().execution_block.unwrap();
    mgr.set_current_block(exec_block + 1);
    mgr.execute_proposal(id, proposer).unwrap();

    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.state, ProposalState::Executed);

    // Deposit returned to proposer
    let balance = mgr.call_balances.get(&proposer).unwrap();
    assert_eq!(*balance, mgr.config.proposal_deposit * 2 - mgr.config.proposal_deposit + mgr.config.proposal_deposit); // deposit deducted then restored
}

// ── Proposal Expire ───────────────────────────────────────────────

#[test]
fn test_proposal_expire_confiscate_deposit() {
    let mut mgr = make_manager_with_validators(3);

    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test proposal".into(),
            vec![],
        )
        .unwrap();

    // Queue the proposal
    {
        let p = mgr.proposals.get_mut(&id).unwrap();
        p.voting_power_yes = 3;
    }
    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
    mgr.queue_proposal(id).unwrap();

    // Advance past execution timeout
    let exec_block = mgr.get_proposal(id).unwrap().execution_block.unwrap();
    mgr.set_current_block(exec_block + EXECUTION_TIMEOUT_BLOCKS + 1);

    mgr.expire_proposal(id).unwrap();

    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.state, ProposalState::Expired);

    // Deposit confiscated
    assert!(mgr.deposits.get(&proposer).is_none());
}

// ── Validator 1=1 Voting ─────────────────────────────────────────

#[test]
fn test_validator_voting_1_1() {
    let mut mgr = make_manager_with_validators(3);

    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ValidatorSlash {
                validator_id: 99,
                reason: "offline".into(),
            },
            "Slash offline validator".into(),
            "Validator 99 has been offline".into(),
            vec![],
        )
        .unwrap();

    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

    // Each validator gets 1 vote regardless of balance
    mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
    mgr.vote(id, test_addr(2), Vote::Yes).unwrap();

    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.voting_power_yes, 2);
}

// ── CALL Holder Balance-Weighted Voting ───────────────────────────

#[test]
fn test_call_holder_voting_balance_weighted() {
    let mut mgr = make_manager_with_validators(3);

    let proposer = test_addr(10);
    let big_holder = test_addr(20);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);
    mgr.set_call_balance(big_holder, TOTAL_SUPPLY / 4); // 25% of supply

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::TreasurySpend {
                recipient: test_addr(30),
                amount: one_million_call(),
                asset_id: 0,
            },
            "Treasury spend".into(),
            "Send 1M CALL to address".into(),
            vec![],
        )
        .unwrap();

    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

    // Big holder votes yes — voting power = their CALL balance
    mgr.vote(id, big_holder, Vote::Yes).unwrap();

    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.voting_power_yes, TOTAL_SUPPLY / 4);
}

// ── Vote Delegation ───────────────────────────────────────────────

#[test]
fn test_vote_delegation() {
    let mut mgr = make_manager_with_validators(3);

    let delegator = test_addr(50);
    let delegate = test_addr(51);
    mgr.set_call_balance(delegator, 5_000_000);
    mgr.set_call_balance(delegate, 1_000_000);

    mgr.delegate_vote(delegator, delegate, 5_000_000, 1_000_000)
        .unwrap();

    // Delegated power for delegate
    let delegated = mgr.get_delegated_voting_power(delegate);
    assert_eq!(delegated, 5_000_000);

    // Undelegate
    mgr.undelegate_vote(delegator).unwrap();
    let delegated = mgr.get_delegated_voting_power(delegate);
    assert_eq!(delegated, 0);
}

// ── Emergency Pause ───────────────────────────────────────────────

#[test]
fn test_emergency_pause_2_3_signatures() {
    let mut mgr = make_manager_with_validators(3);

    // With 3 validators and 6667 BPS, ceil(3 * 6667 / 10000) = 3, so ALL 3 needed
    let result = mgr
        .emergency_pause_initiate(1, "critical bug".into())
        .unwrap();
    assert!(!result); // need more signatures

    let result = mgr
        .emergency_pause_initiate(2, "critical bug".into())
        .unwrap();
    assert!(!result); // still need one more

    let result = mgr
        .emergency_pause_initiate(3, "critical bug".into())
        .unwrap();
    assert!(result); // 3/3 reached, pause activated

    assert!(mgr.is_paused());
    assert_eq!(mgr.emergency_pause.pause_reason, "critical bug");

    // Resume
    mgr.emergency_pause_resume().unwrap();
    assert!(!mgr.is_paused());
}

// ── Compliance Update Joint Voting ────────────────────────────────

#[test]
fn test_compliance_update_issuer_validator_joint() {
    let mut mgr = make_manager_with_validators(3);

    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    // Register an issuer
    let issuer = test_addr(20);
    mgr.register_asset_issuer(1, issuer);
    mgr.set_call_balance(issuer, mgr.config.proposal_deposit);

    let id = mgr
        .submit_proposal(
            issuer,
            ProposalType::ComplianceUpdate {
                asset_id: 1,
                new_policy: 1, // OFAC blacklist
            },
            "Update compliance".into(),
            "Enable OFAC blacklist".into(),
            vec![],
        )
        .unwrap();

    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

    // Issuer votes — should have weight of total_supply / 10
    mgr.vote(id, issuer, Vote::Yes).unwrap();

    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.voting_power_yes, TOTAL_SUPPLY / 10);

    // A validator also votes — adds 1 more
    mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.voting_power_yes, TOTAL_SUPPLY / 10 + 1);
}

// ── Insufficient Deposit ──────────────────────────────────────────

#[test]
fn test_proposal_insufficient_deposit() {
    let mut mgr = GovernanceManager::new();
    let proposer = test_addr(1);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit - 1);

    let result = mgr.submit_proposal(
        proposer,
        ProposalType::ParameterChange {
            param_id: "test".into(),
            new_value: "1".into(),
        },
        "Test".into(),
        "Test".into(),
        vec![],
    );
    assert!(matches!(result, Err(GovernanceError::InsufficientDeposit)));
}

// ── Voting Period Enforcement ─────────────────────────────────────

#[test]
fn test_voting_before_period_rejected() {
    let mut mgr = make_manager_with_validators(3);
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();

    // Voting hasn't started yet (need to pass REVIEW_PERIOD_BLOCKS)
    let result = mgr.vote(id, test_addr(1), Vote::Yes);
    assert!(matches!(result, Err(GovernanceError::VotingNotStarted)));
}

// ── Fee Currency Proposal Types ───────────────────────────────────

#[test]
fn test_fee_currency_proposal_lifecycle() {
    let mut mgr = make_manager_with_validators(3);
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    // FeeCurrencyAdd proposal
    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::FeeCurrencyAdd {
                asset_id: 5,
                name: "USDC".into(),
                oracle_price_key: "USDC/USD".into(),
            },
            "Add USDC as fee currency".into(),
            "USDC market cap > 100M".into(),
            vec![],
        )
        .unwrap();

    let proposal = mgr.get_proposal(id).unwrap();
    // Simple majority quorum for fee currency proposals
    assert_eq!(proposal.quorum_required, 2); // 3/2 + 1 = 2
}

// ── Proposal State Transitions ────────────────────────────────────

#[test]
fn test_proposal_state_machine_full() {
    let mut mgr = make_manager_with_validators(3);
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();

    let p = mgr.get_proposal(id).unwrap();
    assert_eq!(p.state, ProposalState::Pending);

    // Advance past review into voting
    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

    // Vote
    mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
    let p = mgr.get_proposal(id).unwrap();
    assert_eq!(p.state, ProposalState::Active);

    // Advance past voting, pass
    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
    {
        let p = mgr.proposals.get_mut(&id).unwrap();
        p.voting_power_yes = 3; // ensure quorum
    }
    mgr.queue_proposal(id).unwrap();

    let p = mgr.get_proposal(id).unwrap();
    assert_eq!(p.state, ProposalState::Queued);

    // Advance past timelock, execute
    let exec = p.execution_block.unwrap();
    mgr.set_current_block(exec + 1);
    mgr.execute_proposal(id, proposer).unwrap();

    let p = mgr.get_proposal(id).unwrap();
    assert_eq!(p.state, ProposalState::Executed);
}

// ── Duplicate Vote Prevention ─────────────────────────────────────

#[test]
fn test_voter_cannot_vote_twice_same_proposal() {
    let mut mgr = make_manager_with_validators(3);
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();

    mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);
    mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
    // Second vote by same address should be rejected
    assert!(matches!(
        mgr.vote(id, test_addr(1), Vote::Yes),
        Err(GovernanceError::AlreadyVoted)
    ));

    let proposal = mgr.get_proposal(id).unwrap();
    assert_eq!(proposal.voting_power_yes, 1);
}

// ── Protocol Upgrade Quorum ───────────────────────────────────────

#[test]
fn test_protocol_upgrade_dual_quorum() {
    let mut mgr = make_manager_with_validators(3);
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ProtocolUpgrade {
                activation_block: 1_000_000,
                changelog: "v1.1.0 release".into(),
            },
            "Protocol upgrade".into(),
            "Upgrade to v1.1.0".into(),
            vec![],
        )
        .unwrap();

    let proposal = mgr.get_proposal(id).unwrap();
    // max(2/3 validators, 20% total supply)
    // 2/3 of 3 = 2, 20% of 1B = 200M
    assert_eq!(proposal.quorum_required, TOTAL_SUPPLY / 5);
}

// ── Auto-advance state machine ────────────────────────────────────

#[test]
fn test_advance_auto_transitions() {
    let mut mgr = make_manager_with_validators(3);
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();

    // Initially pending
    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Pending);

    // Advance to review period — should auto-activate
    mgr.advance(REVIEW_PERIOD_BLOCKS + 1);
    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Active);

    // Set votes for quorum
    {
        let p = mgr.proposals.get_mut(&id).unwrap();
        p.voting_power_yes = 3;
    }

    // Advance past voting — should auto-queue and auto-execute
    let voting_end = mgr.get_proposal(id).unwrap().end_block;
    mgr.advance(voting_end + 1);
    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Queued);

    // Advance past timelock — should auto-execute
    let exec = mgr.get_proposal(id).unwrap().execution_block.unwrap();
    mgr.advance(exec + 1);
    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Executed);
}

#[test]
fn test_advance_auto_defeat() {
    let mut mgr = make_manager_with_validators(3);
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();

    // Advance past review
    mgr.advance(REVIEW_PERIOD_BLOCKS + 1);

    // No votes — advance past voting end
    let voting_end = mgr.get_proposal(id).unwrap().end_block;
    mgr.advance(voting_end + 1);

    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Defeated);
    // Deposit confiscated
    assert!(mgr.deposits.get(&proposer).is_none());
}

#[test]
fn test_advance_auto_expire() {
    let mut mgr = make_manager_with_validators(3);
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();

    // Set votes, advance to queued
    {
        let p = mgr.proposals.get_mut(&id).unwrap();
        p.voting_power_yes = 3;
    }
    mgr.advance(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);

    // Should be queued now
    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Queued);

    // Advance past execution timeout
    let exec = mgr.get_proposal(id).unwrap().execution_block.unwrap();
    mgr.advance(exec + EXECUTION_TIMEOUT_BLOCKS + 1);

    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Expired);
    assert!(mgr.deposits.get(&proposer).is_none());
}

// ── Event emission ────────────────────────────────────────────────

#[test]
fn test_advance_emits_events() {
    let mut mgr = make_manager_with_validators(3);
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();

    // Advance to active
    mgr.advance(REVIEW_PERIOD_BLOCKS + 1);
    let events = mgr.drain_events();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], GovernanceEvent::ProposalAdvanced { id: event_id, from: ProposalState::Pending, to: ProposalState::Active } if *event_id == id));

    // Set votes, advance past voting
    {
        let p = mgr.proposals.get_mut(&id).unwrap();
        p.voting_power_yes = 3;
    }
    let voting_end = mgr.get_proposal(id).unwrap().end_block;
    mgr.advance(voting_end + 1);
    let events = mgr.drain_events();
    // Queued transition emits event
    assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalAdvanced { to: ProposalState::Queued, .. })));

    // Advance past timelock to execute
    let exec = mgr.get_proposal(id).unwrap().execution_block.unwrap();
    mgr.advance(exec + 1);
    let events = mgr.drain_events();
    assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalExecuted { id: event_id, .. } if *event_id == id)));

    // drain_events clears the buffer
    assert!(mgr.drain_events().is_empty());
}

// ── Balance source ────────────────────────────────────────────────

#[test]
fn test_balance_source_fallback() {
    let mut mgr = GovernanceManager::new();
    let addr = test_addr(1);

    // Without balance_source, uses call_balances
    mgr.set_call_balance(addr, 500_000);
    assert_eq!(mgr.get_voting_balance(addr), 500_000);

    // With balance_source set, uses external
    mgr.balance_source = Some(Arc::new(|_| 1_000_000));
    assert_eq!(mgr.get_voting_balance(addr), 1_000_000);
}

// ── GovernanceConfig ──────────────────────────────────────────────

#[test]
fn test_config_quorum_calculations() {
    let config = GovernanceConfig::default();

    // With div_ceil and 6667 BPS, small validator sets round up
    assert_eq!(config.validator_quorum(3), 3); // ceil(3 * 6667 / 10000) = 3
    assert_eq!(config.validator_quorum(10), 7); // ceil(10 * 6667 / 10000) = 7

    assert_eq!(config.supply_quorum(), TOTAL_SUPPLY / 5); // 20%
    assert_eq!(config.treasury_quorum(), TOTAL_SUPPLY / 5); // 20%

    assert_eq!(config.simple_majority(3), 2); // ceil(3 * 5001 / 10000) = 2
    assert_eq!(config.emergency_pause_threshold(3), 3); // ceil(3 * 6667 / 10000) = 3
}

#[test]
fn test_config_custom_periods() {
    let config = GovernanceConfig {
        review_period_blocks: 100,
        voting_period_blocks: 500,
        timelock_period_blocks: 1000,
        execution_timeout_blocks: 5000,
        ..GovernanceConfig::default()
    };

    let mut mgr = GovernanceManager::new().with_config(config.clone());
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();

    // Should activate at review_period_blocks (100), not default (691200)
    mgr.advance(101);
    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Active);
}

// ── Rate limiting ─────────────────────────────────────────────────

#[test]
fn test_proposal_rate_limiting() {
    let mut mgr = GovernanceManager::new();
    let proposer = test_addr(1);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 10);

    // First proposal should succeed
    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();
    assert_eq!(id, 0);

    // Immediate second proposal should be rate limited
    let result = mgr.submit_proposal(
        proposer,
        ProposalType::ParameterChange {
            param_id: "test2".into(),
            new_value: "2".into(),
        },
        "Test2".into(),
        "Test2".into(),
        vec![],
    );
    assert!(matches!(result, Err(GovernanceError::ProposalRateLimited(_))));

    // Advance past cooldown and try again
    mgr.set_current_block(PROPOSAL_COOLDOWN_BLOCKS + 1);
    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test2".into(),
                new_value: "2".into(),
            },
            "Test2".into(),
            "Test2".into(),
            vec![],
        )
        .unwrap();
    assert_eq!(id, 1);
}

// ── Full cycle (submit → advance → execute) ───────────────────────

#[test]
fn test_full_lifecycle_with_executor() {
    struct TestExecutor {
        executed_count: AtomicU64,
    }
    impl ProposalExecutor for TestExecutor {
        fn on_proposal_executed(&self, _proposal: &Proposal) -> Result<(), String> {
            self.executed_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let mut mgr = GovernanceManager::new()
        .with_executor(Arc::new(TestExecutor { executed_count: AtomicU64::new(0) }));
    let proposer = test_addr(10);
    mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

    // Register validators
    for i in 1u8..=3 {
        mgr.register_validator(i as u32, test_addr(i));
        mgr.set_call_balance(test_addr(i), 1);
    }

    let id = mgr
        .submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        )
        .unwrap();

    // Set votes for quorum before advancing past voting
    {
        let p = mgr.proposals.get_mut(&id).unwrap();
        p.voting_power_yes = 3; // All 3 validators
    }

    // Step 1: Advance past review + voting to get queued
    mgr.advance(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Queued);

    // Drain events from first advance
    let events = mgr.drain_events();
    assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalAdvanced { to: ProposalState::Queued, .. })));

    // Step 2: Advance past timelock to execute
    let exec = mgr.get_proposal(id).unwrap().execution_block.unwrap();
    mgr.advance(exec + 1);
    assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Executed);

    // Drain events — should include ProposalExecuted
    let events = mgr.drain_events();
    assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalExecuted { id: eid, .. } if *eid == id)));
}
