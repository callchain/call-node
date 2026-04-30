//! RPC layer tests (per spec §11)

#[cfg(test)]
mod tests {
    use call_primitives::{Address, AssetId};
    use call_protocol::{AccountState, AssetRegistry, ComplianceEngine};
    use call_oracle::OracleManager;
    use call_evm::EvmState;
    use call_bridge::BridgeStateManager;
    use call_consensus::ValidatorStateManager;
    use call_consensus::exec::evm_instructions;
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
        let issuer = test_addr(1);
        {
            let mut evm = state.evm_state.write().unwrap();
            evm_instructions::seed_asset(&mut *evm, 1, "TEST", "Test Token", 18, issuer, 0, 8_000, 0,
            );
        }

        let info = state.get_asset_info(1).expect("asset info");
        assert_eq!(info.symbol, "TEST");
        assert_eq!(info.name, "Test Token");
        assert_eq!(info.decimals, 18);
        assert_eq!(info.issuer, issuer);
        assert_eq!(info.protocol_supply, 8_000);
        assert_eq!(info.evm_supply, 8_000);
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
        {
            let mut evm = state.evm_state.write().unwrap();
            evm_instructions::seed_asset(
                &mut *evm, 1, "CAPPED", "Capped Token", 18, issuer, 10_000, 0, 1,
            );
        }

        let info = state.get_asset_info(1).expect("asset info");
        assert_eq!(info.max_supply, 10_000);
        assert_eq!(info.status, "Frozen");
        assert_eq!(info.all_supply, 0);
    }

    #[test]
    fn test_rpc_call_protocol_balance() {
        let state = make_test_state();
        let addr = test_addr(1);
        let asset_id: AssetId = 1;

        // Set balance in EVM storage
        {
            let mut evm = state.evm_state.write().unwrap();
            evm_instructions::seed_balance(&mut *evm, asset_id, addr, 5000);
        }

        let balance = state.get_balance(asset_id, &addr);
        assert_eq!(balance, 5000);

        // Zero for unknown
        assert_eq!(state.get_balance(asset_id, &test_addr(99)), 0);
    }

    #[test]
    fn test_rpc_call_total_balance() {
        let state = make_test_state();
        let asset_id: AssetId = 1;

        {
            let mut evm = state.evm_state.write().unwrap();
            evm_instructions::seed_asset(
                &mut *evm, asset_id, "CALL", "Call Token", 18, Address::ZERO, 0, 6_000, 0,
            );
        }

        let total = state.get_total_balance(asset_id);
        assert_eq!(total, 6000);
    }

    #[test]
    fn test_rpc_call_agent_register_and_info() {
        let state = make_test_state();
        let owner = test_addr(1);

        {
            let mut evm = state.evm_state.write().unwrap();
            evm_instructions::seed_agent(
                &mut *evm, 0, owner, "test-agent", "https://agent.example.com", 0,
            );
        }

        let info = state.get_agent_info(0).expect("agent info");
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

        {
            let mut evm = state.evm_state.write().unwrap();
            evm_instructions::seed_balance(&mut *evm, 1, owner, 50_000,
            );
            evm_instructions::seed_agent(
                &mut *evm, 0, owner, "balance-agent", "https://a.com", 0,
            );
            evm_instructions::agent_set_balance(&mut *evm, 0, 1, 10_000,
            );
        }

        let balance = state.get_agent_total_balance(0);
        assert_eq!(balance, 10000);

        // Revoke
        {
            let mut evm = state.evm_state.write().unwrap();
            evm_instructions::agent_set_balance(&mut *evm, 0, 1, 0,
            );
        }
        let balance = state.get_agent_total_balance(0);
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
            block_hash: call_primitives::Hash::ZERO,
            transaction_index: 0,
            to: None,
            contract_address: None,
            cumulative_gas_used: 10_000,
            effective_gas_price: 100,
            logs_bloom: vec![],
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
    fn test_rpc_get_transaction_receipt_reverted() {
        let state = make_test_state();
        use call_primitives::ExecutionStatus;
        use call_protocol::{ProtocolReceipt, InstructionExecResult};
        use call_primitives::FeeCurrency;

        let tx_hash = call_primitives::TxHash::repeat_byte(0xCD);
        let receipt = ProtocolReceipt {
            tx_hash,
            status: ExecutionStatus::Reverted {
                reason: "insufficient balance for asset 1: have 0, need 1000".into(),
            },
            gas_used: 21_000,
            gas_payer: test_addr(2),
            fee_currency: FeeCurrency::Call,
            fee_amount: 500_000,
            block_number: 42,
            block_hash: call_primitives::Hash::ZERO,
            transaction_index: 0,
            to: None,
            contract_address: None,
            cumulative_gas_used: 21_000,
            effective_gas_price: 100,
            logs_bloom: vec![],
            instruction_results: vec![InstructionExecResult {
                success: false,
                gas_used: 21_000,
                revert_reason: Some("insufficient balance".into()),
            }],
            logs: vec![],
            memos: vec![],
            state_changes: vec![],
        };
        state.store_receipt(tx_hash, receipt);

        let found = state.get_receipt(&tx_hash).expect("receipt exists");
        match &found.status {
            ExecutionStatus::Reverted { reason } => {
                assert!(reason.contains("insufficient balance"), "expected revert reason, got: {}", reason);
            }
            other => panic!("expected Reverted, got {:?}", other),
        }
        assert_eq!(found.gas_used, 21_000);
        assert_eq!(found.block_number, 42);

        // Verify receipt_to_json exposes revertReason
        let json = crate::standard::receipt_to_json(&found);
        assert_eq!(json["status"], "0x0", "reverted status should be 0x0");
        assert!(
            json["revertReason"].as_str().unwrap().contains("insufficient balance"),
            "revertReason should be present in JSON"
        );
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
            block_hash: call_primitives::Hash::ZERO,
            transaction_index: 0,
            to: None,
            contract_address: None,
            cumulative_gas_used: 10_000,
            effective_gas_price: 0,
            logs_bloom: vec![],
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
            block_hash: call_primitives::Hash::ZERO,
            transaction_index: 1,
            to: None,
            contract_address: None,
            cumulative_gas_used: 15_000,
            effective_gas_price: 0,
            logs_bloom: vec![],
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
    fn test_rpc_submit_payment_rejected_evm_only_mempool() {
        let state = make_test_state();
        let sender = *test_sender();
        let to = test_addr(2);
        let asset_id: AssetId = 1;

        // Set up balance in EVM storage
        {
            let mut evm = state.evm_state.write().unwrap();
            evm_instructions::seed_balance(&mut *evm, asset_id, sender, 10_000);
        }

        // Build tx and sign it
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
            max_priority_fee: 1,
            expires_at: 0,
            auth: call_protocol::transaction::AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let signature = sign_tx_hash(&tx.compute_tx_hash());

        // Protocol transactions are rejected in EVM-only mempool mode
        let result = state.submit_payment(
            sender, 1, asset_id, to, 5_000,
            Some("test payment".into()),
            100_000, 1_000_000,
            Some(signature),
        );
        assert!(result.is_err(), "protocol tx should be rejected in EVM-only mempool");
        assert!(result.unwrap_err().contains("eth_sendRawTransaction"));

        // Balances unchanged
        assert_eq!(state.get_balance(asset_id, &sender), 10_000);
        assert_eq!(state.get_balance(asset_id, &to), 0);

        // Mempool should be empty (EVM-only)
        let mempool_size = state.mempool.read().unwrap().evm_pool.len();
        assert_eq!(mempool_size, 0);
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
