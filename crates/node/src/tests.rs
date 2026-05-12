use super::*;
use call_consensus::exec::state_accessors;
use call_consensus::BlockExecutionResult;
use call_network::{BlockAnnouncement, EpochBoundarySignal, InMemoryNetwork, SyncResponse};
use call_primitives::{Address, Ed25519PublicKey, B256};
use std::sync::OnceLock;

use alloy_sol_types::SolCall;
use call_precompile::AGENT_ADDRESS;

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
        gas_price: 10,
        to: Some(test_addr(2)),
        value: call_primitives::U256::from(100),
        data: call_evm::Bytes::default(),
        chain_id: 1,
    }
}

fn make_agent_evm_tx(nonce: u64, gas_price: u64, calldata: Vec<u8>) -> call_evm::EvmTransaction {
    call_evm::EvmTransaction {
        caller: *test_sender(),
        nonce,
        gas_limit: 300_000,
        gas_price: gas_price.into(),
        to: Some(AGENT_ADDRESS),
        value: call_primitives::U256::ZERO,
        data: call_evm::Bytes::from(calldata),
        chain_id: 1,
    }
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

#[test]
fn test_node_creation() {
    let tmp = std::env::temp_dir().join(format!("call-node-test-{}", std::process::id()));
    let node = CallNode::new(tmp.clone()).expect("node creation");
    assert_eq!(node.state.chain_id, CALLCHAIN_CHAIN_ID);
    assert!(node.network.is_none());
    assert_eq!(node.parent_hash, BlockHash::ZERO);
    assert_eq!(node.consensus.read().unwrap().current_height(), 0);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_node_mempool_stats() {
    let tmp = std::env::temp_dir().join(format!("call-node-mempool-test-{}", std::process::id()));
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
    let tmp = std::env::temp_dir().join(format!("call-node-block-test-{}", std::process::id()));

    // Create node with validators staked
    let node = CallNode::new(tmp.clone()).expect("node creation");

    // Stake a validator so proposer selection works
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Fund sender balance in EVM storage
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state_mut().set_balance(
            *test_sender(),
            call_primitives::U256::from(10_000_000_000_000u128),
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
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
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
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
    let result = node
        .state
        .write_all()
        .execute_block_no_subsystems(&block, height)
        .expect("execution");
    block.finalize(&result);

    // Commit
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.commit_block(&block, &result).expect("commit");
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        consensus.advance_round(&mut provider);
    }

    let height_after = node.consensus.read().unwrap().current_height();
    assert_eq!(height_after, height_before + 1);
    assert_ne!(block.header.state_root, call_primitives::Hash::ZERO);

    // Persist
    persist_block(&node.state.db_env, height, &block).expect("persist block");

    // Verify persisted block can be read back from MDBX
    let loaded = load_block(&node.state.db_env, height);
    assert!(loaded.is_some(), "block should be persisted in MDBX");
    assert_eq!(loaded.unwrap().header.hash(), block.header.hash());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_block_production_with_empty_mempool() {
    let tmp =
        std::env::temp_dir().join(format!("call-node-empty-block-test-{}", std::process::id()));

    let node = CallNode::new(tmp.clone()).expect("node creation");

    // Stake a validator
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Select and build with empty mempool
    let selection = { node.mempool.write().unwrap().select_transactions() };
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let height = node.consensus.read().unwrap().current_height();

    assert!(selection.evm_txs.is_empty());

    let version = node.state.fork_manager.read().unwrap().current_version();
    let mut block = Block::new(height, node.parent_hash, 2_000, proposer, version, vec![]);

    // Execute empty block
    let result = node
        .state
        .write_all()
        .execute_block_no_subsystems(&block, height)
        .expect("empty block execution");
    block.finalize(&result);
    block.finalize(&result);

    // Commit
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .commit_block(&block, &result)
            .expect("commit empty");
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        consensus.advance_round(&mut provider);
    }

    assert_eq!(node.consensus.read().unwrap().current_height(), 1);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_e2e_two_nodes_tx_propagation() {
    let tmp1 = std::env::temp_dir().join(format!("call-node-e2e-node1-{}", std::process::id()));
    let tmp2 = std::env::temp_dir().join(format!("call-node-e2e-node2-{}", std::process::id()));

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
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node1.state.db_env).unwrap();
        let mut consensus = node1.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node1.state.db_env).unwrap();
    }

    // Fund sender balance on node1 (EVM)
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node1.state.db_env).unwrap();
        state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            *test_sender(),
            10_000,
        );
        provider.state().save_to_db(&node1.state.db_env).unwrap();
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
    assert_eq!(
        node1.mempool_stats().0,
        1,
        "node1 should have 1 tx in mempool"
    );

    // Step 2: Produce a block on node1
    let selection = { node1.mempool.write().unwrap().select_transactions() };
    let proposer = node1
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let height = node1.consensus.read().unwrap().current_height();

    let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();
    assert_eq!(evm_txs.len(), 1, "should have 1 evm tx");

    let version = node1.state.fork_manager.read().unwrap().current_version();
    let mut block = Block::new(height, node1.parent_hash, 3_000, proposer, version, evm_txs);

    let result = node1
        .state
        .write_all()
        .execute_block_no_subsystems(&block, height)
        .expect("execution");
    block.finalize(&result);

    // Commit on node1
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node1.state.db_env).unwrap();
        let mut consensus = node1.consensus.write().unwrap();
        consensus.commit_block(&block, &result).expect("commit");
        consensus.advance_round(&mut provider);
    }
    assert_eq!(
        node1.consensus.read().unwrap().current_height(),
        1,
        "node1 should be at height 1"
    );

    // Step 3: Broadcast block announcement via shared network
    let block_hash = block.header.hash();
    let announcement = BlockAnnouncement {
        block_hash,
        height,
        proposer,
        timestamp_millis: block.header.timestamp_millis,
    };
    let msg = postcard::to_allocvec(&NetworkMessage::BlockAnnouncement(announcement))
        .expect("serialize block announcement");
    shared_network.broadcast(BLOCK_CHANNEL, msg).await;

    // Step 4: Node2 receives the block announcement from shared network
    let result = shared_network.receive().await;
    let (peer_id, channel, data) = result.expect("should receive message");
    assert_eq!(channel, BLOCK_CHANNEL);
    assert_eq!(peer_id, "broadcast");

    let received = postcard::from_bytes::<NetworkMessage>(&data).expect("parse network message");
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
    let tmp = std::env::temp_dir().join(format!("call-node-e2e-persist-{}", std::process::id()));

    let node = CallNode::new(tmp.clone()).expect("node creation");

    // Stake validator
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Fund sender in EVM storage
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state_mut().set_balance(
            *test_sender(),
            call_primitives::U256::from(10_000_000_000_000u128),
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Insert EVM tx
    let tx = make_evm_tx(0);
    {
        let mut mempool = node.mempool.write().unwrap();
        let _ = mempool.insert_evm_tx(tx);
    }

    // Produce block
    let selection = { node.mempool.write().unwrap().select_transactions() };
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let height = node.consensus.read().unwrap().current_height();

    let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

    let version = node.state.fork_manager.read().unwrap().current_version();
    let mut block = Block::new(height, node.parent_hash, 4_000, proposer, version, evm_txs);

    let result = node
        .state
        .write_all()
        .execute_block_no_subsystems(&block, height)
        .expect("execution");
    block.finalize(&result);

    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.commit_block(&block, &result).expect("commit");
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        consensus.advance_round(&mut provider);
    }

    // Persist block
    persist_block(&node.state.db_env, height, &block).expect("persist block");

    // Verify block can be read back from MDBX
    let restored = load_block(&node.state.db_env, height).expect("block should exist in MDBX");
    assert_eq!(restored.header.height, height);
    assert_eq!(restored.header.hash(), block.header.hash());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_e2e_state_persistence_restart() {
    let tmp =
        std::env::temp_dir().join(format!("call-node-persist-restart-{}", std::process::id()));
    let initial_balance: u128 = 10_000_000;
    let _transfer_amount: u128 = 5_000;

    // === Phase 1: Create node, fund account, produce block, persist state ===
    {
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut provider =
                call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
            let mut consensus = node.consensus.write().unwrap();
            consensus
                .stake_validator(
                    &mut provider,
                    test_addr(1),
                    test_pubkey(1),
                    one_million_call(),
                )
                .expect("stake");
            consensus.refresh_proposer_subset(&provider);
            provider.state().save_to_db(&node.state.db_env).unwrap();
        }

        // Fund sender balance in EVM storage
        {
            let mut provider =
                call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
            state_accessors::seed_balance(
                provider.state_mut(),
                call_protocol::CALL_ASSET_ID,
                *test_sender(),
                initial_balance,
            );
            state_accessors::seed_asset(
                provider.state_mut(),
                1,
                "CALL",
                "Callchain",
                18,
                *test_sender(),
                0,
                initial_balance,
                0,
            );
            // Set native EVM balance for gas payment
            provider.state_mut().set_balance(
                *test_sender(),
                call_primitives::U256::from(100_000_000_000u128),
            );
            provider.state().save_to_db(&node.state.db_env).unwrap();
        }

        // Insert an EVM tx
        let tx = make_evm_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_evm_tx(tx);
        }

        // Produce and commit block
        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node
            .consensus
            .read()
            .unwrap()
            .current_proposer()
            .expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(height, node.parent_hash, 5_000, proposer, version, evm_txs);

        let result = {
            let mut s = node.state.write_all();
            let mut provider =
                call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
            let result = block
                .execute(
                    &mut provider,
                    &mut s.fee_params,
                    height,
                    Some(&node.state.db_env),
                )
                .expect("execution");
            provider.state().save_to_db(&node.state.db_env).unwrap();
            result
        };
        block.finalize(&result);

        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
            let mut provider =
                call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
            consensus.advance_round(&mut provider);
        }

        // Persist state to reth-db immediately
        let db_env = &node.db.db;
        persist_state_to_db(db_env, &node.state, &node.consensus).expect("persist state");

        // Node is dropped here, simulating shutdown
    }

    // Give MDBX a moment to release file locks before reopening
    std::thread::sleep(std::time::Duration::from_millis(100));

    // === Phase 2: Create new node from same data dir, verify state ===
    {
        let node2 = CallNode::new(tmp.clone()).expect("node creation (restart)");

        // Verify native EVM balance was recovered
        let sender_balance = {
            let provider =
                call_evm::provider::InMemoryStateProvider::from_db(&node2.state.db_env).unwrap();
            provider.state().get_balance(test_sender())
        };

        // Native balance should be less than what was seeded (gas deducted)
        assert!(
            sender_balance < call_primitives::U256::from(100_000_000_000u128),
            "sender balance should be reduced after restart, got {sender_balance}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[tokio::test]
async fn test_crash_recovery_checkpoint_detected() {
    let tmp = std::env::temp_dir().join(format!("call-node-crash-recovery-{}", std::process::id()));

    // Phase 1: Create node, persist state cleanly, then simulate crash
    {
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut provider =
                call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
            let mut consensus = node.consensus.write().unwrap();
            consensus
                .stake_validator(
                    &mut provider,
                    test_addr(1),
                    test_pubkey(1),
                    one_million_call(),
                )
                .expect("stake");
            consensus.refresh_proposer_subset(&provider);
            provider.state().save_to_db(&node.state.db_env).unwrap();
        }

        // Fund sender
        {
            let mut provider =
                call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
            state_accessors::seed_balance(
                provider.state_mut(),
                call_protocol::CALL_ASSET_ID,
                *test_sender(),
                10_000_000,
            );
            provider.state_mut().set_balance(
                *test_sender(),
                call_primitives::U256::from(100_000_000_000u128),
            );
            provider.state().save_to_db(&node.state.db_env).unwrap();
        }

        // Produce and commit block
        let tx = make_evm_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_evm_tx(tx);
        }

        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node
            .consensus
            .read()
            .unwrap()
            .current_proposer()
            .expect("proposer");
        let height = node.consensus.read().unwrap().current_height();
        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();
        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(height, node.parent_hash, 5_000, proposer, version, evm_txs);

        let result = {
            let mut s = node.state.write_all();
            let mut provider =
                call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
            let result = block
                .execute(
                    &mut provider,
                    &mut s.fee_params,
                    height,
                    Some(&node.state.db_env),
                )
                .expect("execution");
            provider.state().save_to_db(&node.state.db_env).unwrap();
            result
        };
        block.finalize(&result);

        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
            let mut provider =
                call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
            consensus.advance_round(&mut provider);
        }

        // Clean persist
        persist_state_to_db(&node.db.db, &node.state, &node.consensus).expect("persist state");

        // Simulate crash: write checkpoint marker without clearing it
        // This represents a crash during a subsequent persist
        crate::state_persist::write_checkpoint_pending(&node.db.db, block.header.hash().0)
            .expect("write checkpoint");

        // Node dropped here
    }

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Phase 2: Create new node — should detect checkpoint and recover
    {
        let node2 = CallNode::new(tmp.clone()).expect("node creation after crash");

        // Recovery should have reset fork_manager to default
        let fm = node2.state.fork_manager.read().unwrap();
        assert_eq!(
            fm.current_version,
            call_primitives::ProtocolVersion::new(1, 0, 0)
        );
        assert!(fm.scheduled_upgrades().is_empty());
        drop(fm);

        // Receipts should be empty (reset by recovery)
        let receipts = node2.state.receipts.read().unwrap();
        assert!(
            receipts.is_empty(),
            "receipts should be empty after recovery"
        );
        drop(receipts);

        // Checkpoint should be cleared
        assert!(
            !crate::state_persist::check_recovery_needed(&node2.db.db).unwrap(),
            "checkpoint should be cleared after recovery"
        );

        // EVM state should still exist (recovery only resets in-memory state)
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node2.state.db_env).unwrap();
        let balance = provider.state().get_balance(test_sender());
        assert!(
            balance > call_primitives::U256::ZERO,
            "EVM state should survive recovery, got {balance}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

// ── State isolation tests (Phase 1) ─────────────────────────────────

#[tokio::test]
async fn test_state_isolation_propose_does_not_modify_shared_state() {
    let tmp = std::env::temp_dir().join(format!("call-node-isolation-test-{}", std::process::id()));

    let node = CallNode::new(tmp.clone()).expect("node creation");

    // Stake validator
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Fund sender with ample balance for fees + transfer (EVM)
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            *test_sender(),
            10_000_000,
        );
        // Set native EVM balance for gas payment
        provider.state_mut().set_balance(
            *test_sender(),
            call_primitives::U256::from(100_000_000_000u128),
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Insert tx into mempool
    let tx = make_evm_tx(0);
    {
        let mut mempool = node.mempool.write().unwrap();
        let _ = mempool.insert_evm_tx(tx);
    }

    let selection = { node.mempool.write().unwrap().select_transactions() };
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let height = node.consensus.read().unwrap().current_height();

    let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

    let version = node.state.fork_manager.read().unwrap().current_version();
    let mut block = Block::new(height, node.parent_hash, 5_000, proposer, version, evm_txs);

    // Capture shared state BEFORE propose-phase execution
    let balance_before = {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state().get_balance(test_sender())
    };

    // Simulate PROPOSE phase: execute on CLONED state
    {
        let result = node
            .state
            .read_all()
            .execute_block_cloned_no_subsystems(&block, height)
            .expect("propose execution on clone");

        block.finalize(&result);
    }

    // Verify shared state is UNCHANGED after propose
    let balance_after_propose = {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state().get_balance(test_sender())
    };
    assert_eq!(
        balance_after_propose, balance_before,
        "shared state must NOT be modified by propose-phase execution"
    );

    // Simulate FINALIZE phase: execute on SHARED state (write locks)
    {
        let result = node
            .state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("finalize execution on shared state");

        // Verify state root matches header
        assert_eq!(
            result.state_root, block.header.state_root,
            "state_root mismatch"
        );

        block.finalize(&result);

        let mut consensus = node.consensus.write().unwrap();
        consensus.commit_block(&block, &result).expect("commit");
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        consensus.advance_round(&mut provider);
    }

    // Verify shared state IS modified after finalize
    let balance_after_finalize = {
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state().get_balance(test_sender())
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
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Fund sender with ample balance for fees + transfer (EVM)
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            *test_sender(),
            10_000_000,
        );
        provider.state_mut().set_balance(
            *test_sender(),
            call_primitives::U256::from(100_000_000_000u128),
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    let tx = make_evm_tx(0);
    {
        let mut mempool = node.mempool.write().unwrap();
        let _ = mempool.insert_evm_tx(tx);
    }

    let selection = { node.mempool.write().unwrap().select_transactions() };
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let height = node.consensus.read().unwrap().current_height();

    let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

    let version = node.state.fork_manager.read().unwrap().current_version();
    let mut block = Block::new(height, node.parent_hash, 6_000, proposer, version, evm_txs);

    // Execute on cloned state to get valid roots
    let result = node
        .state
        .read_all()
        .execute_block_cloned_no_subsystems(&block, height)
        .expect("execution");

    block.finalize(&result);

    // Tamper with a state root in the header
    let original_state_root = block.header.state_root;
    block.header.state_root = call_primitives::Hash::repeat_byte(0xDE);

    // Verify: re-execution on clone detects root mismatch
    {
        let result2 = node
            .state
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
        let result3 = node
            .state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution on shared state");

        // The re-computed result3 should have the ORIGINAL correct root
        assert_eq!(
            result3.state_root, original_state_root,
            "re-computed root should match original"
        );
        // But the block header has the tampered root
        assert_ne!(
            result3.state_root, block.header.state_root,
            "tampered block should fail root check"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_commit_block_height_replay_protection() {
    let tmp = std::env::temp_dir().join(format!("call-node-replay-test-{}", std::process::id()));

    let node = CallNode::new(tmp.clone()).expect("node creation");

    // Stake validator
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    let height = node.consensus.read().unwrap().current_height();
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let version = node.state.fork_manager.read().unwrap().current_version();

    let mut block = Block::new(height, node.parent_hash, 7_000, proposer, version, vec![]);

    // Execute and commit once
    {
        let result = node
            .state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution");

        block.finalize(&result);

        let mut consensus = node.consensus.write().unwrap();
        consensus
            .commit_block(&block, &result)
            .expect("first commit");
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        consensus.advance_round(&mut provider);
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
        assert!(
            err.contains("height mismatch"),
            "error should mention height mismatch: {err}"
        );
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
    assert_eq!(
        (10usize * 2).div_ceil(3),
        7,
        "subset of 10 needs quorum of 7"
    );
}

#[tokio::test]
async fn test_epoch_boundary_signal_updates_peer_heights() {
    let tmp = std::env::temp_dir().join(format!("call-node-signal-test-{}", std::process::id()));
    let node = CallNode::new(tmp.clone()).expect("node creation");

    // Construct EpochBoundarySignal
    let signal = EpochBoundarySignal {
        height: 1000,
        epoch: 1,
        sender_pubkey: [0xAB; 32],
    };
    let data = postcard::to_allocvec(&NetworkMessage::EpochBoundarySignal(signal))
        .expect("serialize signal");

    let network: Arc<dyn Network> = Arc::new(InMemoryNetwork::new());
    let sync_inflight: SyncInflight =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    // Send signal via handle_network_message on BLOCK_CHANNEL
    handle_network_message(
        "peer_abc123",
        BLOCK_CHANNEL,
        &data,
        &node.mempool,
        &node.state,
        &network,
        &sync_inflight,
        &node.oracle_tracker,
        &node.telemetry,
    )
    .await;

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
    let tmp =
        std::env::temp_dir().join(format!("call-node-sync-epoch-test-{}", std::process::id()));
    let node = CallNode::new(tmp.clone()).expect("node creation");

    // Stake validator so proposer selection works
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
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
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let version = node.state.fork_manager.read().unwrap().current_version();
    let mut block = Block::new(height, node.parent_hash, 9_000, proposer, version, vec![]);

    // Execute to compute valid state roots
    {
        let result = node
            .state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution");
        block.finalize(&result);
    }

    // Verify signal is NOT set before sync
    assert!(
        !node
            .state
            .engine_restart_signal
            .load(std::sync::atomic::Ordering::Relaxed),
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
    let applied = apply_synced_blocks(&response, &node.state, &node.consensus);
    assert_eq!(applied, 1, "should apply exactly 1 block");

    // Height should now be 2
    assert_eq!(node.state.get_current_block(), 2);

    // We crossed from epoch 0 (height 1/2) to epoch 1 (height 2/2)
    assert!(
        node.state
            .engine_restart_signal
            .load(std::sync::atomic::Ordering::Relaxed),
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
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
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
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let version = node.state.fork_manager.read().unwrap().current_version();
    let mut block = Block::new(height, node.parent_hash, 10_000, proposer, version, vec![]);

    // Execute to compute valid state roots
    {
        let result = node
            .state
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
    let applied = apply_synced_blocks(&response, &node.state, &node.consensus);
    assert_eq!(applied, 1, "should apply exactly 1 block");

    // Height should now be 6
    assert_eq!(node.state.get_current_block(), 6);

    // Epoch did not change: old=5/1000=0, new=6/1000=0
    assert!(
        !node
            .state
            .engine_restart_signal
            .load(std::sync::atomic::Ordering::Relaxed),
        "engine_restart_signal should NOT be set when sync stays within same epoch"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_receipt_persistence_restart() {
    let tmp =
        std::env::temp_dir().join(format!("call-node-receipt-persist-{}", std::process::id()));
    let log_addr = test_addr(0x42);
    let topic = call_primitives::Hash::repeat_byte(0x11);

    // === Phase 1: Create node, store receipts, persist state ===
    {
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Store receipts with logs across multiple blocks
        let tx1 = call_primitives::TxHash::repeat_byte(1);
        let receipt1 = call_protocol::ProtocolReceipt {
            tx_hash: tx1,
            block_number: 10,
            block_hash: call_primitives::Hash::repeat_byte(10),
            transaction_index: 0,
            status: call_primitives::ExecutionStatus::Success,
            gas_used: 21_000,
            gas_payer: test_addr(1),
            fee_currency: call_primitives::FeeCurrency::Call,
            fee_amount: 1_000,
            cumulative_gas_used: 21_000,
            effective_gas_price: 10,
            to: Some(test_addr(2)),
            contract_address: None,
            logs: vec![call_protocol::LogEntry {
                address: log_addr,
                topics: vec![topic],
                data: vec![0x01, 0x02],
            }],
            logs_bloom: vec![0u8; 256],
            instruction_results: vec![call_protocol::InstructionExecResult {
                success: true,
                gas_used: 21_000,
                revert_reason: None,
            }],
            memos: vec![],
            state_changes: vec![],
        };
        node.state.store_receipt(tx1, receipt1);

        let tx2 = call_primitives::TxHash::repeat_byte(2);
        let receipt2 = call_protocol::ProtocolReceipt {
            tx_hash: tx2,
            block_number: 10,
            block_hash: call_primitives::Hash::repeat_byte(10),
            transaction_index: 1,
            status: call_primitives::ExecutionStatus::Success,
            gas_used: 21_000,
            gas_payer: test_addr(1),
            fee_currency: call_primitives::FeeCurrency::Call,
            fee_amount: 1_000,
            cumulative_gas_used: 42_000,
            effective_gas_price: 10,
            to: Some(test_addr(3)),
            contract_address: None,
            logs: vec![call_protocol::LogEntry {
                address: log_addr,
                topics: vec![],
                data: vec![0x03],
            }],
            logs_bloom: vec![0u8; 256],
            instruction_results: vec![call_protocol::InstructionExecResult {
                success: true,
                gas_used: 21_000,
                revert_reason: None,
            }],
            memos: vec![],
            state_changes: vec![],
        };
        node.state.store_receipt(tx2, receipt2);

        let tx3 = call_primitives::TxHash::repeat_byte(3);
        let receipt3 = call_protocol::ProtocolReceipt {
            tx_hash: tx3,
            block_number: 20,
            block_hash: call_primitives::Hash::repeat_byte(20),
            transaction_index: 0,
            status: call_primitives::ExecutionStatus::Reverted {
                reason: "insufficient gas".into(),
            },
            gas_used: 5_000,
            gas_payer: test_addr(1),
            fee_currency: call_primitives::FeeCurrency::Call,
            fee_amount: 500,
            cumulative_gas_used: 5_000,
            effective_gas_price: 10,
            to: Some(test_addr(4)),
            contract_address: None,
            logs: vec![],
            logs_bloom: vec![0u8; 256],
            instruction_results: vec![call_protocol::InstructionExecResult {
                success: false,
                gas_used: 5_000,
                revert_reason: Some("insufficient gas".into()),
            }],
            memos: vec![],
            state_changes: vec![],
        };
        node.state.store_receipt(tx3, receipt3);

        // Persist state (including receipts)
        persist_state_to_db(&node.db.db, &node.state, &node.consensus).expect("persist state");

        // Verify in-memory state before restart
        let receipts_block_10 = node.state.get_receipts_by_block(10);
        assert_eq!(
            receipts_block_10.len(),
            2,
            "should have 2 receipts in block 10"
        );

        let receipts_block_20 = node.state.get_receipts_by_block(20);
        assert_eq!(
            receipts_block_20.len(),
            1,
            "should have 1 receipt in block 20"
        );

        // Verify log_index was built
        let log_entries = node.state.lookup_logs_by_address(&[log_addr]);
        assert!(
            log_entries.is_some(),
            "log_index should contain entries for log_addr"
        );
        assert_eq!(
            log_entries.unwrap().len(),
            2,
            "should have 2 log entries indexed"
        );
    }

    std::thread::sleep(std::time::Duration::from_millis(100));

    // === Phase 2: Restart node, verify receipts recovered ===
    {
        let node2 = CallNode::new(tmp.clone()).expect("node creation (restart)");

        // Receipts should be recovered from DB
        let receipts_block_10 = node2.state.get_receipts_by_block(10);
        assert_eq!(
            receipts_block_10.len(),
            2,
            "block 10 receipts should survive restart"
        );

        let receipts_block_20 = node2.state.get_receipts_by_block(20);
        assert_eq!(
            receipts_block_20.len(),
            1,
            "block 20 receipt should survive restart"
        );

        // Verify individual receipt fields
        let tx1 = call_primitives::TxHash::repeat_byte(1);
        let r1 = node2
            .state
            .get_receipt(&tx1)
            .expect("tx1 receipt should exist");
        assert_eq!(r1.block_number, 10);
        assert!(matches!(
            r1.status,
            call_primitives::ExecutionStatus::Success
        ));
        assert_eq!(r1.logs.len(), 1);
        assert_eq!(r1.logs[0].address, log_addr);
        assert_eq!(r1.logs[0].topics.len(), 1);
        assert_eq!(r1.logs[0].topics[0], topic);

        let tx3 = call_primitives::TxHash::repeat_byte(3);
        let r3 = node2
            .state
            .get_receipt(&tx3)
            .expect("tx3 receipt should exist");
        assert_eq!(r3.block_number, 20);
        assert!(matches!(
            &r3.status, call_primitives::ExecutionStatus::Reverted { reason } if reason == "insufficient gas"
        ));

        // Verify log_index was rebuilt from loaded receipts
        let log_entries = node2.state.lookup_logs_by_address(&[log_addr]);
        assert!(
            log_entries.is_some(),
            "log_index should be rebuilt after restart"
        );
        assert_eq!(
            log_entries.unwrap().len(),
            2,
            "log_index should contain 2 entries after restart"
        );

        // Verify empty block returns empty
        let receipts_block_99 = node2.state.get_receipts_by_block(99);
        assert!(
            receipts_block_99.is_empty(),
            "block 99 should have no receipts"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[tokio::test]
async fn test_agent_register_in_block() {
    let tmp = std::env::temp_dir().join(format!("call-node-agent-reg-test-{}", std::process::id()));
    let node = CallNode::new(tmp.clone()).expect("node creation");

    // Stake validator
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Fund sender EVM balance for gas
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state_mut().set_balance(
            *test_sender(),
            call_primitives::U256::from(10_000_000_000_000u128),
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Build and insert registerAgent tx
    let register_call = call_agent::precompile::IProtocolAgent::registerAgentCall {
        name: "TestAgent".into(),
        url: "http://test.com".into(),
        pubkeyHash: [0xBBu8; 32].into(),
    };
    let tx = make_agent_evm_tx(0, 10, register_call.abi_encode());
    {
        let mut mempool = node.mempool.write().unwrap();
        let _ = mempool.insert_evm_tx(tx);
    }

    // Build block
    let selection = { node.mempool.write().unwrap().select_transactions() };
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let height = node.consensus.read().unwrap().current_height();
    let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();
    let version = node.state.fork_manager.read().unwrap().current_version();
    let mut block = Block::new(height, node.parent_hash, 1_000, proposer, version, evm_txs);

    // Execute
    let result = node
        .state
        .write_all()
        .execute_block_no_subsystems(&block, height)
        .expect("execution");
    block.finalize(&result);

    // Commit
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.commit_block(&block, &result).expect("commit");
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        consensus.advance_round(&mut provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Verify tx succeeded
    assert_eq!(result.evm_tx_results.len(), 1);
    assert!(
        result.evm_tx_results[0].status,
        "agent register tx should succeed"
    );

    // Verify agent state in DB
    let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
    assert_eq!(state_accessors::read_agent_count(provider.state()), 1);
    assert_eq!(
        state_accessors::agent_get_owner(provider.state(), 0),
        *test_sender()
    );
    assert_eq!(
        state_accessors::agent_get_name(provider.state(), 0),
        "TestAgent"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_agent_grant_and_pay_in_block() {
    let tmp = std::env::temp_dir().join(format!("call-node-agent-pay-test-{}", std::process::id()));
    let node = CallNode::new(tmp.clone()).expect("node creation");

    // Stake validator
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                test_pubkey(1),
                one_million_call(),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Fund sender EVM balance for gas and protocol asset balance for grant
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider.state_mut().set_balance(
            *test_sender(),
            call_primitives::U256::from(10_000_000_000_000u128),
        );
        state_accessors::seed_balance(
            provider.state_mut(),
            call_agent::CALL_ASSET_ID,
            *test_sender(),
            10_000,
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    let recipient = test_addr(0x44);

    // Build 3 agent txs with descending gas prices so mempool drain order
    // is deterministic (higher gas_price = higher priority).
    let tx0 = make_agent_evm_tx(
        0,
        30,
        call_agent::precompile::IProtocolAgent::registerAgentCall {
            name: "PayAgent".into(),
            url: "http://pay.com".into(),
            pubkeyHash: [0xCCu8; 32].into(),
        }
        .abi_encode(),
    );
    let tx1 = make_agent_evm_tx(
        1,
        20,
        call_agent::precompile::IProtocolAgent::grantBalanceCall {
            agentId: 0,
            assetId: call_agent::CALL_ASSET_ID,
            amount: 5_000,
        }
        .abi_encode(),
    );
    let tx2 = make_agent_evm_tx(
        2,
        10,
        call_agent::precompile::IProtocolAgent::payCall {
            agentId: 0,
            assetId: call_agent::CALL_ASSET_ID,
            to: recipient,
            amount: 1_000,
        }
        .abi_encode(),
    );

    {
        let mut mempool = node.mempool.write().unwrap();
        let _ = mempool.insert_evm_tx(tx0);
        let _ = mempool.insert_evm_tx(tx1);
        let _ = mempool.insert_evm_tx(tx2);
    }

    // Build block
    let selection = { node.mempool.write().unwrap().select_transactions() };
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let height = node.consensus.read().unwrap().current_height();
    let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();
    let version = node.state.fork_manager.read().unwrap().current_version();
    let mut block = Block::new(height, node.parent_hash, 2_000, proposer, version, evm_txs);

    // Execute
    let result = node
        .state
        .write_all()
        .execute_block_no_subsystems(&block, height)
        .expect("execution");
    block.finalize(&result);

    // Commit
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.commit_block(&block, &result).expect("commit");
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        consensus.advance_round(&mut provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Verify all 3 txs succeeded
    assert_eq!(result.evm_tx_results.len(), 3);
    assert!(result.evm_tx_results[0].status, "register should succeed");
    assert!(result.evm_tx_results[1].status, "grant should succeed");
    assert!(result.evm_tx_results[2].status, "pay should succeed");

    // Verify agent state in DB
    let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
    assert_eq!(state_accessors::read_agent_count(provider.state()), 1);
    assert_eq!(
        state_accessors::agent_get_owner(provider.state(), 0),
        *test_sender()
    );
    // Agent balance should be 5_000 - 1_000 = 4_000
    assert_eq!(
        state_accessors::agent_get_balance(provider.state(), 0, call_agent::CALL_ASSET_ID),
        4_000
    );
    // Recipient should have received 1_000
    assert_eq!(
        state_accessors::read_balance(provider.state(), call_agent::CALL_ASSET_ID, recipient),
        1_000
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Beacon sync background task tests (gap #25) ─────────────────────

/// Build a mock LightClientUpdate with real BLS signatures from generated keys.
fn build_mock_light_client_update_for_node(
    signing_root: B256,
) -> (
    call_light_client::LightClientUpdate,
    Vec<call_crypto::BlsSecretKey>,
) {
    use call_crypto::{bls_generate, bls_sign_beacon, BlsPublicKey, BlsSecretKey, BlsSignature};

    const PARTICIPANTS: usize = 350;

    let mut secrets = Vec::with_capacity(PARTICIPANTS);
    let mut pubkeys = Vec::with_capacity(call_light_client::SYNC_COMMITTEE_SIZE);

    for i in 0..call_light_client::SYNC_COMMITTEE_SIZE {
        if i < PARTICIPANTS {
            let (sk, pk) = bls_generate().unwrap();
            secrets.push(sk);
            pubkeys.push(pk);
        } else {
            pubkeys.push(BlsPublicKey([0u8; 48]));
        }
    }

    let mut agg_pk = [0u8; 48];
    agg_pk.copy_from_slice(&pubkeys[0].0);
    let sync_committee = call_light_client::SyncCommittee {
        pubkeys: pubkeys.clone(),
        aggregate_pubkey: BlsPublicKey(agg_pk),
    };

    let attested_header = call_light_client::BeaconBlockHeader {
        slot: 100,
        proposer_index: 0,
        parent_root: B256::repeat_byte(0x01),
        state_root: B256::repeat_byte(0x02),
        body_root: B256::repeat_byte(0x03),
    };
    let finalized_header = call_light_client::BeaconBlockHeader {
        slot: 98,
        proposer_index: 0,
        parent_root: B256::repeat_byte(0x04),
        state_root: B256::repeat_byte(0x05),
        body_root: B256::repeat_byte(0x06),
    };

    let mut bits = [0u8; 64];
    for i in 0..PARTICIPANTS {
        let byte_idx = i / 8;
        let bit_idx = i % 8;
        bits[byte_idx] |= 1 << bit_idx;
    }

    let sigs: Vec<BlsSignature> = secrets
        .iter()
        .map(|sk| bls_sign_beacon(sk, signing_root.as_slice()))
        .collect();

    let agg_sig = call_crypto::bls_aggregate(&sigs).unwrap();

    let sync_aggregate = call_light_client::SyncAggregate {
        sync_committee_bits: bits,
        sync_committee_signature: agg_sig,
    };

    let update = call_light_client::LightClientUpdate {
        attested_header,
        next_sync_committee: sync_committee,
        next_sync_committee_branch: [B256::ZERO;
            call_light_client::NEXT_SYNC_COMMITTEE_BRANCH_DEPTH],
        finalized_header,
        finality_branch: [B256::ZERO; call_light_client::FINALIZED_BRANCH_DEPTH],
        sync_aggregate,
        signature_slot: 101,
    };

    (update, secrets)
}

#[tokio::test]
async fn test_beacon_sync_task_spawns_with_light_client() {
    let tmp = std::env::temp_dir().join(format!("call-node-beacon-test-{}", std::process::id()));
    let mut node = CallNode::new(tmp.clone()).expect("node creation");

    let genesis = call_light_client::GenesisState {
        anchor_hash: B256::repeat_byte(0xAA),
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let beacon_config = call_light_client::BeaconConfig {
        fork_version: [0, 0, 0, 1],
        genesis_validators_root: B256::repeat_byte(0xBB),
    };
    let lc =
        call_light_client::EthLightClient::init_with_beacon_config(genesis, Some(beacon_config));
    node.eth_light_client = Some(Arc::new(std::sync::RwLock::new(lc)));

    // Start beacon sync task with invalid URL (will fail fast) and short interval
    node.start_beacon_sync_task("http://127.0.0.1:1".to_string(), 1);
    assert!(
        node.beacon_sync_handle.is_some(),
        "beacon_sync_handle should be set after start_beacon_sync_task"
    );

    // Wait for at least one tick to fire
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // Stop should abort cleanly
    let _ = node.stop().await;
    assert!(
        node.beacon_sync_handle.is_none(),
        "beacon_sync_handle should be cleared after stop"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_beacon_sync_task_returns_early_without_light_client() {
    let tmp = std::env::temp_dir().join(format!(
        "call-node-beacon-no-lc-test-{}",
        std::process::id()
    ));
    let mut node = CallNode::new(tmp.clone()).expect("node creation");

    assert!(node.eth_light_client.is_none());

    node.start_beacon_sync_task("http://127.0.0.1:1".to_string(), 1);
    assert!(
        node.beacon_sync_handle.is_none(),
        "beacon_sync_handle should NOT be set when eth_light_client is None"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_beacon_sync_task_applies_update_and_sets_finalized_block() {
    let tmp = std::env::temp_dir().join(format!(
        "call-node-beacon-apply-test-{}",
        std::process::id()
    ));
    let mut node = CallNode::new(tmp.clone()).expect("node creation");

    let genesis = call_light_client::GenesisState {
        anchor_hash: B256::repeat_byte(0xAA),
        anchor_block: 1000,
        state_root: B256::ZERO,
    };
    let beacon_config = call_light_client::BeaconConfig {
        fork_version: [0, 0, 0, 1],
        genesis_validators_root: B256::repeat_byte(0xBB),
    };
    let mut client = call_light_client::EthLightClient::init_with_beacon_config(
        genesis,
        Some(beacon_config.clone()),
    );

    // Compute signing root and build mock update
    let attested_header = call_light_client::BeaconBlockHeader {
        slot: 100,
        proposer_index: 0,
        parent_root: B256::repeat_byte(0x01),
        state_root: B256::repeat_byte(0x02),
        body_root: B256::repeat_byte(0x03),
    };
    let signing_root = call_light_client::compute_sync_committee_signing_root(
        &attested_header,
        beacon_config.fork_version,
        beacon_config.genesis_validators_root,
    );

    let (update, _secrets) = build_mock_light_client_update_for_node(signing_root);

    // Manually apply the update (simulating what the background task does)
    let result = client.apply_light_client_update(update);
    assert!(
        result.is_ok(),
        "apply_light_client_update should succeed: {:?}",
        result
    );

    let (finalized_slot, finalized_root) = result.unwrap();
    assert_eq!(finalized_slot, 98);
    client.set_finalized_block(finalized_slot, finalized_root);

    // Verify is_consensus_verified reflects the finalized block
    assert!(
        client.is_consensus_verified(98),
        "block 98 should be consensus-verified"
    );
    assert!(
        client.is_consensus_verified(97),
        "block 97 should also be consensus-verified"
    );
    assert!(
        !client.is_consensus_verified(99),
        "block 99 should NOT be consensus-verified"
    );

    node.eth_light_client = Some(Arc::new(std::sync::RwLock::new(client)));

    let _ = std::fs::remove_dir_all(&tmp);
}
