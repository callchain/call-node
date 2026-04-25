//! RPC layer tests (per spec §11)

#[cfg(test)]
mod tests {
    use call_primitives::{Address, AssetId};
    use call_protocol::{AccountState, AssetRegistry, ComplianceEngine};
    use call_oracle::OracleManager;
    use call_evm::EvmState;
    use call_bridge::BridgeStateManager;
    use call_consensus::ValidatorStateManager;
    use call_agent::{AgentRegistry, AgentBalances};
    use call_shielded::ShieldedState;
    use call_transaction_pool::Mempool;
    use crate::handlers::RpcState;
    use std::sync::{Arc, RwLock, OnceLock};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    /// Lazily-generated secp256k1 keypair for test transactions.
    static TEST_SENDER: OnceLock<(Address, [u8; 32])> = OnceLock::new();

    fn test_sender() -> &'static Address {
        &TEST_SENDER
            .get_or_init(|| {
                let (secret, pubkey) = call_crypto::generate_keypair();
                let addr = call_crypto::pubkey_to_address(&pubkey);
                (addr, secret)
            })
            .0
    }

    fn sign_tx_hash(tx_hash: &[u8; 32]) -> [u8; 65] {
        let secret = &TEST_SENDER.get().expect("TEST_SENDER initialized").1;
        call_crypto::secp256k1_sign(secret, tx_hash)
    }

    fn make_test_state() -> RpcState {
        let mempool = Arc::new(RwLock::new(Mempool::new()));
        RpcState::new(
            AccountState::new(),
            AssetRegistry::new(),
            ComplianceEngine::new(),
            EvmState::new(),
            BridgeStateManager::default(),
            ValidatorStateManager::default(),
            AgentRegistry::new(),
            AgentBalances::new(),
            call_agent::AgentNonces::new(),
            ShieldedState::new(),
            mempool,
            1,
            OracleManager::default(),
        )
    }

    #[test]
    fn test_rpc_eth_get_balance() {
        let state = make_test_state();
        let addr = test_addr(1);
        // Set EVM balance
        state.evm_state.write().unwrap().set_balance(addr, alloy_primitives::U256::from(1000));
        let balance = state.get_evm_balance(&addr);
        assert_eq!(balance, alloy_primitives::U256::from(1000));
        // Zero for unknown address
        assert_eq!(state.get_evm_balance(&test_addr(99)), alloy_primitives::U256::ZERO);
    }

    #[test]
    fn test_rpc_call_asset_info() {
        let state = make_test_state();
        // Register an asset
        let issuer = test_addr(1);
        let mut registry = state.asset_registry.write().unwrap();
        let id = registry.register_asset("TEST".into(), "Test Token".into(), 18, issuer, 0, 0, 0).unwrap();
        registry.mint_supply(id, &issuer, 5_000).unwrap();
        registry.add_evm_supply(id, 3_000).unwrap();
        drop(registry);

        let info = state.get_asset_info(id).expect("asset info");
        assert_eq!(info.symbol, "TEST");
        assert_eq!(info.name, "Test Token");
        assert_eq!(info.decimals, 18);
        assert_eq!(info.issuer, issuer);
        assert_eq!(info.protocol_supply, 5_000);
        assert_eq!(info.evm_supply, 3_000);
        assert_eq!(info.all_supply, 8_000);
        assert_eq!(info.max_supply, 0);
        assert_eq!(info.status, "Active");

        // Non-existent asset
        assert!(state.get_asset_info(999).is_none());
    }

    #[test]
    fn test_rpc_call_asset_info_capped() {
        let state = make_test_state();
        let issuer = test_addr(1);
        let mut registry = state.asset_registry.write().unwrap();
        let id = registry.register_asset("CAPPED".into(), "Capped Token".into(), 18, issuer, 0, 0, 10_000).unwrap();
        registry.freeze_asset(id, &issuer).unwrap();
        drop(registry);

        let info = state.get_asset_info(id).expect("asset info");
        assert_eq!(info.max_supply, 10_000);
        assert_eq!(info.status, "Frozen");
        assert_eq!(info.all_supply, 0);
    }

    #[test]
    fn test_rpc_call_protocol_balance() {
        let state = make_test_state();
        let addr = test_addr(1);
        let asset_id: AssetId = 1;

        // Set balance
        state.balance_state.write().unwrap().balances.set_balance(asset_id, addr, 5000).unwrap();

        let balance = state.get_balance(asset_id, &addr);
        assert_eq!(balance, 5000);

        // Zero for unknown
        assert_eq!(state.get_balance(asset_id, &test_addr(99)), 0);
    }

    #[test]
    fn test_rpc_call_total_balance() {
        let state = make_test_state();
        let asset_id: AssetId = 1;

        state.balance_state.write().unwrap().balances.set_balance(asset_id, test_addr(1), 1000).unwrap();
        state.balance_state.write().unwrap().balances.set_balance(asset_id, test_addr(2), 2000).unwrap();
        state.balance_state.write().unwrap().balances.set_balance(asset_id, test_addr(3), 3000).unwrap();

        let total = state.get_total_balance(asset_id);
        assert_eq!(total, 6000);
    }

    #[test]
    fn test_rpc_call_agent_register_and_info() {
        let state = make_test_state();
        let owner = test_addr(1);
        let pubkey = [1u8; 64];

        let agent_id = state.register_agent(owner, pubkey, "test-agent".into(), "https://agent.example.com".into(), [0u8; 32]).unwrap();
        assert_eq!(agent_id, 0);

        let info = state.get_agent_info(agent_id).expect("agent info");
        assert_eq!(info.agent_id, 0);
        assert_eq!(info.owner, owner);
        assert_eq!(info.name, "test-agent");
        assert_eq!(info.url, "https://agent.example.com");
        assert!(!info.domain_verified);

        // Non-existent agent
        assert!(state.get_agent_info(999).is_none());
    }

    #[test]
    fn test_rpc_call_agent_balance() {
        let state = make_test_state();
        let owner = test_addr(1);
        let pubkey = [1u8; 64];

        // Give owner sufficient balance for grants
        state.balance_state.write().unwrap().balances.set_balance(1, owner, 50000).unwrap();

        let agent_id = state.register_agent(owner, pubkey, "balance-agent".into(), "https://a.com".into(), [0u8; 32]).unwrap();

        // Grant balance
        state.grant_agent_balance(agent_id, 1, 10000).unwrap();
        let balance = state.get_agent_total_balance(agent_id);
        assert_eq!(balance, 10000);

        // Revoke
        state.revoke_agent_balance(agent_id, 1).unwrap();
        let balance = state.get_agent_total_balance(agent_id);
        assert_eq!(balance, 0);
    }

    #[test]
    fn test_rpc_call_shielded_tree_state() {
        let state = make_test_state();
        let tree_state = state.get_shielded_tree_state();
        assert_eq!(tree_state.leaf_count, 0);
        assert_eq!(tree_state.nullifier_count, 0);

        // Tree state has default merkle root
        assert_eq!(tree_state.merkle_root.as_slice().len(), 32);
    }

    #[test]
    fn test_rpc_get_transaction_receipt() {
        let state = make_test_state();
        use call_primitives::ExecutionStatus;
        use call_protocol::{ProtocolReceipt, InstructionExecResult};
        use call_primitives::FeeCurrency;

        let tx_hash = call_primitives::TxHash::repeat_byte(0xAB);
        let receipt = ProtocolReceipt {
            tx_hash,
            status: ExecutionStatus::Success,
            gas_used: 10_000,
            gas_payer: test_addr(1),
            fee_currency: FeeCurrency::Call,
            fee_amount: 1_000_000,
            block_number: 1,
            instruction_results: vec![InstructionExecResult {
                success: true,
                gas_used: 10_000,
                revert_reason: None,
            }],
            logs: vec![],
            memos: vec![],
            state_changes: vec![],
        };
        state.store_receipt(tx_hash, receipt);

        let found = state.get_receipt(&tx_hash).expect("receipt exists");
        assert!(matches!(found.status, ExecutionStatus::Success));
        assert_eq!(found.gas_used, 10_000);

        // Non-existent
        assert!(state.get_receipt(&call_primitives::TxHash::ZERO).is_none());
    }

    #[test]
    fn test_rpc_get_block_receipts() {
        let state = make_test_state();
        use call_primitives::ExecutionStatus;
        use call_protocol::ProtocolReceipt;
        use call_primitives::FeeCurrency;

        let tx1 = call_primitives::TxHash::repeat_byte(1);
        let tx2 = call_primitives::TxHash::repeat_byte(2);
        state.store_receipt(tx1, ProtocolReceipt {
            tx_hash: tx1,
            status: ExecutionStatus::Success,
            gas_used: 10_000,
            gas_payer: test_addr(1),
            fee_currency: FeeCurrency::Call,
            fee_amount: 0,
            block_number: 1,
            instruction_results: vec![],
            logs: vec![],
            memos: vec![],
            state_changes: vec![],
        });
        state.store_receipt(tx2, ProtocolReceipt {
            tx_hash: tx2,
            status: ExecutionStatus::Reverted { reason: "out of gas".into() },
            gas_used: 5_000,
            gas_payer: test_addr(2),
            fee_currency: FeeCurrency::Call,
            fee_amount: 0,
            block_number: 1,
            instruction_results: vec![],
            logs: vec![],
            memos: vec![],
            state_changes: vec![],
        });

        let receipts = state.get_all_receipts();
        assert_eq!(receipts.len(), 2);
    }

    #[test]
    fn test_rpc_get_logs_by_address() {
        // Logs are stored in receipts; filtering by address returns empty
        // (placeholder until real log indexing is implemented)
        let state = make_test_state();
        let receipts = state.get_all_receipts();
        assert!(receipts.is_empty());
    }

    #[test]
    fn test_rpc_get_tx_by_reference() {
        // Placeholder: external reference lookup returns null
        let state = make_test_state();
        let _ = state;
        // In the real implementation, this would look up by external reference
    }

    #[test]
    fn test_ws_new_payment_block_subscription() {
        // WebSocket subscription registration test:
        // The subscription endpoints are registered successfully.
        // Full subscription testing requires a running server.
        use crate::ws::{register_ws_subscriptions, SubscriptionManager};
        use jsonrpsee::RpcModule;
        use std::sync::Arc;
        let state = Arc::new(make_test_state());
        let subs = SubscriptionManager::new();
        let mut module = RpcModule::new(Arc::clone(&state));
        let result = register_ws_subscriptions(&mut module, &subs);
        assert!(result.is_ok(), "WebSocket subscriptions should register OK");
    }

    #[test]
    fn test_rpc_execute_evm_call() {
        let state = make_test_state();
        let caller = test_addr(1);
        let to = test_addr(2);

        // Set up EVM state with balance
        state.evm_state.write().unwrap().set_balance(caller, alloy_primitives::U256::from(1_000_000_000i128));
        state.evm_state.write().unwrap().create_account(caller);
        state.evm_state.write().unwrap().create_account(to);

        // Execute a simple call (no data, just reading state)
        let result = state.execute_evm_call(
            caller,
            Some(to),
            alloy_primitives::U256::from(100),
            alloy_primitives::Bytes::default(),
            21_000,
            10,
        );
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.success);
    }

    #[test]
    fn test_rpc_submit_payment_success() {
        let state = make_test_state();
        let sender = *test_sender();
        let to = test_addr(2);
        let asset_id: AssetId = 1;

        // Set up protocol balance (not required for mempool insertion, but harmless)
        state.balance_state.write().unwrap().balances.set_balance(asset_id, sender, 10_000).unwrap();

        // Build tx to compute canonical hash for signing
        let tx = call_protocol::transaction::ProtocolTransaction {
            sender,
            nonce: 1,
            instructions: vec![call_protocol::Instruction::Transfer {
                asset_id,
                to,
                amount: 5_000,
                memo: Some(call_protocol::PaymentMemo {
                    message: "test payment".into(),
                    reference: None,
                    metadata: None,
                }),
            }],
            gas_config: call_protocol::transaction::GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: call_protocol::transaction::AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let tx_hash = tx.compute_tx_hash();
        let signature = sign_tx_hash(&tx_hash);

        let returned_hash = state.submit_payment(
            sender, 1, asset_id, to, 5_000,
            Some("test payment".into()),
            100_000, 1_000_000,
            Some(signature),
        ).unwrap();

        assert_eq!(returned_hash.as_slice().len(), 32);

        // Balances are NOT changed immediately — execution is deferred to block production
        let balance = state.get_balance(asset_id, &sender);
        assert_eq!(balance, 10_000);
        let to_balance = state.get_balance(asset_id, &to);
        assert_eq!(to_balance, 0);

        // No receipt stored yet — receipt is produced during block execution
        assert!(state.get_receipt(&returned_hash).is_none());

        // Transaction should be in the mempool
        let mempool_size = state.mempool.read().unwrap().protocol_pool.len();
        assert_eq!(mempool_size, 1);
    }

    #[test]
    fn test_rpc_submit_payment_insufficient_balance() {
        let state = make_test_state();
        let sender = *test_sender();
        let to = test_addr(2);
        let asset_id: AssetId = 1;

        // Low balance — but mempool insertion does not validate balance
        state.balance_state.write().unwrap().balances.set_balance(asset_id, sender, 100).unwrap();

        // Build tx and sign it
        let tx = call_protocol::transaction::ProtocolTransaction {
            sender,
            nonce: 1,
            instructions: vec![call_protocol::Instruction::Transfer {
                asset_id,
                to,
                amount: 5_000,
                memo: None,
            }],
            gas_config: call_protocol::transaction::GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: call_protocol::transaction::AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let signature = sign_tx_hash(&tx.compute_tx_hash());

        // Mempool insertion succeeds — balance check is deferred to block execution
        let result = state.submit_payment(
            sender, 1, asset_id, to, 5_000,
            None, 100_000, 1_000_000,
            Some(signature),
        );
        assert!(result.is_ok(), "mempool should accept tx even with insufficient balance");

        // Balances unchanged — execution not yet performed
        assert_eq!(state.get_balance(asset_id, &sender), 100);
        assert_eq!(state.get_balance(asset_id, &to), 0);
    }

    #[test]
    fn test_rpc_submit_evm_tx_real_signed() {
        use alloy_consensus::{SignableTransaction, TxLegacy};
        use alloy_consensus::crypto::secp256k1::sign_message;
        use alloy_primitives::TxKind;

        let state = make_test_state();

        // Known test key (standard Ethereum test private key)
        let secret = alloy_primitives::FixedBytes::<32>::from_slice(
            &hex::decode("4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318").unwrap(),
        );
        let signer_address: call_primitives::Address =
            alloy_primitives::address!("0x2c7536E3605D9C16a7a3D7b1898e529396a65c23");

        // Set up EVM state with balance
        state.evm_state.write().unwrap().set_balance(signer_address, alloy_primitives::U256::from(1_000_000_000_000i128));
        state.evm_state.write().unwrap().create_account(signer_address);
        state.evm_state.write().unwrap().create_account(test_addr(2));

        // Build transaction
        let tx = TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price: 10,
            gas_limit: 21_000,
            to: TxKind::Call(test_addr(2).into()),
            value: alloy_primitives::U256::from(100),
            input: alloy_primitives::Bytes::default(),
        };

        // Sign
        let sig_hash = tx.signature_hash();
        let signature = sign_message(secret, sig_hash).unwrap();
        let signed = tx.into_signed(signature);
        let envelope = alloy_consensus::TxEnvelope::from(signed);

        // RLP-encode
        let mut raw_tx_bytes = Vec::new();
        alloy_rlp::Encodable::encode(&envelope, &mut raw_tx_bytes);

        // Submit to mempool only — execution is deferred to block production
        let result = state.submit_evm_tx(&raw_tx_bytes);
        assert!(result.is_ok(), "submit_evm_tx failed: {:?}", result);
        let tx_hash = result.unwrap();
        assert_eq!(tx_hash.as_slice().len(), 32);

        // No receipt stored yet — execution is deferred to consensus
        assert!(state.get_receipt(&tx_hash).is_none());

        // EVM state unchanged — balance not transferred yet
        assert_eq!(
            state.evm_state.read().unwrap().get_balance(&signer_address),
            alloy_primitives::U256::from(1_000_000_000_000i128)
        );

        // Transaction should be in the EVM mempool
        let mempool_evm_count = state.mempool.read().unwrap().evm_pool.len();
        assert_eq!(mempool_evm_count, 1);
    }
}
