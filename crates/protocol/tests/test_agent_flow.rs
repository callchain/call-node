//! Agent flow integration tests
mod integration;

mod test_agent_flow_impl {
    use super::integration::*;
    use call_primitives::Address;
    use call_protocol::balances::BalanceState;
    use call_protocol::registry::AssetRegistry;
    use call_protocol::instructions::{AgentPayment, Instruction};
    use call_agent::{
        AgentRegistry, AgentBalances, AgentNonces, AgentFeeConfig, AgentFundingAction,
        FeePayer, DomainProof, AgentError,
    };
    use call_shielded::ShieldedState;

    #[test]
    fn test_agent_register_no_domain_proof() {
        let mut registry = AgentRegistry::new();
        let owner = addr(1);
        let pubkey = [1u8; 64];
        let metadata = [2u8; 32];
        let id = registry.register_agent(owner, pubkey, "test-agent".into(), "https://agent.example.com".into(), metadata, None, 100).unwrap();
        assert_eq!(id, 0);
        let agent = registry.get_agent(0).unwrap();
        assert_eq!(agent.owner, owner);
        assert!(!agent.domain_verified);
    }

    #[test]
    fn test_agent_register_with_dns_proof() {
        let mut registry = AgentRegistry::new();
        let proof = DomainProof::DnsTxt { domain: "agent.example.com".into(), txt_value: "call-agent=0x1234".into() };
        let id = registry.register_agent(addr(1), [1u8; 64], "dns-agent".into(), "https://agent.example.com".into(), [0u8; 32], Some(proof), 100).unwrap();
        assert!(registry.get_agent(id).unwrap().domain_verified);
    }

    #[test]
    fn test_agent_register_with_http_proof() {
        let mut registry = AgentRegistry::new();
        let proof = DomainProof::HttpFile { url: "https://agent.example.com/.well-known/call-agent".into(), expected_content: "agent=0x1234".into() };
        let id = registry.register_agent(addr(1), [1u8; 64], "http-agent".into(), "https://agent.example.com".into(), [0u8; 32], Some(proof), 100).unwrap();
        assert!(registry.get_agent(id).unwrap().domain_verified);
    }

    #[test]
    fn test_agent_multiple_agents_same_owner() {
        let mut registry = AgentRegistry::new();
        let owner = addr(5);
        let metadata = [0u8; 32];
        registry.register_agent(owner, [1u8; 64], "agent-a".into(), "https://a.com".into(), metadata, None, 100).unwrap();
        registry.register_agent(owner, [2u8; 64], "agent-b".into(), "https://b.com".into(), metadata, None, 101).unwrap();
        assert_eq!(registry.get_agents_by_owner(&owner).len(), 2);
    }

    #[test]
    fn test_agent_duplicate_name_rejected() {
        let mut registry = AgentRegistry::new();
        let metadata = [0u8; 32];
        registry.register_agent(addr(1), [1u8; 64], "dup".into(), "https://a.com".into(), metadata, None, 100).unwrap();
        let result = registry.register_agent(addr(2), [2u8; 64], "dup".into(), "https://b.com".into(), metadata, None, 101);
        assert!(matches!(result, Err(AgentError::AgentAlreadyRegistered(_))));
    }

    #[test]
    fn test_agent_owner_grant_funds() {
        let mut balances = AgentBalances::new();
        let owner = addr(1);
        balances.grant_funds(owner, 0, 1, 5_000);
        assert_eq!(balances.get_balance(owner, 0, 1), 5_000);
        balances.top_up(owner, 0, 1, 3_000);
        assert_eq!(balances.get_balance(owner, 0, 1), 8_000);
    }

    #[test]
    fn test_agent_revoke_funds() {
        let mut balances = AgentBalances::new();
        let owner = addr(1);
        balances.grant_funds(owner, 0, 1, 5_000);
        let revoked = balances.revoke_funds(owner, 0, 1);
        assert_eq!(revoked, 5_000);
        assert_eq!(balances.get_balance(owner, 0, 1), 0);
    }

    #[test]
    fn test_agent_balances_isolated_by_owner() {
        let mut balances = AgentBalances::new();
        balances.grant_funds(addr(1), 0, 1, 1_000);
        balances.grant_funds(addr(2), 0, 1, 2_000);
        assert_eq!(balances.get_balance(addr(1), 0, 1), 1_000);
        assert_eq!(balances.get_balance(addr(2), 0, 1), 2_000);
    }

    #[test]
    fn test_agent_deduct_from_balance() {
        let mut balances = AgentBalances::new();
        let owner = addr(1);
        balances.grant_funds(owner, 0, 1, 1_000);
        balances.deduct(owner, 0, 1, 300).unwrap();
        assert_eq!(balances.get_balance(owner, 0, 1), 700);
        assert!(balances.deduct(owner, 0, 1, 701).is_err());
    }

    #[test]
    fn test_agent_nonce_sequential() {
        let mut nonces = AgentNonces::new();
        let owner = addr(1);
        assert_eq!(nonces.get_nonce(owner, 0), 0);
        nonces.check_and_increment(owner, 0, 0).unwrap();
        assert_eq!(nonces.get_nonce(owner, 0), 1);
    }

    #[test]
    fn test_agent_nonce_stale_duplicate_rejected() {
        let mut nonces = AgentNonces::new();
        let owner = addr(1);
        nonces.check_and_increment(owner, 0, 0).unwrap();
        assert!(nonces.check_and_increment(owner, 0, 0).is_err());
    }

    #[test]
    fn test_agent_fee_config_defaults() {
        let config = AgentFeeConfig::default();
        assert_eq!(config.fee_payer, FeePayer::SelfPay);
        assert_eq!(config.owner_max_daily_fee, u128::MAX);
    }

    #[test]
    fn test_agent_fee_config_owner_pays() {
        let config = AgentFeeConfig {
            fee_payer: FeePayer::OwnerPays,
            owner_max_daily_fee: 10_000,
            owner_max_total_fee: 100_000,
            require_owner_signature_above: 5_000,
        };
        assert_eq!(config.fee_payer, FeePayer::OwnerPays);
    }

    #[test]
    fn test_agent_fee_config_third_party() {
        let payer = addr(99);
        let config = AgentFeeConfig { fee_payer: FeePayer::ThirdParty { payer }, ..Default::default() };
        assert_eq!(config.fee_payer, FeePayer::ThirdParty { payer });
    }

    #[test]
    fn test_agent_pay_instruction_executes() {
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        let compliance = call_protocol::compliance::ComplianceEngine::new();
        let sender = addr(1);
        let receiver = addr(2);
        let asset_id = setup_asset(&mut balances, &mut registry, "AGENT", addr(10), sender, 5_000);

        let instructions = vec![Instruction::AgentPay { payment: AgentPayment { agent_id: 0, asset_id, to: receiver, amount: 1_000 } }];
        let mut shielded_state = ShieldedState::new();
        call_protocol::instructions::execute_protocol_instructions(&instructions, &mut balances, &registry, &compliance, &mut shielded_state, sender).unwrap();

        assert_eq!(balances.get_balance(asset_id, &sender), 4_000);
        assert_eq!(balances.get_balance(asset_id, &receiver), 1_000);
    }

    #[test]
    fn test_agent_batch_pay_multiple() {
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        let compliance = call_protocol::compliance::ComplianceEngine::new();
        let sender = addr(1);
        let asset_id = setup_asset(&mut balances, &mut registry, "BATCH", addr(10), sender, 10_000);

        let payments: Vec<AgentPayment> = (2..6).map(|i| AgentPayment { agent_id: 0, asset_id, to: addr(i), amount: 500 }).collect();
        let instructions = vec![Instruction::AgentBatchPay { payments }];
        let mut shielded_state = ShieldedState::new();
        call_protocol::instructions::execute_protocol_instructions(&instructions, &mut balances, &registry, &compliance, &mut shielded_state, sender).unwrap();

        assert_eq!(balances.get_balance(asset_id, &sender), 8_000);
        for i in 2..6 {
            assert_eq!(balances.get_balance(asset_id, &addr(i)), 500);
        }
    }

    #[test]
    fn test_agent_funding_action_grant() {
        let mut balances = AgentBalances::new();
        let owner = addr(1);
        let action = AgentFundingAction::Grant { agent_id: 0, asset_id: 1, amount: 3_000 };
        match action {
            AgentFundingAction::Grant { agent_id, asset_id, amount } => {
                balances.grant_funds(owner, agent_id, asset_id, amount);
            }
            _ => panic!("wrong variant"),
        }
        assert_eq!(balances.get_balance(owner, 0, 1), 3_000);
    }

    #[test]
    fn test_agent_funding_action_revoke() {
        let mut balances = AgentBalances::new();
        let owner = addr(1);
        balances.grant_funds(owner, 0, 1, 2_000);
        let action = AgentFundingAction::Revoke { agent_id: 0, asset_id: 1 };
        match action {
            AgentFundingAction::Revoke { agent_id, asset_id } => {
                let revoked = balances.revoke_funds(owner, agent_id, asset_id);
                assert_eq!(revoked, 2_000);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_agent_update_config() {
        let mut registry = AgentRegistry::new();
        let id = registry.register_agent(addr(1), [1u8; 64], "old".into(), "https://old.com".into(), [0u8; 32], None, 100).unwrap();
        registry.update_agent_config(id, Some("new".into()), Some("https://new.com".into()), Some([3u8; 32])).unwrap();
        let agent = registry.get_agent(id).unwrap();
        assert_eq!(agent.name, "new");
        assert_eq!(agent.url, "https://new.com");
    }

    #[test]
    fn test_agent_update_domain_proof() {
        let mut registry = AgentRegistry::new();
        let id = registry.register_agent(addr(1), [1u8; 64], "agent".into(), "https://agent.com".into(), [0u8; 32], None, 100).unwrap();
        assert!(!registry.get_agent(id).unwrap().domain_verified);
        let proof = DomainProof::DnsTxt { domain: "agent.com".into(), txt_value: "call-agent=verified".into() };
        registry.update_domain_proof(id, proof).unwrap();
        assert!(registry.get_agent(id).unwrap().domain_verified);
    }
}
