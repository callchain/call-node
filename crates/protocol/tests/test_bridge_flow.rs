//! Bridge flow integration tests

mod test_bridge_flow_impl {
    use call_primitives::{Address, AssetId};
    use call_protocol::AccountState;
    use call_protocol::registry::AssetRegistry;
    use call_bridge::{
        BridgeConfig, BridgeOp, BridgeStateManager, ExternalBridgeOp, ExternalChain,
        BridgeSignature, process_external_deposit, process_external_withdraw,
        verify_bridge_signatures, sign_bridge_event, bridge_event_hash,
        check_deposit_balance,
    };
    use alloy_primitives::B256;
    use call_crypto::{generate_keypair, secp256k1_sign, recover_secp256k1_signer};

    fn addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn setup_asset(
        account: &mut AccountState,
        registry: &mut AssetRegistry,
        symbol: &str,
        issuer: Address,
        holder: Address,
        amount: u128,
    ) -> AssetId {
        let asset_id = registry.register_asset(
            symbol.into(),
            symbol.into(),
            18,
            issuer,
            0,
            0,
            1_000_000_000 * 10u128.pow(18),
        ).unwrap();
        account.balances.set_balance(asset_id, holder, amount).unwrap();
        asset_id
    }

    fn generate_bridge_validators(count: usize) -> (Vec<[u8; 32]>, Vec<Address>) {
        let mut secrets = Vec::with_capacity(count);
        let mut addrs = Vec::with_capacity(count);
        for _ in 0..count {
            let (secret, _) = generate_keypair();
            let msg_hash = [0u8; 32];
            let sig = secp256k1_sign(&secret, &msg_hash);
            let addr = recover_secp256k1_signer(&msg_hash, &sig).unwrap();
            secrets.push(secret);
            addrs.push(addr);
        }
        (secrets, addrs)
    }

    fn build_external_deposit_with_sigs(
        secrets: &[[u8; 32]],
        indices: &[usize],
    ) -> ExternalBridgeOp {
        let source_tx_hash = B256::ZERO;
        let source_block_number: u64 = 100;
        let sender = vec![0u8; 32];
        let recipient = addr(1);
        let asset_id: AssetId = 1;
        let amount: u128 = 1000;
        let source_chain = ExternalChain::EthereumMainnet;

        let signatures = indices
            .iter()
            .map(|&vi| BridgeSignature {
                validator_index: vi as u32,
                signature: sign_bridge_event(
                    &secrets[vi],
                    &source_chain,
                    source_tx_hash,
                    source_block_number,
                    &sender,
                    recipient,
                    asset_id,
                    amount,
                ),
            })
            .collect();

        ExternalBridgeOp::Deposit {
            source_chain,
            source_tx_hash,
            source_block_number,
            sender,
            recipient,
            asset_id,
            amount,
            signatures,
        }
    }

    #[test]
    fn test_internal_bridge_deposit_balance_check() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let holder = addr(1);
        let asset_id = setup_asset(&mut account, &mut registry, "BRIDGE", addr(10), holder, 5_000);
        assert!(check_deposit_balance(&account, asset_id, holder, 5_000).is_ok());
        assert!(check_deposit_balance(&account, asset_id, holder, 5_001).is_err());
    }

    #[test]
    fn test_internal_bridge_limit_enforcement() {
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig {
            max_per_tx: 1_000,
            daily_limit_per_asset: 5_000,
            ..Default::default()
        };
        assert!(bridge_state.check_per_tx_limit(500, config.max_per_tx).is_ok());
        assert!(bridge_state.check_per_tx_limit(1_001, config.max_per_tx).is_err());
        assert!(bridge_state.check_and_update_daily_limit(1, 2_000, config.daily_limit_per_asset, 100, config.blocks_per_day).is_ok());
        assert!(bridge_state.check_and_update_daily_limit(1, 2_000, config.daily_limit_per_asset, 100, config.blocks_per_day).is_ok());
        assert!(bridge_state.check_and_update_daily_limit(1, 2_000, config.daily_limit_per_asset, 100, config.blocks_per_day).is_err());
    }

    #[test]
    fn test_internal_bridge_pause_unpause() {
        let mut bridge_state = BridgeStateManager::default();
        let asset_id: AssetId = 1;
        assert!(!bridge_state.is_paused(asset_id));
        bridge_state.pause_asset(asset_id);
        assert!(bridge_state.is_paused(asset_id));
        bridge_state.unpause_asset(asset_id);
        assert!(!bridge_state.is_paused(asset_id));
    }

    #[test]
    fn test_internal_bridge_pending_op_lifecycle() {
        let mut bridge_state = BridgeStateManager::default();
        let op = BridgeOp::DepositToEvm {
            asset_id: 1,
            from: addr(1).into_array().into(),
            to: addr(2).into_array().into(),
            amount: 1_000,
        };
        bridge_state.add_pending_op(op.clone(), 100);
        assert_eq!(bridge_state.pending_ops.len(), 1);
        bridge_state.clear_completed_ops();
        assert_eq!(bridge_state.pending_ops.len(), 0);
    }

    #[test]
    fn test_internal_bridge_deposit_withdrawal_recording() {
        let mut bridge_state = BridgeStateManager::default();
        bridge_state.record_deposit(1, 1_000);
        bridge_state.record_deposit(1, 500);
        assert_eq!(bridge_state.total_deposits.get(&1), Some(&1_500));
        bridge_state.record_withdrawal(1, 300);
        assert_eq!(bridge_state.total_withdrawals.get(&1), Some(&300));
        bridge_state.check_and_update_daily_limit(1, 200, 10_000, 100, 10).unwrap();
        assert_eq!(bridge_state.daily_usage.get(&1), Some(&200));
        bridge_state.reset_daily_usage();
        assert!(bridge_state.daily_usage.is_empty());
    }

    #[test]
    fn test_external_bridge_valid_signatures_14() {
        let (secrets, validators) = generate_bridge_validators(21);
        let op = build_external_deposit_with_sigs(&secrets, &(0..14).collect::<Vec<_>>());
        assert!(verify_bridge_signatures(&op, &validators, 14).is_ok());
    }

    #[test]
    fn test_external_bridge_insufficient_signatures() {
        let (secrets, validators) = generate_bridge_validators(21);
        let op = build_external_deposit_with_sigs(&secrets, &[0, 1, 2, 3, 4]);
        let result = verify_bridge_signatures(&op, &validators, 14);
        assert!(matches!(result, Err(call_bridge::BridgeError::InsufficientSignatures(5, 14))));
    }

    #[test]
    fn test_external_bridge_duplicate_validator_rejected() {
        let (secrets, validators) = generate_bridge_validators(21);
        let source_tx_hash = B256::ZERO;
        let source_block_number: u64 = 100;
        let sender = vec![0u8; 32];
        let recipient = addr(1);
        let asset_id: AssetId = 1;
        let amount: u128 = 1000;
        let source_chain = ExternalChain::EthereumMainnet;

        let mut sigs: Vec<BridgeSignature> = (0..14)
            .map(|i| BridgeSignature {
                validator_index: i as u32,
                signature: sign_bridge_event(&secrets[i], &source_chain, source_tx_hash, source_block_number, &sender, recipient, asset_id, amount),
            })
            .collect();
        sigs.push(BridgeSignature {
            validator_index: 0,
            signature: sign_bridge_event(&secrets[0], &source_chain, source_tx_hash, source_block_number, &sender, recipient, asset_id, amount),
        });

        let op = ExternalBridgeOp::Deposit {
            source_chain, source_tx_hash, source_block_number, sender, recipient, asset_id, amount, signatures: sigs,
        };
        let result = verify_bridge_signatures(&op, &validators, 14);
        assert!(matches!(result, Err(call_bridge::BridgeError::InvalidSignature(14, _))));
    }

    #[test]
    fn test_external_bridge_process_deposit_end_to_end() {
        let mut account = AccountState::new();
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig { allowed_assets: vec![1], ..Default::default() };
        let (secrets, validators) = generate_bridge_validators(21);

        let op = build_external_deposit_with_sigs(&secrets, &(0..14).collect::<Vec<_>>());
        let result = process_external_deposit(&op, &mut account, &mut bridge_state, &config, &validators, 100, None).unwrap();
        assert!(matches!(result, call_bridge::ExternalDepositResult::Queued { .. }));

        // Deposit is queued, not credited yet (challenge period)
        if let ExternalBridgeOp::Deposit { recipient, .. } = &op {
            assert_eq!(account.get_balance(1, recipient), 0);
        }

        // Finalize after challenge period
        let finalized = call_bridge::finalize_pending_external_deposits_with_period(
            &mut bridge_state, &mut account, 100 + config.challenge_period_blocks, config.challenge_period_blocks,
        );
        assert_eq!(finalized, 1);

        if let ExternalBridgeOp::Deposit { recipient, amount, .. } = op {
            assert_eq!(account.get_balance(1, &recipient), amount);
        }

        // Replay protection
        let result = process_external_deposit(&op, &mut account, &mut bridge_state, &config, &validators, 200, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_external_bridge_asset_not_allowed() {
        let mut account = AccountState::new();
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig { allowed_assets: vec![1], ..Default::default() };
        let (_, validators) = generate_bridge_validators(21);

        let op = ExternalBridgeOp::Deposit {
            source_chain: ExternalChain::EthereumMainnet, source_tx_hash: B256::ZERO,
            source_block_number: 100, sender: vec![0u8; 32], recipient: addr(1),
            asset_id: 99, amount: 1000, signatures: vec![],
        };
        assert!(matches!(
            process_external_deposit(&op, &mut account, &mut bridge_state, &config, &validators, 100, None),
            Err(call_bridge::BridgeError::ExternalAssetNotAllowed(99))
        ));
    }

    #[test]
    fn test_external_bridge_process_withdraw_end_to_end() {
        let mut account = AccountState::new();
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig::default();
        let sender = addr(1);
        account.balances.set_balance(1, sender, 5_000).unwrap();

        let op = ExternalBridgeOp::Withdraw {
            target_chain: ExternalChain::EthereumMainnet, target_address: vec![0u8; 32],
            asset_id: 1, sender, amount: 2_000,
        };
        process_external_withdraw(&op, &mut account, &mut bridge_state, &config, 100).unwrap();
        assert_eq!(account.get_balance(1, &sender), 3_000);
        assert_eq!(bridge_state.total_withdrawals.get(&1), Some(&2_000));
    }

    #[test]
    fn test_external_bridge_withdraw_insufficient_balance() {
        let mut account = AccountState::new();
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig::default();
        let sender = addr(1);
        account.balances.set_balance(1, sender, 100).unwrap();

        let op = ExternalBridgeOp::Withdraw {
            target_chain: ExternalChain::EthereumMainnet, target_address: vec![0u8; 32],
            asset_id: 1, sender, amount: 500,
        };
        assert!(matches!(
            process_external_withdraw(&op, &mut account, &mut bridge_state, &config, 100),
            Err(call_bridge::BridgeError::InsufficientProtocolBalance(1, 500))
        ));
    }

    #[test]
    fn test_external_bridge_chain_ids() {
        assert_eq!(ExternalChain::EthereumMainnet.chain_id(), 1);
        assert_eq!(ExternalChain::Arbitrum.chain_id(), 42161);
    }

    #[test]
    fn test_external_bridge_event_hash_deterministic() {
        let hash1 = bridge_event_hash(&ExternalChain::EthereumMainnet, B256::ZERO, 100, &[1, 2, 3], addr(1), 1, 500);
        let hash2 = bridge_event_hash(&ExternalChain::EthereumMainnet, B256::ZERO, 100, &[1, 2, 3], addr(1), 1, 500);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_external_bridge_daily_limit_on_deposits() {
        let mut account = AccountState::new();
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig { allowed_assets: vec![1], daily_limit_per_asset: 1_500, ..Default::default() };
        let (secrets, validators) = generate_bridge_validators(21);

        let op1 = build_external_deposit_with_sigs(&secrets, &(0..14).collect::<Vec<_>>());
        let result1 = process_external_deposit(&op1, &mut account, &mut bridge_state, &config, &validators, 100, None);
        assert!(result1.is_ok());

        // Second deposit would exceed daily limit
        let op2 = ExternalBridgeOp::Deposit {
            source_chain: ExternalChain::EthereumMainnet,
            source_tx_hash: B256::from_slice(&[1u8; 32]),
            source_block_number: 101, sender: vec![0u8; 32], recipient: addr(2),
            asset_id: 1, amount: 600,
            signatures: (0..14).map(|i| BridgeSignature {
                validator_index: i as u32,
                signature: sign_bridge_event(&secrets[i], &ExternalChain::EthereumMainnet, B256::from_slice(&[1u8; 32]), 101, &[0u8; 32], addr(2), 1, 600),
            }).collect(),
        };
        let result = process_external_deposit(&op2, &mut account, &mut bridge_state, &config, &validators, 100, None);
        assert!(result.is_err());
    }
}
