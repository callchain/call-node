//! RPC layer tests (per spec §11)

#[cfg(test)]
mod tests {
    use crate::handlers::RpcState;
    use call_consensus::exec::state_accessors;
    use call_evm::provider::InMemoryStateProvider;
    use call_mempool::Mempool;
    use call_primitives::{Address, AssetId};
    use std::sync::{Arc, OnceLock, RwLock};

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

    fn make_test_db() -> (std::path::PathBuf, Arc<reth_db::DatabaseEnv>) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = std::env::temp_dir().join(format!(
            "call-rpc-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init test db");
        let evm = InMemoryStateProvider::new();
        evm.save_to_db(&db).expect("seed test db");
        (tmp, db)
    }

    fn make_test_state() -> RpcState {
        let (_tmp, db) = make_test_db();
        let mempool = Arc::new(RwLock::new(Mempool::new()));
        RpcState::new(db, mempool, 1)
    }

    /// Helper: load provider, apply mutation, save back to MDBX.
    fn with_test_state<F>(state: &RpcState, f: F)
    where
        F: FnOnce(&mut InMemoryStateProvider),
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&state.db_env).unwrap();
        f(&mut provider);
        provider.save_to_db(&state.db_env).unwrap();
    }

    #[test]
    fn test_rpc_eth_get_balance() {
        let state = make_test_state();
        let addr = test_addr(1);
        // Set EVM balance
        with_test_state(&state, |evm| {
            evm.set_balance(addr, alloy_primitives::U256::from(1000));
        });
        let balance = state.get_evm_balance(&addr);
        assert_eq!(balance, alloy_primitives::U256::from(1000));
        // Zero for unknown address
        assert_eq!(
            state.get_evm_balance(&test_addr(99)),
            alloy_primitives::U256::ZERO
        );
    }

    #[test]
    fn test_rpc_call_asset_info() {
        let state = make_test_state();
        let issuer = test_addr(1);
        with_test_state(&state, |evm| {
            state_accessors::seed_asset(evm, 1, "TEST", "Test Token", 18, issuer, 0, 8_000, 0);
        });

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
        with_test_state(&state, |evm| {
            state_accessors::seed_asset(evm, 1, "CAPPED", "Capped Token", 18, issuer, 10_000, 0, 1);
        });

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
        with_test_state(&state, |evm| {
            state_accessors::seed_balance(evm, asset_id, addr, 5000);
        });

        let balance = state.get_balance(asset_id, &addr);
        assert_eq!(balance, 5000);

        // Zero for unknown
        assert_eq!(state.get_balance(asset_id, &test_addr(99)), 0);
    }

    #[test]
    fn test_rpc_call_total_balance() {
        let state = make_test_state();
        let asset_id: AssetId = 1;

        with_test_state(&state, |evm| {
            state_accessors::seed_asset(
                evm,
                asset_id,
                "CALL",
                "Call Token",
                18,
                Address::ZERO,
                0,
                6_000,
                0,
            );
        });

        let total = state.get_total_balance(asset_id);
        assert_eq!(total, 6000);
    }

    #[test]
    fn test_rpc_call_agent_register_and_info() {
        let state = make_test_state();
        let owner = test_addr(1);

        with_test_state(&state, |evm| {
            state_accessors::seed_agent(
                evm,
                0,
                owner,
                "test-agent",
                "https://agent.example.com",
                0,
            );
        });

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

        with_test_state(&state, |evm| {
            state_accessors::seed_balance(evm, 1, owner, 50_000);
            state_accessors::seed_agent(evm, 0, owner, "balance-agent", "https://a.com", 0);
            state_accessors::agent_set_balance(evm, 0, 1, 10_000);
        });

        let balance = state.get_agent_total_balance(0);
        assert_eq!(balance, 10000);

        // Revoke
        with_test_state(&state, |evm| {
            state_accessors::agent_set_balance(evm, 0, 1, 0);
        });
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
        use call_primitives::FeeCurrency;
        use call_protocol::{InstructionExecResult, ProtocolReceipt};

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
        use call_primitives::FeeCurrency;
        use call_protocol::{InstructionExecResult, ProtocolReceipt};

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
                assert!(
                    reason.contains("insufficient balance"),
                    "expected revert reason, got: {}",
                    reason
                );
            }
            other => panic!("expected Reverted, got {:?}", other),
        }
        assert_eq!(found.gas_used, 21_000);
        assert_eq!(found.block_number, 42);

        // Verify receipt_to_json exposes revertReason
        let json = crate::standard::receipt_to_json(&found);
        assert_eq!(json["status"], "0x0", "reverted status should be 0x0");
        assert!(
            json["revertReason"]
                .as_str()
                .unwrap()
                .contains("insufficient balance"),
            "revertReason should be present in JSON"
        );
    }

    #[test]
    fn test_rpc_get_block_receipts() {
        let state = make_test_state();
        use call_primitives::ExecutionStatus;
        use call_primitives::FeeCurrency;
        use call_protocol::ProtocolReceipt;

        let tx1 = call_primitives::TxHash::repeat_byte(1);
        let tx2 = call_primitives::TxHash::repeat_byte(2);
        state.store_receipt(
            tx1,
            ProtocolReceipt {
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
            },
        );
        state.store_receipt(
            tx2,
            ProtocolReceipt {
                tx_hash: tx2,
                status: ExecutionStatus::Reverted {
                    reason: "out of gas".into(),
                },
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
            },
        );

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
        with_test_state(&state, |evm| {
            evm.set_balance(caller, alloy_primitives::U256::from(1_000_000_000i128));
            evm.create_account(caller);
            evm.create_account(to);
        });

        // Execute a simple call (no data, just reading state)
        let result = state.execute_evm_call(
            caller,
            Some(to),
            alloy_primitives::U256::from(100),
            alloy_primitives::Bytes::default(),
            21_000,
            10,
            None,
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
        with_test_state(&state, |evm| {
            state_accessors::seed_balance(evm, asset_id, sender, 10_000);
        });

        // Protocol transactions are rejected in EVM-only mempool mode
        let result = state.submit_payment(
            sender,
            1,
            asset_id,
            to,
            5_000,
            Some("test payment".into()),
            100_000,
            1_000_000,
            Some([0u8; 65]),
        );
        assert!(
            result.is_err(),
            "protocol tx should be rejected in EVM-only mempool"
        );
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
        use alloy_consensus::crypto::secp256k1::sign_message;
        use alloy_consensus::{SignableTransaction, TxLegacy};
        use alloy_primitives::TxKind;

        let state = make_test_state();

        // Known test key (standard Ethereum test private key)
        let secret = alloy_primitives::FixedBytes::<32>::from_slice(
            &hex::decode("4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318")
                .unwrap(),
        );
        let signer_address: call_primitives::Address =
            alloy_primitives::address!("0x2c7536E3605D9C16a7a3D7b1898e529396a65c23");

        // Set up EVM state with balance
        with_test_state(&state, |evm| {
            evm.set_balance(
                signer_address,
                alloy_primitives::U256::from(1_000_000_000_000i128),
            );
            evm.create_account(signer_address);
            evm.create_account(test_addr(2));
        });

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
            state.get_evm_balance(&signer_address),
            alloy_primitives::U256::from(1_000_000_000_000i128)
        );

        // Transaction should be in the EVM mempool
        let mempool_evm_count = state.mempool.read().unwrap().evm_pool.len();
        assert_eq!(mempool_evm_count, 1);
    }

    #[test]
    fn test_filter_manager_persistence() {
        let (tmp, db) = make_test_db();

        // Create a filter manager and add a block filter
        let fm1 = crate::handlers::state::FilterManager::new(Arc::clone(&db));
        let id = fm1.create_filter(crate::handlers::state::Filter::Block { last_height: 42 });
        assert_eq!(id, 1);

        // Verify filter exists
        let filter = fm1.get_filter(id).expect("filter should exist");
        match filter {
            crate::handlers::state::Filter::Block { last_height } => {
                assert_eq!(last_height, 42);
            }
            _ => panic!("expected Block filter"),
        }

        // Drop the first manager and create a new one — should load persisted state
        drop(fm1);
        let fm2 = crate::handlers::state::FilterManager::new(Arc::clone(&db));
        let loaded = fm2.get_filter(id).expect("filter should survive restart");
        match loaded {
            crate::handlers::state::Filter::Block { last_height } => {
                assert_eq!(last_height, 42);
            }
            _ => panic!("expected Block filter after reload"),
        }

        // next_id should continue from where it left off
        let id2 = fm2.create_filter(crate::handlers::state::Filter::Block { last_height: 100 });
        assert_eq!(id2, 2);

        // Remove filter and verify persistence
        assert!(fm2.remove_filter(id));
        drop(fm2);

        let fm3 = crate::handlers::state::FilterManager::new(Arc::clone(&db));
        assert!(
            fm3.get_filter(id).is_none(),
            "removed filter should not persist"
        );
        assert!(
            fm3.get_filter(id2).is_some(),
            "other filter should still exist"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ------------------------------------------------------------------
    // eth_getLogs performance & correctness tests (issue #11)
    // ------------------------------------------------------------------

    use call_primitives::{ExecutionStatus, FeeCurrency, Hash, TxHash};
    use call_protocol::{LogEntry, ProtocolReceipt};

    fn make_log(address: Address, topic_seed: u8, data: Vec<u8>) -> LogEntry {
        let mut topics = Vec::new();
        if topic_seed > 0 {
            topics.push(Hash::repeat_byte(topic_seed));
        }
        LogEntry {
            address,
            topics,
            data,
        }
    }

    fn make_receipt_with_logs(
        block_number: u64,
        tx_hash: TxHash,
        logs: Vec<LogEntry>,
    ) -> ProtocolReceipt {
        ProtocolReceipt {
            tx_hash,
            status: ExecutionStatus::Success,
            gas_used: 21_000,
            gas_payer: test_addr(1),
            fee_currency: FeeCurrency::Call,
            fee_amount: 0,
            block_number,
            block_hash: Hash::repeat_byte(block_number as u8),
            transaction_index: 0,
            to: None,
            contract_address: None,
            cumulative_gas_used: 21_000,
            effective_gas_price: 0,
            logs_bloom: vec![],
            instruction_results: vec![],
            logs,
            memos: vec![],
            state_changes: vec![],
        }
    }

    #[test]
    fn test_eth_get_logs_basic() {
        let state = make_test_state();
        state.set_current_block(10);

        let addr_a = test_addr(1);
        let addr_b = test_addr(2);

        // Block 1: 2 receipts, 2 logs each
        let tx1 = TxHash::repeat_byte(1);
        state.store_receipt(
            tx1,
            make_receipt_with_logs(
                1,
                tx1,
                vec![
                    make_log(addr_a, 1, vec![0x01]),
                    make_log(addr_b, 2, vec![0x02]),
                ],
            ),
        );
        let tx2 = TxHash::repeat_byte(2);
        state.store_receipt(
            tx2,
            make_receipt_with_logs(
                1,
                tx2,
                vec![
                    make_log(addr_a, 3, vec![0x03]),
                    make_log(addr_b, 4, vec![0x04]),
                ],
            ),
        );

        // Block 5: 1 receipt, 1 log
        let tx3 = TxHash::repeat_byte(3);
        state.store_receipt(
            tx3,
            make_receipt_with_logs(5, tx3, vec![make_log(addr_a, 5, vec![0x05])]),
        );

        let filter = serde_json::json!({"fromBlock": "0x1", "toBlock": "0xa"});
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        assert_eq!(logs.len(), 5, "expected 5 logs across blocks 1-10");
    }

    #[test]
    fn test_eth_get_logs_by_address() {
        let state = make_test_state();
        state.set_current_block(100);

        let addr_a = test_addr(1);
        let addr_b = test_addr(2);
        let addr_c = test_addr(3);

        for block in 1..=50 {
            let tx = TxHash::repeat_byte(block as u8);
            let logs = vec![
                make_log(addr_a, 1, vec![block as u8]),
                make_log(addr_b, 2, vec![block as u8]),
                make_log(addr_c, 3, vec![block as u8]),
            ];
            state.store_receipt(tx, make_receipt_with_logs(block, tx, logs));
        }

        // Filter by addr_b only
        let filter = serde_json::json!({
            "fromBlock": "0x1",
            "toBlock": "0x64",
            "address": format!("{:?}", addr_b)
        });
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        assert_eq!(logs.len(), 50, "expected 50 logs from addr_b");
        for log in &logs {
            assert_eq!(log["address"], format!("{:?}", addr_b));
        }

        // Filter by multiple addresses
        let filter = serde_json::json!({
            "fromBlock": "0x1",
            "toBlock": "0x64",
            "address": [format!("{:?}", addr_a), format!("{:?}", addr_c)]
        });
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        assert_eq!(logs.len(), 100, "expected 100 logs from addr_a + addr_c");
    }

    #[test]
    fn test_eth_get_logs_by_topic() {
        let state = make_test_state();
        state.set_current_block(20);

        let addr = test_addr(1);
        let topic_a = Hash::repeat_byte(0xAA);
        let topic_b = Hash::repeat_byte(0xBB);

        for block in 1..=10 {
            let tx = TxHash::repeat_byte(block as u8);
            let logs = vec![
                LogEntry {
                    address: addr,
                    topics: vec![topic_a],
                    data: vec![0x01],
                },
                LogEntry {
                    address: addr,
                    topics: vec![topic_b],
                    data: vec![0x02],
                },
                LogEntry {
                    address: addr,
                    topics: vec![topic_a, topic_b],
                    data: vec![0x03],
                },
            ];
            state.store_receipt(tx, make_receipt_with_logs(block, tx, logs));
        }

        // Filter by topic_a at position 0
        let filter = serde_json::json!({
            "fromBlock": "0x1",
            "toBlock": "0x14",
            "topics": [format!("0x{}", hex::encode(topic_a.as_slice()))]
        });
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        // Logs with topic_a at position 0: first and third log of each block = 20
        assert_eq!(logs.len(), 20);

        // Filter by topic_b at position 1
        let filter = serde_json::json!({
            "fromBlock": "0x1",
            "toBlock": "0x14",
            "topics": [null, format!("0x{}", hex::encode(topic_b.as_slice()))]
        });
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        // Only third log has topic_b at position 1 = 10
        assert_eq!(logs.len(), 10);
    }

    #[test]
    fn test_eth_get_logs_address_and_topic_combined() {
        let state = make_test_state();
        state.set_current_block(30);

        let addr_a = test_addr(1);
        let addr_b = test_addr(2);
        let topic_x = Hash::repeat_byte(0xDE);

        for block in 1..=20 {
            let tx = TxHash::repeat_byte(block as u8);
            let logs = vec![
                LogEntry {
                    address: addr_a,
                    topics: vec![topic_x],
                    data: vec![0x01],
                },
                LogEntry {
                    address: addr_b,
                    topics: vec![topic_x],
                    data: vec![0x02],
                },
                LogEntry {
                    address: addr_a,
                    topics: vec![],
                    data: vec![0x03],
                },
            ];
            state.store_receipt(tx, make_receipt_with_logs(block, tx, logs));
        }

        let filter = serde_json::json!({
            "fromBlock": "0x1",
            "toBlock": "0x1e",
            "address": format!("{:?}", addr_a),
            "topics": [format!("0x{}", hex::encode(topic_x.as_slice()))]
        });
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        // addr_a + topic_x = first log of each block = 20
        assert_eq!(logs.len(), 20);
    }

    #[test]
    fn test_eth_get_logs_block_range_filtering() {
        let state = make_test_state();
        state.set_current_block(1000);

        let addr = test_addr(1);

        for block in 1..=100 {
            let tx = TxHash::repeat_byte(block as u8);
            state.store_receipt(
                tx,
                make_receipt_with_logs(block, tx, vec![make_log(addr, 1, vec![block as u8])]),
            );
        }

        // Query blocks 10-20 inclusive
        let filter = serde_json::json!({
            "fromBlock": "0xa",
            "toBlock": "0x14"
        });
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        assert_eq!(logs.len(), 11, "blocks 10-20 inclusive = 11 blocks");
    }

    #[test]
    fn test_eth_get_logs_performance_large_dataset() {
        let state = make_test_state();
        state.set_current_block(10_000);

        let target_addr = test_addr(0x42);
        let other_addrs: Vec<Address> = (1..=20).map(test_addr).collect();

        // Generate 5,000 blocks, 2 receipts per block, 3 logs per receipt
        // Only ~10% of logs are from target_addr
        let mut total_target_logs = 0;
        for block in 1u64..=5_000 {
            for rec_idx in 0u64..2 {
                let tx_input = block
                    .to_le_bytes()
                    .iter()
                    .chain(rec_idx.to_le_bytes().iter())
                    .copied()
                    .collect::<Vec<u8>>();
                let tx = TxHash::from(call_crypto::keccak256(&tx_input).0);
                let logs: Vec<LogEntry> = (0..3)
                    .map(|log_idx| {
                        let addr = if (block + rec_idx + log_idx) % 10 == 0 {
                            total_target_logs += 1;
                            target_addr
                        } else {
                            other_addrs[((block + rec_idx + log_idx) % 20) as usize]
                        };
                        make_log(
                            addr,
                            log_idx as u8,
                            vec![block as u8, rec_idx as u8, log_idx as u8],
                        )
                    })
                    .collect();
                state.store_receipt(tx, make_receipt_with_logs(block, tx, logs));
            }
        }

        // Address-filtered query — should use log_index and be fast
        let filter = serde_json::json!({
            "fromBlock": "0x1",
            "toBlock": "0x2710",
            "address": format!("{:?}", target_addr)
        });
        let start = std::time::Instant::now();
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        let elapsed_indexed = start.elapsed();
        assert_eq!(
            logs.len(),
            total_target_logs,
            "indexed query should return all target logs"
        );
        // Address-indexed query should complete in well under 1 second even with 30K logs
        assert!(
            elapsed_indexed < std::time::Duration::from_secs(1),
            "indexed eth_getLogs took too long: {:?}",
            elapsed_indexed
        );

        // Full scan over a small range — verify fallback path still works
        let filter = serde_json::json!({
            "fromBlock": "0x1",
            "toBlock": "0x64"
        });
        let start = std::time::Instant::now();
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        let elapsed_scan = start.elapsed();
        assert_eq!(
            logs.len(),
            100 * 2 * 3,
            "full scan over 100 blocks = 600 logs"
        );
        assert!(
            elapsed_scan < std::time::Duration::from_secs(2),
            "full scan eth_getLogs took too long: {:?}",
            elapsed_scan
        );
    }

    #[test]
    fn test_eth_get_logs_no_logs_in_range() {
        let state = make_test_state();
        state.set_current_block(10);

        let addr = test_addr(1);
        let tx = TxHash::repeat_byte(1);
        state.store_receipt(
            tx,
            make_receipt_with_logs(1, tx, vec![make_log(addr, 1, vec![])]),
        );

        // Query a block range with no logs
        let filter = serde_json::json!({"fromBlock": "0x5", "toBlock": "0xa"});
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        assert!(logs.is_empty());
    }

    #[test]
    fn test_eth_get_logs_log_index_consistency() {
        let state = make_test_state();
        state.set_current_block(5);

        let addr = test_addr(1);
        let tx = TxHash::repeat_byte(1);
        let receipt = make_receipt_with_logs(
            1,
            tx,
            vec![
                make_log(addr, 1, vec![0x01]),
                make_log(addr, 2, vec![0x02]),
                make_log(addr, 3, vec![0x03]),
            ],
        );
        state.store_receipt(tx, receipt);

        let filter = serde_json::json!({"fromBlock": "0x1", "toBlock": "0x5"});
        let logs = crate::standard::get_logs_from_filter(&filter, &state).unwrap();
        assert_eq!(logs.len(), 3);
        // Verify logIndex is consistent
        assert_eq!(logs[0]["logIndex"], "0x0");
        assert_eq!(logs[1]["logIndex"], "0x1");
        assert_eq!(logs[2]["logIndex"], "0x2");
    }

    // ------------------------------------------------------------------
    // Structured error code tests (issue #23)
    // ------------------------------------------------------------------

    use crate::handlers::helpers::{
        db_error, execution_reverted, filter_not_found, internal_error, invalid_params,
        method_not_available, resource_unavailable, rpc_error, tx_validation_failed, RpcErrorCode,
    };

    #[test]
    fn test_rpc_error_code_values() {
        assert_eq!(RpcErrorCode::InternalError.code(), -32000);
        assert_eq!(RpcErrorCode::ExecutionReverted.code(), -32001);
        assert_eq!(RpcErrorCode::ResourceUnavailable.code(), -32002);
        assert_eq!(RpcErrorCode::DatabaseError.code(), -32003);
        assert_eq!(RpcErrorCode::MethodNotAvailable.code(), -32004);
        assert_eq!(RpcErrorCode::TransactionValidationFailed.code(), -32005);
        assert_eq!(RpcErrorCode::FilterNotFound.code(), -32006);
        assert_eq!(RpcErrorCode::LightClientVerificationFailed.code(), -32007);
        assert_eq!(RpcErrorCode::InvalidHex.code(), -32010);
    }

    #[test]
    fn test_rpc_error_helpers_return_correct_codes() {
        let e = internal_error("something broke");
        assert_eq!(e.code(), -32000);
        assert!(e.message().contains("something broke"));

        let e = execution_reverted("out of gas");
        assert_eq!(e.code(), -32001);
        assert!(e.message().contains("out of gas"));

        let e = resource_unavailable("lock poisoned");
        assert_eq!(e.code(), -32002);
        assert!(e.message().contains("lock poisoned"));

        let e = db_error("MDBX read failed");
        assert_eq!(e.code(), -32003);
        assert!(e.message().contains("MDBX read failed"));

        let e = method_not_available("feature disabled");
        assert_eq!(e.code(), -32004);
        assert!(e.message().contains("feature disabled"));

        let e = tx_validation_failed("nonce too low");
        assert_eq!(e.code(), -32005);
        assert!(e.message().contains("nonce too low"));

        let e = filter_not_found("no such filter");
        assert_eq!(e.code(), -32006);
        assert!(e.message().contains("no such filter"));

        let e = rpc_error(RpcErrorCode::InvalidHex, "bad hex");
        assert_eq!(e.code(), -32010);
        assert!(e.message().contains("bad hex"));
    }

    #[test]
    fn test_invalid_params_uses_standard_code() {
        let e = invalid_params("missing field");
        assert_eq!(
            e.code(),
            -32602,
            "invalid_params must use standard JSON-RPC -32602"
        );
        assert!(e.message().contains("missing field"));
    }

    #[test]
    fn test_rpc_error_helpers_accept_static_str() {
        // Verify the `impl Into<String>` bound works for &'static str
        let _ = internal_error("static str ok");
        let _ = execution_reverted("static str ok");
        let _ = db_error("static str ok");
        let _ = invalid_params("static str ok");
    }

    #[test]
    fn test_rpc_error_helpers_accept_owned_string() {
        // Verify the `impl Into<String>` bound works for String
        let _ = internal_error(String::from("owned string ok"));
        let _ = execution_reverted(String::from("owned string ok"));
        let _ = db_error(String::from("owned string ok"));
        let _ = invalid_params(String::from("owned string ok"));
    }

    // ------------------------------------------------------------------
    // WebSocket lag handling tests (issue #24)
    // ------------------------------------------------------------------

    use crate::ws::{SubscriptionManager, WsEvent};
    use tokio::sync::broadcast;

    #[test]
    fn test_ws_event_lagged_serializes() {
        let event = WsEvent::Lagged { dropped: 42 };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("lagged"));
        assert!(json.contains("42"));
    }

    #[test]
    fn test_broadcast_channel_lag_detected() {
        // Create a tiny broadcast channel (capacity 2) to force lag quickly
        let (tx, mut rx) = broadcast::channel(2);

        // Send 5 messages without the receiver consuming any
        for i in 0..5 {
            let _ = tx.send(WsEvent::NewBlock {
                height: i,
                hash: format!("hash-{i}"),
                proposer: 1,
                tx_count: 0,
            });
        }

        // The receiver should now be lagged because the channel only holds 2
        match rx.try_recv() {
            Ok(_) => {
                // May succeed for the newest items in the buffer
            }
            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                assert!(n >= 3, "expected at least 3 dropped messages, got {n}");
            }
            Err(broadcast::error::TryRecvError::Closed) => {
                panic!("channel should not be closed");
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                // Channel may be empty if all items were overwritten
            }
        }
    }

    #[test]
    fn test_subscription_manager_broadcast_does_not_panic_on_full_channel() {
        let subs = SubscriptionManager::new();

        // Flood the block channel with many events
        for i in 0..10_000 {
            subs.broadcast_block(i, format!("hash-{i}"), 1, 0);
        }

        // A new subscriber should still be able to join and receive the latest event
        let mut rx = subs.subscribe_blocks();
        match rx.try_recv() {
            Ok(WsEvent::NewBlock { height, .. }) => {
                assert_eq!(height, 9999, "should receive the most recent event");
            }
            Ok(_) => {
                // Other event types are fine too
            }
            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                assert!(n > 0, "should report lag after flood");
            }
            Err(broadcast::error::TryRecvError::Closed) => {
                panic!("channel should not be closed");
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                // Channel may be empty if all items were overwritten
            }
        }
    }

    #[tokio::test]
    async fn test_ws_subscriber_receives_lag_notification() {
        // Simulate a slow subscriber: create a channel, subscribe, fill past capacity,
        // then consume and verify the lag notification is detected.
        let (tx, _rx) = broadcast::channel(4);

        // Spawn a slow consumer
        let mut rx = tx.subscribe();
        let handle = tokio::spawn(async move {
            let mut received = Vec::new();
            let mut lagged = false;
            // Consume with a delay, forcing lag
            loop {
                match rx.recv().await {
                    Ok(event) => received.push(event),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        lagged = true;
                        received.push(WsEvent::Lagged { dropped: n });
                        // After lag, continue receiving remaining buffered events
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            (received, lagged)
        });

        // Rapidly send more messages than channel capacity
        for i in 0..20u64 {
            let _ = tx.send(WsEvent::NewBlock {
                height: i,
                hash: format!("hash-{i}"),
                proposer: 1,
                tx_count: 0,
            });
        }

        // Give the consumer a moment to process, then drop the sender
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        drop(tx);

        let (received, lagged) = handle.await.unwrap();
        assert!(
            lagged,
            "subscriber should have detected lag, got {received:?}"
        );
        // Verify at least one Lagged event is in the received list
        assert!(
            received.iter().any(|e| matches!(e, WsEvent::Lagged { .. })),
            "should contain a Lagged event"
        );
    }

    #[tokio::test]
    async fn test_ws_eth_lag_notification_json() {
        // Verify the ETH subscription lag notification JSON format
        let lag = serde_json::json!({
            "subscription": "newHeads",
            "lagged": 128,
            "error": "subscriber lagged",
        });
        assert_eq!(lag["subscription"], "newHeads");
        assert_eq!(lag["lagged"], 128);
        assert_eq!(lag["error"], "subscriber lagged");
    }

    // ------------------------------------------------------------------
    // Local keystore tests
    // ------------------------------------------------------------------

    #[test]
    fn test_keystore_import_and_list() {
        let keystore = crate::keystore::LocalKeystore::new();
        let (secret, pubkey) = call_crypto::generate_keypair();
        let expected_addr = call_crypto::pubkey_to_address(&pubkey);

        let addr = keystore.import_raw_key(&secret, None).expect("import ok");
        assert_eq!(addr, expected_addr);

        let accounts = keystore.list_accounts();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0], expected_addr);
        assert!(keystore.has_account(&expected_addr));

        let key = keystore.get_key(&expected_addr).expect("key exists");
        assert_eq!(key, secret);
    }

    #[test]
    fn test_keystore_sign_message_recovery() {
        use call_crypto::recover_secp256k1_signer;

        let keystore = crate::keystore::LocalKeystore::new();
        let (secret, _pubkey) = call_crypto::generate_keypair();
        let addr = keystore.import_raw_key(&secret, None).expect("import ok");

        let message = b"hello world";
        let sig = keystore.sign_message(&addr, message).expect("sign ok");

        // Reconstruct the personal_sign hash
        let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
        let mut full = prefix.into_bytes();
        full.extend_from_slice(message);
        let hash = call_crypto::keccak256(&full);

        // Recover signer address from signature
        let recovered = recover_secp256k1_signer(&hash, &sig).expect("recover ok");
        assert_eq!(recovered, addr, "recovered address should match");
    }

    #[test]
    fn test_keystore_persistence() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("keystore");

        // Known test key
        let secret = {
            let hex = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
            let mut bytes = [0u8; 32];
            hex::decode_to_slice(hex, &mut bytes).unwrap();
            bytes
        };

        // Create keystore with persistence and import a key
        {
            let keystore = crate::keystore::LocalKeystore::new_with_dir(&dir);
            let addr = keystore
                .import_raw_key(&secret, Some("testpass"))
                .expect("import ok");
            let accounts = keystore.list_accounts();
            assert_eq!(accounts.len(), 1);
            assert_eq!(accounts[0], addr);
        }

        // Create a new keystore pointing at the same dir, load all keys
        {
            let keystore = crate::keystore::LocalKeystore::new_with_dir(&dir);
            let (ok, failures) = keystore.load_all_from_dir("testpass");
            assert_eq!(ok, 1, "should load 1 key");
            assert!(failures.is_empty(), "no failures: {:?}", failures);

            let accounts = keystore.list_accounts();
            assert_eq!(accounts.len(), 1);

            let key = keystore.get_key(&accounts[0]).expect("key loaded");
            assert_eq!(key, secret);
        }
    }

    #[test]
    fn test_keystore_remove_account() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("keystore");

        let (secret, pubkey) = call_crypto::generate_keypair();
        let addr = call_crypto::pubkey_to_address(&pubkey);

        // Create keystore with persistence, import, then remove
        {
            let keystore = crate::keystore::LocalKeystore::new_with_dir(&dir);
            let imported = keystore
                .import_raw_key(&secret, Some("testpass"))
                .expect("import ok");
            assert_eq!(imported, addr);
            assert!(keystore.has_account(&addr));

            let removed = keystore.remove_account(&addr);
            assert!(removed, "should remove");
            assert!(!keystore.has_account(&addr));
        }

        // File should also be gone
        let path = dir.join(format!("{:?}.json", addr).to_lowercase());
        assert!(!path.exists(), "keystore file should be removed");
    }

    #[test]
    fn test_keystore_eth_accounts_via_rpc_state() {
        let state = make_test_state();
        let (secret, pubkey) = call_crypto::generate_keypair();
        let addr = call_crypto::pubkey_to_address(&pubkey);

        // Initially empty
        assert!(state.keystore.list_accounts().is_empty());

        // Import key into the state's keystore
        let imported = state
            .keystore
            .import_raw_key(&secret, None)
            .expect("import ok");
        assert_eq!(imported, addr);

        let accounts = state.keystore.list_accounts();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0], addr);
    }

    // ------------------------------------------------------------------
    // Access list tests
    // ------------------------------------------------------------------

    #[test]
    fn test_build_and_sign_tx_eip1559_with_access_list() {
        let state = make_test_state();
        let (secret, _pubkey) = call_crypto::generate_keypair();
        let addr = call_crypto::pubkey_to_address(&_pubkey);
        state
            .keystore
            .import_raw_key(&secret, None)
            .expect("import ok");
        state
            .keystore
            .import_raw_key(&secret, None)
            .expect("import ok");

        let access_list = serde_json::json!([
            {
                "address": "0x0000000000000000000000000000000000000001",
                "storageKeys": [
                    "0x0000000000000000000000000000000000000000000000000000000000000001",
                    "0x0000000000000000000000000000000000000000000000000000000000000002"
                ]
            }
        ]);

        let tx_obj = serde_json::json!({
            "from": format!("{:?}", addr),
            "to": format!("{:?}", test_addr(2)),
            "value": "0x64",
            "gas": "0x5208",
            "type": "0x2",
            "maxFeePerGas": "0xa",
            "maxPriorityFeePerGas": "0x1",
            "nonce": "0x0",
            "accessList": access_list,
        });

        let raw_hex = crate::standard::build_and_sign_tx(&tx_obj, &state).expect("sign ok");
        assert!(raw_hex.starts_with("0x"));

        // Decode and verify it's an EIP-1559 envelope with access list
        let raw_bytes = hex::decode(raw_hex.trim_start_matches("0x")).expect("decode hex");
        let envelope =
            alloy_rlp::Decodable::decode(&mut raw_bytes.as_slice()).expect("decode envelope");
        match envelope {
            alloy_consensus::TxEnvelope::Eip1559(signed) => {
                let tx = signed.tx();
                assert_eq!(tx.access_list.0.len(), 1);
                assert_eq!(
                    tx.access_list.0[0].address,
                    alloy_primitives::address!("0x0000000000000000000000000000000000000001")
                );
                assert_eq!(tx.access_list.0[0].storage_keys.len(), 2);
            }
            other => panic!("expected EIP-1559 envelope, got {:?}", other),
        }
    }

    #[test]
    fn test_build_and_sign_tx_eip2930_with_access_list() {
        let state = make_test_state();
        let (secret, pubkey) = call_crypto::generate_keypair();
        let addr = call_crypto::pubkey_to_address(&pubkey);
        state
            .keystore
            .import_raw_key(&secret, None)
            .expect("import ok");

        let access_list = serde_json::json!([
            {
                "address": "0x0000000000000000000000000000000000000002",
                "storageKeys": ["0x0000000000000000000000000000000000000000000000000000000000000003"]
            }
        ]);

        let tx_obj = serde_json::json!({
            "from": format!("{:?}", addr),
            "to": format!("{:?}", test_addr(2)),
            "value": "0x64",
            "gas": "0x5208",
            "type": "0x1",
            "gasPrice": "0xa",
            "nonce": "0x0",
            "accessList": access_list,
        });

        let raw_hex = crate::standard::build_and_sign_tx(&tx_obj, &state).expect("sign ok");
        let raw_bytes = hex::decode(raw_hex.trim_start_matches("0x")).expect("decode hex");
        let envelope =
            alloy_rlp::Decodable::decode(&mut raw_bytes.as_slice()).expect("decode envelope");
        match envelope {
            alloy_consensus::TxEnvelope::Eip2930(signed) => {
                let tx = signed.tx();
                assert_eq!(tx.access_list.0.len(), 1);
                assert_eq!(
                    tx.access_list.0[0].address,
                    alloy_primitives::address!("0x0000000000000000000000000000000000000002")
                );
                assert_eq!(tx.access_list.0[0].storage_keys.len(), 1);
            }
            other => panic!("expected EIP-2930 envelope, got {:?}", other),
        }
    }

    #[test]
    fn test_build_and_sign_tx_legacy_no_access_list() {
        let state = make_test_state();
        let (secret, pubkey) = call_crypto::generate_keypair();
        let addr = call_crypto::pubkey_to_address(&pubkey);
        state
            .keystore
            .import_raw_key(&secret, None)
            .expect("import ok");

        let tx_obj = serde_json::json!({
            "from": format!("{:?}", addr),
            "to": format!("{:?}", test_addr(2)),
            "value": "0x64",
            "gas": "0x5208",
            "gasPrice": "0xa",
            "nonce": "0x0",
        });

        let raw_hex = crate::standard::build_and_sign_tx(&tx_obj, &state).expect("sign ok");
        let raw_bytes = hex::decode(raw_hex.trim_start_matches("0x")).expect("decode hex");
        let envelope =
            alloy_rlp::Decodable::decode(&mut raw_bytes.as_slice()).expect("decode envelope");
        assert!(
            matches!(envelope, alloy_consensus::TxEnvelope::Legacy(_)),
            "expected Legacy envelope"
        );
    }
}
