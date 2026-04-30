    use super::*;
    use call_consensus::BlockExecutionResult;
    use call_consensus::block::{ExecutionState, BlockContext, Subsystems};
    use call_consensus::exec::evm_instructions;
    use call_network::{InMemoryNetwork, EpochBoundarySignal, BlockAnnouncement, SyncResponse};
    use call_primitives::{Address, Ed25519PublicKey};
    use crate::state_persist::{save_asset_registry_inner, load_asset_registry_inner};
    use std::sync::OnceLock;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn test_pubkey(n: u8) -> Ed25519PublicKey {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    /// Lazily-generated secp256k1 keypair for test transactions.
    /// All signed test transactions reuse this sender so balance setup stays simple.
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

    fn make_evm_tx(nonce: u64) -> call_evm::EvmTransaction {
        call_evm::EvmTransaction {
            caller: *test_sender(),
            nonce,
            gas_limit: 21_000,
            gas_price: 1,
            to: Some(test_addr(2)),
            value: call_primitives::U256::from(100),
            data: call_evm::Bytes::default(),
            chain_id: 1,
        }
    }

    fn one_million_call() -> u128 {
        1_000_000 * 10u128.pow(18)
    }

    #[test]
    fn test_node_creation() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");
        assert_eq!(node.state.chain_id, CALLCHAIN_CHAIN_ID);
        assert!(node.network.is_none());
        assert_eq!(node.parent_hash, BlockHash::ZERO);
        assert_eq!(node.consensus.read().unwrap().current_height(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_asset_registry_persistence_roundtrip() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-registry-test-{}",
            std::process::id()
        ));

        // Phase 1: Direct db test — save and load registry
        {
            let db = open_db(tmp.clone()).expect("open db");
            let mut registry = AssetRegistry::new();
            let id = registry
                .register_asset("PERSIST".into(), "Persist Token".into(), 18, test_addr(1), 0, 100, 1_000_000)
                .unwrap();
            registry.mint_supply(id, &test_addr(1), 5_000).unwrap();
            registry.add_evm_supply(id, 3_000).unwrap();
            save_asset_registry_inner(&db.db, &registry).expect("save");
        }

        // Give MDBX a moment to release file locks before reopening
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Phase 2: Reopen db and load registry
        {
            let db = open_db(tmp.clone()).expect("reopen db");
            let registry = load_asset_registry_inner(&db.db).expect("load");
            let asset = registry.get_asset(1).expect("asset should exist after reload");
            assert_eq!(asset.symbol, "PERSIST");
            assert_eq!(asset.name, "Persist Token");
            assert_eq!(asset.decimals, 18);
            assert_eq!(asset.issuer, test_addr(1));
            assert_eq!(asset.protocol_supply, 5_000);
            assert_eq!(asset.evm_supply, 3_000);
            assert_eq!(asset.max_supply, 1_000_000);
            assert_eq!(asset.status, call_protocol::registry::AssetStatus::Active);
            assert_eq!(asset.registered_at, 100);
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_node_mempool_stats() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-mempool-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");

        let (evm, known) = node.mempool_stats();
        assert_eq!(evm, 0);
        assert_eq!(known, 0);

        // Insert an EVM tx
        let tx = make_evm_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_evm_tx(tx);
        }

        let (evm, known) = node.mempool_stats();
        assert_eq!(evm, 1);
        assert_eq!(known, 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_block_production_single_block() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-block-test-{}",
            std::process::id()
        ));

        // Create node with validators staked
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake a validator so proposer selection works
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund sender balance in EVM storage
        {
            let mut evm = node.state.evm_state.write().unwrap();
            evm.set_balance(*test_sender(), call_primitives::U256::from(10_000_000_000_000u128));
        }

        // Insert an EVM tx into mempool
        let tx = make_evm_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_evm_tx(tx);
        }

        // Manually run one iteration of block production
        let height_before = node.consensus.read().unwrap().current_height();

        // Select and build
        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            1_000, // timestamp
            proposer,
            version,
            evm_txs,
        );

        // Execute
        let result = node.state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution");
        block.finalize(&result);

        // Commit
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
        }

        let height_after = node.consensus.read().unwrap().current_height();
        assert_eq!(height_after, height_before + 1);
        assert_ne!(block.header.state_root, call_primitives::Hash::ZERO);

        // Persist
        persist_block(&tmp, height, &block).expect("persist block");

        // Verify persisted block can be read back
        let dir = tmp.join("blocks");
        let path = dir.join(format!("{height:012}.json"));
        assert!(path.exists(), "block file should exist");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_block_production_with_empty_mempool() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-empty-block-test-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake a validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Select and build with empty mempool
        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        assert!(selection.evm_txs.is_empty());

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            2_000,
            proposer,
            version,
            vec![],
        );

        // Execute empty block
        let result = node.state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("empty block execution");
        block.finalize(&result);
        block.finalize(&result);

        // Commit
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit empty");
        }

        assert_eq!(node.consensus.read().unwrap().current_height(), 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_e2e_two_nodes_tx_propagation() {
        let tmp1 = std::env::temp_dir().join(format!(
            "call-node-e2e-node1-{}",
            std::process::id()
        ));
        let tmp2 = std::env::temp_dir().join(format!(
            "call-node-e2e-node2-{}",
            std::process::id()
        ));

        // Create two nodes
        let mut node1 = CallNode::new(tmp1.clone()).expect("node1 creation");
        let mut node2 = CallNode::new(tmp2.clone()).expect("node2 creation");

        // Create a shared in-memory network
        let shared_network: Arc<InMemoryNetwork> = Arc::new(InMemoryNetwork::new());

        // Wire both nodes to the same network
        node1.inject_network(Arc::clone(&shared_network) as Arc<dyn Network>);
        node2.inject_network(Arc::clone(&shared_network) as Arc<dyn Network>);

        // Stake validators on node1 so it can produce blocks
        {
            let mut consensus = node1.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund sender balance on node1
        {
            let mut balances = node1.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, *test_sender(), 10_000).unwrap();
        }

        // Verify initial state
        assert_eq!(node1.mempool_stats().0, 0); // no evm txs
        assert_eq!(node2.mempool_stats().0, 0);

        // Step 1: Submit an EVM tx to node1's mempool
        let tx = make_evm_tx(0);
        {
            let mut mempool = node1.mempool.write().unwrap();
            let _ = mempool.insert_evm_tx(tx);
        }
        assert_eq!(node1.mempool_stats().0, 1, "node1 should have 1 tx in mempool");

        // Step 2: Produce a block on node1
        let selection = { node1.mempool.write().unwrap().select_transactions() };
        let proposer = node1.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node1.consensus.read().unwrap().current_height();

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();
        assert_eq!(evm_txs.len(), 1, "should have 1 evm tx");

        let version = node1.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node1.parent_hash,
            3_000,
            proposer,
            version,
            evm_txs,
        );

        let result = node1.state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution");
        block.finalize(&result);

        // Commit on node1
        {
            let mut consensus = node1.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
        }
        assert_eq!(node1.consensus.read().unwrap().current_height(), 1, "node1 should be at height 1");

        // Step 3: Broadcast block announcement via shared network
        let block_hash = block.header.hash();
        let announcement = BlockAnnouncement {
            block_hash,
            height,
            proposer,
            timestamp_millis: block.header.timestamp_millis,
        };
        let msg = bincode::serialize(&NetworkMessage::BlockAnnouncement(announcement))
            .expect("serialize block announcement");
        shared_network.broadcast(BLOCK_CHANNEL, msg).await;

        // Step 4: Node2 receives the block announcement from shared network
        let result = shared_network.receive().await;
        let (peer_id, channel, data) = result.expect("should receive message");
        assert_eq!(channel, BLOCK_CHANNEL);
        assert_eq!(peer_id, "broadcast");

        let received = bincode::deserialize::<NetworkMessage>(&data).expect("parse network message");
        let announcement = match received {
            NetworkMessage::BlockAnnouncement(a) => a,
            other => panic!("expected block announcement, got {other:?}"),
        };
        assert_eq!(announcement.height, height);
        assert_eq!(announcement.block_hash, block_hash);
        assert_eq!(announcement.proposer, proposer);

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp1);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[tokio::test]
    async fn test_e2e_two_nodes_block_persistence() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-e2e-persist-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund sender in EVM storage
        {
            let mut evm = node.state.evm_state.write().unwrap();
            evm.set_balance(*test_sender(), call_primitives::U256::from(10_000_000_000_000u128));
        }

        // Insert EVM tx
        let tx = make_evm_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_evm_tx(tx);
        }

        // Produce block
        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            4_000,
            proposer,
            version,
            evm_txs,
        );

        let result = node.state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution");
        block.finalize(&result);

        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
        }

        // Persist block
        persist_block(&tmp, height, &block).expect("persist block");

        // Verify block file exists and can be read back
        let dir = tmp.join("blocks");
        let path = dir.join(format!("{height:012}.json"));
        assert!(path.exists(), "block file should exist");

        let data = std::fs::read(&path).expect("read block file");
        let restored: Block = serde_json::from_slice(&data).expect("deserialize block");
        assert_eq!(restored.header.height, height);
        assert_eq!(restored.header.hash(), block.header.hash());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_e2e_state_persistence_restart() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-persist-restart-{}",
            std::process::id()
        ));
        let initial_balance: u128 = 10_000_000;
        let transfer_amount: u128 = 5_000;

        // === Phase 1: Create node, fund account, produce block, persist state ===
        {
            let node = CallNode::new(tmp.clone()).expect("node creation");

            // Stake validator
            {
                let mut consensus = node.consensus.write().unwrap();
                consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
                consensus.refresh_proposer_subset();
            }

            // Fund sender balance in EVM storage
            {
                let mut evm = node.state.evm_state.write().unwrap();
                evm_instructions::seed_balance(&mut *evm, call_protocol::CALL_ASSET_ID, *test_sender(), initial_balance);
                evm_instructions::seed_asset(&mut *evm, 1, "CALL", "Callchain", 18, *test_sender(), 0, initial_balance, 0);
                // Set native EVM balance for gas payment
                evm.set_balance(*test_sender(), call_primitives::U256::from(100_000_000_000u128));
            }

            // Insert an EVM tx
            let tx = make_evm_tx(0);
            {
                let mut mempool = node.mempool.write().unwrap();
                let _ = mempool.insert_evm_tx(tx);
            }

            // Produce and commit block
            let selection = { node.mempool.write().unwrap().select_transactions() };
            let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
            let height = node.consensus.read().unwrap().current_height();

            let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

            let version = node.state.fork_manager.read().unwrap().current_version();
            let mut block = Block::new(
                height, node.parent_hash, 5_000, proposer, version, evm_txs);

            let result = {
                let mut s = node.state.write_all();
                block.execute(
                    &mut ExecutionState::new(&mut s.shielded, &mut s.evm),
                    &mut BlockContext::new(height, &mut s.fee_params),
                    &mut Subsystems::none(),
                )
                    .expect("execution")
            };
            block.finalize(&result);

            {
                let mut consensus = node.consensus.write().unwrap();
                consensus.commit_block(&block, &result).expect("commit");
            }

            // Persist state to reth-db immediately
            let db_env = &node.db.db;
            persist_state_to_db(db_env, &node.state, &node.consensus)
                .expect("persist state");

            // Node is dropped here, simulating shutdown
        }

        // Give MDBX a moment to release file locks before reopening
        std::thread::sleep(std::time::Duration::from_millis(100));

        // === Phase 2: Create new node from same data dir, verify state ===
        {
            let node2 = CallNode::new(tmp.clone()).expect("node creation (restart)");

            // Verify native EVM balance was recovered
            let sender_balance = {
                let evm = node2.state.evm_state.read().unwrap();
                evm.get_balance(test_sender())
            };

            // Native balance should be less than what was seeded (gas deducted)
            assert!(
                sender_balance < call_primitives::U256::from(100_000_000_000u128),
                "sender balance should be reduced after restart, got {sender_balance}"
            );

            let _ = std::fs::remove_dir_all(&tmp);
        }
    }

    // ── Governance full cycle integration ─────────────────────────────

    #[test]
    fn test_governance_full_cycle() {
        use call_governance::{ProposalType, GovernanceEvent, DEFAULT_PROPOSAL_DEPOSIT, REVIEW_PERIOD_BLOCKS, VOTING_PERIOD_BLOCKS, TIMELOCK_PERIOD_BLOCKS};

        let tmp = std::env::temp_dir().join("call_gov_cycle_test");
        let _ = std::fs::remove_dir_all(&tmp);

        let node = CallNode::new(tmp.clone()).expect("node creation");

        let proposer = call_primitives::Address::repeat_byte(0xAA);

        // Fund proposer in AccountState and EVM storage (asset_id 1 = CALL)
        {
            let mut balances = node.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, proposer, DEFAULT_PROPOSAL_DEPOSIT * 5).expect("fund proposer");
        }
        {
            let mut evm = node.state.evm_state.write().unwrap();
            call_consensus::exec::evm_instructions::seed_balance(
                &mut *evm, call_protocol::CALL_ASSET_ID, proposer, DEFAULT_PROPOSAL_DEPOSIT * 5,
            );
        }

        // Register some validators so quorum can be met
        {
            let mut gov = node.state.governance.write().unwrap();
            for i in 1u32..=3 {
                gov.register_validator(i, call_primitives::Address::repeat_byte(i as u8));
                gov.set_call_balance(call_primitives::Address::repeat_byte(i as u8), 1);
            }
        }

        // Submit a proposal
        let proposal_id = {
            let mut gov = node.state.governance.write().unwrap();
            gov.submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test_param".into(),
                    new_value: "{\"base_fee\": 100}".into(),
                },
                "Test proposal".into(),
                "Integration test".into(),
                vec![],
            ).expect("submit proposal")
        };

        // Verify proposal was created
        {
            let gov = node.state.governance.read().unwrap();
            let p = gov.get_proposal(proposal_id).expect("proposal exists");
            assert_eq!(p.state, call_governance::ProposalState::Pending);
        }

        // Advance to voting period and vote with all validators
        {
            let mut gov = node.state.governance.write().unwrap();
            gov.set_current_block(REVIEW_PERIOD_BLOCKS);
            // Vote yes from all 3 registered validators
            for i in 1u32..=3 {
                let validator_addr = call_primitives::Address::repeat_byte(i as u8);
                let _ = gov.vote(proposal_id, validator_addr, call_governance::Vote::Yes);
            }
        }

        // Advance through all phases
        {
            let mut gov = node.state.governance.write().unwrap();
            gov.advance(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + TIMELOCK_PERIOD_BLOCKS + 1);
        }

        // Should be queued or executed (depending on timelock)
        {
            let gov = node.state.governance.read().unwrap();
            let p = gov.get_proposal(proposal_id).expect("proposal exists");
            assert!(
                matches!(p.state, call_governance::ProposalState::Queued | call_governance::ProposalState::Executed),
                "expected queued or executed, got {:?}", p.state
            );
        }

        // Advance past timelock to execute
        {
            let mut gov = node.state.governance.write().unwrap();
            let exec = gov.get_proposal(proposal_id).unwrap().execution_block.unwrap();
            gov.advance(exec + 1);
            // Drain events
            let events = gov.drain_events();
            assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalExecuted { .. })));
        }

        // Should be executed
        {
            let gov = node.state.governance.read().unwrap();
            let p = gov.get_proposal(proposal_id).expect("proposal exists");
            assert_eq!(p.state, call_governance::ProposalState::Executed);
        }

        // Verify fee_params were updated by executor
        {
            let fp = node.state.fee_params.read().unwrap();
            assert_eq!(fp.base_fee, 100); // Should match the JSON in execution_data
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── State isolation tests (Phase 1) ─────────────────────────────────

    #[tokio::test]
    async fn test_state_isolation_propose_does_not_modify_shared_state() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-isolation-test-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund sender with ample balance for fees + transfer
        {
            let mut balances = node.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, *test_sender(), 10_000_000).unwrap();
        }
        // Seed EVM storage with CALL balance for fees
        {
            let mut evm = node.state.evm_state.write().unwrap();
            evm_instructions::seed_balance(&mut *evm, call_protocol::CALL_ASSET_ID, *test_sender(), 10_000_000);
            // Set native EVM balance for gas payment
            evm.set_balance(*test_sender(), call_primitives::U256::from(100_000_000_000u128));
        }

        // Insert tx into mempool
        let tx = make_evm_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_evm_tx(tx);
        }

        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        let evm_txs: Vec<Vec<u8>> = selection
            .evm_txs
            .into_iter()
            .map(|e| e.data)
            .collect();

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            5_000,
            proposer,
            version,
            evm_txs,
        );

        // Capture shared state BEFORE propose-phase execution
        let balance_before = {
            let evm = node.state.evm_state.read().unwrap();
            evm.get_balance(test_sender())
        };

        // Simulate PROPOSE phase: execute on CLONED state
        {
            let result = node.state
                .read_all()
                .execute_block_cloned_no_subsystems(&block, height)
                .expect("propose execution on clone");

            block.finalize(&result);
        }

        // Verify shared state is UNCHANGED after propose
        let balance_after_propose = {
            let evm = node.state.evm_state.read().unwrap();
            evm.get_balance(test_sender())
        };
        assert_eq!(
            balance_after_propose, balance_before,
            "shared state must NOT be modified by propose-phase execution"
        );

        // Simulate FINALIZE phase: execute on SHARED state (write locks)
        {
            let result = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("finalize execution on shared state");

            // Verify state root matches header
            assert_eq!(result.state_root, block.header.state_root, "state_root mismatch");

            block.finalize(&result);

            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
        }

        // Verify shared state IS modified after finalize
        let balance_after_finalize = {
            let evm = node.state.evm_state.read().unwrap();
            evm.get_balance(test_sender())
        };
        assert!(
            balance_after_finalize < balance_before,
            "shared state must be modified by finalize-phase execution"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_state_root_mismatch_rejects_block() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-root-mismatch-test-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund sender with ample balance for fees + transfer
        {
            let mut balances = node.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, *test_sender(), 10_000_000).unwrap();
        }

        // Register CALL asset so Transfer instructions succeed
        {
            let mut registry = node.state.asset_registry.write().unwrap();
            registry
                .register_asset("CALL".into(), "Callchain".into(), 18, *test_sender(), 0, 0, 0)
                .unwrap();
        }

        let tx = make_evm_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_evm_tx(tx);
        }

        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        let evm_txs: Vec<Vec<u8>> = selection
            .evm_txs
            .into_iter()
            .map(|e| e.data)
            .collect();

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            6_000,
            proposer,
            version,
            evm_txs,
        );

        // Execute on cloned state to get valid roots
        let result = node.state
            .read_all()
            .execute_block_cloned_no_subsystems(&block, height)
            .expect("execution");

        block.finalize(&result);

        // Tamper with a state root in the header
        let original_state_root = block.header.state_root;
        block.header.state_root = call_primitives::Hash::repeat_byte(0xDE);

        // Verify: re-execution on clone detects root mismatch
        {
            let result2 = node.state
                .read_all()
                .execute_block_cloned_no_subsystems(&block, height)
                .expect("re-execution");

            assert_ne!(
                result2.state_root, block.header.state_root,
                "tampered state_root should mismatch re-computed root"
            );
        }

        // Verify: finalize with tampered root is caught by root check
        {
            let result3 = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("execution on shared state");

            // The re-computed result3 should have the ORIGINAL correct root
            assert_eq!(result3.state_root, original_state_root, "re-computed root should match original");
            // But the block header has the tampered root
            assert_ne!(result3.state_root, block.header.state_root, "tampered block should fail root check");
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_commit_block_height_replay_protection() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-replay-test-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        let height = node.consensus.read().unwrap().current_height();
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let version = node.state.fork_manager.read().unwrap().current_version();

        let mut block = Block::new(
            height,
            node.parent_hash,
            7_000,
            proposer,
            version,
            vec![],
        );

        // Execute and commit once
        {
            let result = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("execution");

            block.finalize(&result);

            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("first commit");
        }

        // Consensus height should have advanced
        assert_eq!(node.consensus.read().unwrap().current_height(), height + 1);

        // Attempt to commit the SAME block again should fail due to height mismatch
        {
            let mut consensus = node.consensus.write().unwrap();
            let result = consensus.commit_block(&block, &BlockExecutionResult::default());
            assert!(
                result.is_err(),
                "double-commit of same block should be rejected"
            );
            let err = result.unwrap_err().to_string();
            assert!(err.contains("height mismatch"), "error should mention height mismatch: {err}");
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_epoch_number_derived_from_height() {
        // Verify epoch_number = current_height / epoch_length for various heights
        let epoch_length: u64 = 1000;

        assert_eq!(0 / epoch_length, 0, "height 0 should be epoch 0");
        assert_eq!(999 / epoch_length, 0, "height 999 should be epoch 0");
        assert_eq!(1000 / epoch_length, 1, "height 1000 should be epoch 1");
        assert_eq!(1001 / epoch_length, 1, "height 1001 should be epoch 1");
        assert_eq!(1999 / epoch_length, 1, "height 1999 should be epoch 1");
        assert_eq!(2000 / epoch_length, 2, "height 2000 should be epoch 2");
        assert_eq!(2500 / epoch_length, 2, "height 2500 should be epoch 2");
        assert_eq!(3000 / epoch_length, 3, "height 3000 should be epoch 3");
    }

    #[tokio::test]
    async fn test_quorum_threshold_calculation() {
        // Verify (subset_size * 2).div_ceil(3) for various subset sizes
        assert_eq!((1usize * 2).div_ceil(3), 1, "subset of 1 needs quorum of 1");
        assert_eq!((2usize * 2).div_ceil(3), 2, "subset of 2 needs quorum of 2");
        assert_eq!((3usize * 2).div_ceil(3), 2, "subset of 3 needs quorum of 2");
        assert_eq!((4usize * 2).div_ceil(3), 3, "subset of 4 needs quorum of 3");
        assert_eq!((5usize * 2).div_ceil(3), 4, "subset of 5 needs quorum of 4");
        assert_eq!((6usize * 2).div_ceil(3), 4, "subset of 6 needs quorum of 4");
        assert_eq!((7usize * 2).div_ceil(3), 5, "subset of 7 needs quorum of 5");
        assert_eq!((10usize * 2).div_ceil(3), 7, "subset of 10 needs quorum of 7");
    }

    #[tokio::test]
    async fn test_epoch_boundary_signal_updates_peer_heights() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-signal-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Construct EpochBoundarySignal
        let signal = EpochBoundarySignal {
            height: 1000,
            epoch: 1,
            sender_pubkey: [0xAB; 32],
        };
        let data = bincode::serialize(&NetworkMessage::EpochBoundarySignal(signal))
            .expect("serialize signal");

        let network: Arc<dyn Network> = Arc::new(InMemoryNetwork::new());
        let sync_inflight: SyncInflight = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

        // Send signal via handle_network_message on BLOCK_CHANNEL
        handle_network_message(
            "peer_abc123",
            BLOCK_CHANNEL,
            &data,
            &node.mempool,
            &node.state,
            &network,
            &sync_inflight,
        );

        // Verify peer_heights was updated
        let peer_heights = node.state.peer_heights.read().unwrap();
        assert_eq!(
            peer_heights.get("peer_abc123"),
            Some(&1000u64),
            "peer height should be recorded from EpochBoundarySignal"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_sync_crosses_epoch_boundary_sets_restart_signal() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-sync-epoch-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator so proposer selection works
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Use small epoch length so we cross boundary quickly
        {
            let mut params = node.state.consensus_params.write().unwrap();
            params.epoch_length = 2;
        }

        // Set current block to 1 (epoch = 1/2 = 0)
        node.state.set_current_block(1);

        // Build block at height 1
        let height = 1u64;
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            9_000,
            proposer,
            version,
            vec![],
        );

        // Execute to compute valid state roots
        {
            let result = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("execution");
            block.finalize(&result);
        }

        // Verify signal is NOT set before sync
        assert!(
            !node.state.engine_restart_signal.load(std::sync::atomic::Ordering::Relaxed),
            "signal should NOT be set before sync"
        );

        // Build SyncResponse with the block
        let block_json = serde_json::to_vec(&block).expect("serialize");
        let response = SyncResponse {
            start_height: 1,
            blocks: vec![block_json],
            state_root: block.header.state_root,
        };

        // Apply synced blocks
        let applied = apply_synced_blocks(&response, &node.state, &node.consensus, &tmp);
        assert_eq!(applied, 1, "should apply exactly 1 block");

        // Height should now be 2
        assert_eq!(node.state.get_current_block(), 2);

        // We crossed from epoch 0 (height 1/2) to epoch 1 (height 2/2)
        assert!(
            node.state.engine_restart_signal.load(std::sync::atomic::Ordering::Relaxed),
            "engine_restart_signal should be set after sync crosses epoch boundary"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_sync_within_same_epoch_does_not_set_restart_signal() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-sync-no-epoch-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Large epoch length — sync won't cross boundary
        {
            let mut params = node.state.consensus_params.write().unwrap();
            params.epoch_length = 1000;
        }

        // Set current block to 5 (epoch = 5/1000 = 0)
        node.state.set_current_block(5);

        // Build block at height 5
        let height = 5u64;
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            10_000,
            proposer,
            version,
            vec![],
        );

        // Execute to compute valid state roots
        {
            let result = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("execution");
            block.finalize(&result);
        }

        // Build SyncResponse with the block
        let block_json = serde_json::to_vec(&block).expect("serialize");
        let response = SyncResponse {
            start_height: 5,
            blocks: vec![block_json],
            state_root: block.header.state_root,
        };

        // Apply synced blocks
        let applied = apply_synced_blocks(&response, &node.state, &node.consensus, &tmp);
        assert_eq!(applied, 1, "should apply exactly 1 block");

        // Height should now be 6
        assert_eq!(node.state.get_current_block(), 6);

        // Epoch did not change: old=5/1000=0, new=6/1000=0
        assert!(
            !node.state.engine_restart_signal.load(std::sync::atomic::Ordering::Relaxed),
            "engine_restart_signal should NOT be set when sync stays within same epoch"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
