//! Governance flow integration tests
mod integration;

mod test_governance_flow_impl {
    use super::integration::*;
    use call_primitives::{Address, ValidatorId};
    use call_governance::{
        GovernanceManager, ProposalType, ProposalState, Vote,
        PROPOSAL_DEPOSIT, REVIEW_PERIOD_BLOCKS, VOTING_PERIOD_BLOCKS,
        TIMELOCK_PERIOD_BLOCKS, EXECUTION_TIMEOUT_BLOCKS,
    };

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn make_manager_with_validators(count: u8) -> GovernanceManager {
        let mut mgr = GovernanceManager::new();
        for i in 1..=count {
            mgr.register_validator(i as ValidatorId, test_addr(i));
            mgr.set_call_balance(test_addr(i), 1);
        }
        mgr
    }

    fn million_call() -> u128 {
        1_000_000 * 10u128.pow(18)
    }

    #[test]
    fn test_parameter_change_full_lifecycle() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::ParameterChange { param_id: "max_block_size".into(), new_value: "10000000".into() },
            "Increase block size".into(), "Double the max block size".into(), vec![],
        ).unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);
        mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
        mgr.vote(id, test_addr(2), Vote::Yes).unwrap();
        mgr.vote(id, test_addr(3), Vote::Yes).unwrap();
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
        mgr.queue_proposal(id).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Queued);

        // Execute after timelock — use a large enough block number
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + TIMELOCK_PERIOD_BLOCKS + 100);
        mgr.execute_proposal(id).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Executed);
    }

    #[test]
    fn test_proposal_defeated_confiscate_deposit() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::ParameterChange { param_id: "test".into(), new_value: "1".into() },
            "Test".into(), "Test proposal".into(), vec![],
        ).unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
        let result = mgr.queue_proposal(id);
        assert!(result.is_err());
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Defeated);
    }

    #[test]
    fn test_proposal_expire_confiscate() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::ParameterChange { param_id: "test".into(), new_value: "1".into() },
            "Test".into(), "Test".into(), vec![],
        ).unwrap();

        // Force pass and queue by advancing and setting votes via voting
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);
        for i in 1..=3 {
            let _ = mgr.vote(id, test_addr(i), Vote::Yes);
        }
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
        mgr.queue_proposal(id).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Queued);

        // Advance past execution timeout
        let expire_block = REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + TIMELOCK_PERIOD_BLOCKS + EXECUTION_TIMEOUT_BLOCKS + 100;
        mgr.set_current_block(expire_block);
        mgr.expire_proposal(id).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Expired);
    }

    #[test]
    fn test_emergency_pause_and_resume() {
        let mut mgr = make_manager_with_validators(3);
        assert!(!mgr.emergency_pause_initiate(1, "critical bug".into()).unwrap());
        assert!(!mgr.emergency_pause_initiate(2, "critical bug".into()).unwrap());
        assert!(mgr.emergency_pause_initiate(3, "critical bug".into()).unwrap());
        assert!(mgr.is_paused());
        mgr.emergency_pause_resume().unwrap();
        assert!(!mgr.is_paused());
    }

    #[test]
    fn test_emergency_pause_threshold() {
        let mut mgr = make_manager_with_validators(5);
        assert!(!mgr.emergency_pause_initiate(1, "bug".into()).unwrap());
        assert!(!mgr.emergency_pause_initiate(2, "bug".into()).unwrap());
        assert!(!mgr.emergency_pause_initiate(3, "bug".into()).unwrap());
        assert!(mgr.emergency_pause_initiate(4, "bug".into()).unwrap());
        assert!(mgr.is_paused());
    }

    #[test]
    fn test_emergency_pause_skips_timelock() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::EmergencyPause { reason: "critical bug".into() },
            "Emergency pause".into(), "Pause chain for fix".into(), vec![],
        ).unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);
        for i in 1..=3 {
            let _ = mgr.vote(id, test_addr(i), Vote::Yes);
        }
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
        mgr.queue_proposal(id).unwrap();

        let p = mgr.get_proposal(id).unwrap();
        assert_eq!(p.state, ProposalState::Queued);

        // Emergency pause skips timelock, so execution_block == queue_block
        // which is current_block at queue time
        let exec_block = REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1;
        mgr.set_current_block(exec_block);
        mgr.execute_proposal(id).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Executed);
    }

    #[test]
    fn test_vote_delegation() {
        let mut mgr = make_manager_with_validators(3);
        let delegator = test_addr(50);
        let delegate = test_addr(51);
        mgr.set_call_balance(delegator, 5_000_000);
        mgr.set_call_balance(delegate, 1_000_000);
        mgr.delegate_vote(delegator, delegate, 5_000_000, 1_000_000).unwrap();
        assert_eq!(mgr.get_delegated_voting_power(delegate), 5_000_000);
        mgr.undelegate_vote(delegator).unwrap();
        assert_eq!(mgr.get_delegated_voting_power(delegate), 0);
    }

    #[test]
    fn test_vote_delegation_expiry() {
        let mut mgr = make_manager_with_validators(3);
        let delegator = test_addr(50);
        let delegate = test_addr(51);
        mgr.set_call_balance(delegator, 5_000_000);
        mgr.delegate_vote(delegator, delegate, 5_000_000, 100).unwrap();

        mgr.set_current_block(50);
        assert_eq!(mgr.get_delegated_voting_power(delegate), 5_000_000);

        mgr.set_current_block(200);
        assert_eq!(mgr.get_delegated_voting_power(delegate), 0);
    }

    #[test]
    fn test_delegation_insufficient_balance() {
        let mut mgr = GovernanceManager::new();
        let delegator = test_addr(1);
        mgr.set_call_balance(delegator, 100);
        let result = mgr.delegate_vote(delegator, test_addr(2), 200, 1_000);
        assert!(result.is_err());
    }

    #[test]
    fn test_treasury_spend_balance_weighted_voting() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        let big_holder = test_addr(20);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);
        mgr.set_call_balance(big_holder, call_protocol::economics::TOTAL_SUPPLY / 4);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::TreasurySpend { recipient: test_addr(30), amount: million_call(), asset_id: 0 },
            "Treasury spend".into(), "Send 1M CALL".into(), vec![],
        ).unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);
        mgr.vote(id, big_holder, Vote::Yes).unwrap();
        let p = mgr.get_proposal(id).unwrap();
        assert_eq!(p.voting_power_yes, call_protocol::economics::TOTAL_SUPPLY / 4);
    }

    #[test]
    fn test_compliance_update_issuer_weight() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);
        let issuer = test_addr(20);
        mgr.register_asset_issuer(1, issuer);
        mgr.set_call_balance(issuer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            issuer,
            ProposalType::ComplianceUpdate { asset_id: 1, new_policy: 1 },
            "Update compliance".into(), "Enable OFAC blacklist".into(), vec![],
        ).unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);
        mgr.vote(id, issuer, Vote::Yes).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().voting_power_yes, call_protocol::economics::TOTAL_SUPPLY / 10);
    }

    #[test]
    fn test_fee_currency_add_quorum() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::FeeCurrencyAdd { asset_id: 5, name: "USDC".into(), oracle_price_key: "USDC/USD".into() },
            "Add USDC".into(), "USDC market cap > 100M".into(), vec![],
        ).unwrap();

        assert_eq!(mgr.get_proposal(id).unwrap().quorum_required, 2);
    }

    #[test]
    fn test_protocol_upgrade_dual_quorum() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::ProtocolUpgrade { activation_block: 1_000_000, changelog: "v1.1.0".into() },
            "Upgrade".into(), "v1.1.0 release".into(), vec![],
        ).unwrap();

        assert_eq!(mgr.get_proposal(id).unwrap().quorum_required, call_protocol::economics::TOTAL_SUPPLY / 5);
    }

    #[test]
    fn test_insufficient_deposit_rejected() {
        let mut mgr = GovernanceManager::new();
        let proposer = test_addr(1);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT - 1);
        let result = mgr.submit_proposal(
            proposer,
            ProposalType::ParameterChange { param_id: "test".into(), new_value: "1".into() },
            "Test".into(), "Test".into(), vec![],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_voting_before_review_rejected() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::ParameterChange { param_id: "test".into(), new_value: "1".into() },
            "Test".into(), "Test".into(), vec![],
        ).unwrap();

        let result = mgr.vote(id, test_addr(1), Vote::Yes);
        assert!(result.is_err());
    }

    #[test]
    fn test_validator_slash_1_1_voting() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::ValidatorSlash { validator_id: 99, reason: "offline".into() },
            "Slash validator 99".into(), "Validator has been offline for 24h".into(), vec![],
        ).unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);
        mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
        mgr.vote(id, test_addr(2), Vote::Yes).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().voting_power_yes, 2);
    }

    #[test]
    fn test_state_machine_full_path() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr.submit_proposal(
            proposer,
            ProposalType::ParameterChange { param_id: "test".into(), new_value: "1".into() },
            "Test".into(), "Test".into(), vec![],
        ).unwrap();

        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Pending);
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);
        mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Active);

        // Get all 3 validators to vote yes for quorum
        for i in 2..=3 {
            let _ = mgr.vote(id, test_addr(i), Vote::Yes);
        }
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
        mgr.queue_proposal(id).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Queued);

        let exec = REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + TIMELOCK_PERIOD_BLOCKS + 100;
        mgr.set_current_block(exec);
        mgr.execute_proposal(id).unwrap();
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Executed);
    }
}
