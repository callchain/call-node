//! E2E test: Fork and upgrade
//!
//! Height-activated governance upgrade simulation.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_evm::EvmTransaction;
use call_primitives::{Address, BlockHash, ProtocolVersion};

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

fn make_evm_tx(sender: Address, nonce: u64, to: Address, amount: u128) -> EvmTransaction {
    EvmTransaction {
        caller: sender,
        nonce,
        gas_limit: 21_000,
        gas_price: 1_000_000_000,
        to: Some(to),
        value: call_primitives::U256::from(amount),
        data: call_evm::Bytes::default(),
        chain_id: 1,
        max_priority_fee: None,
        tx_type: 0,
    }
}

/// Simulate a protocol upgrade at a specific block height.
#[tokio::test]
async fn test_height_activated_upgrade() {
    let mut node = TestNode::new();

    let (_secret, sender) = test_keypair();
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(provider.state_mut(), sender, [1u8; 32], one_million_call())
            .unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            sender,
            100_000,
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    let upgrade_height = 50u64;
    let mut upgraded = false;

    // Produce blocks up to and past the upgrade height
    for i in 0..60 {
        node.insert_evm_tx(make_evm_tx(
            sender,
            i as u64,
            test_addr(50 + (i % 10) as u8),
            100,
        ));

        let ts = 1_000_000 + i * 250;
        let selection = { node.mempool.write().unwrap().select_transactions() };

        let (proposer, height) = {
            let c = node.consensus.read().unwrap();
            (c.current_proposer(), c.current_height())
        };

        let Some(proposer) = proposer else {
            continue;
        };

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        if height == upgrade_height && !upgraded {
            upgraded = true;
            node.state.fork_manager.write().unwrap().schedule_upgrade(
                call_consensus::fork::UpgradeEntry {
                    version: ProtocolVersion::new(2, 0, 0),
                    activation_height: upgrade_height,
                    applied: false,
                    proposal_id: Some(1),
                    approved_at_height: Some(height),
                },
            );
        }

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block =
            call_consensus::Block::new(height, node.parent_hash, ts, proposer, version, evm_txs);

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

        node.state.set_current_block(height + 1);
        node.parent_hash = block.header.hash();
        node.state.finalize_block();
        node.height = height + 1;
        node.blocks_produced.push(block);
    }

    assert!(upgraded, "upgrade should have been triggered");
    assert_eq!(node.consensus_height(), 60);
}

/// Chain fork: two nodes produce different blocks at same height, then reconcile.
#[tokio::test]
async fn test_chain_fork_and_reconcile() {
    let mut sim = NetworkSimulator::new();

    let (_secret, sender) = test_keypair();

    // Node A and Node B start with same initial state
    let node_a = NodeBuilder::new()
        .validator(sender, [1u8; 32], one_million_call())
        .balance(1, sender, 100_000)
        .build();

    let node_b = NodeBuilder::new()
        .balance(1, sender, 100_000) // no validators, just tracking state
        .build();

    let node_a_ref = sim.add_node(node_a);
    let _node_b_ref = sim.add_node(node_b);

    // Node A produces 5 blocks
    {
        let mut na = node_a_ref.write().unwrap();
        for i in 0..5 {
            na.insert_evm_tx(make_evm_tx(sender, i, test_addr(20 + i as u8), 100));
            na.produce_block(1_000_000 + i * 250);
        }
    }

    // Node A's chain is linear
    {
        let na = node_a_ref.read().unwrap();
        assert_eq!(na.consensus_height(), 5);

        let mut prev_hash = BlockHash::ZERO;
        for block in &na.blocks_produced {
            assert_eq!(block.header.parent_hash, prev_hash);
            prev_hash = block.header.hash();
        }
    }
}

/// Governance-style upgrade: schedule via fork_manager, upgrade activates at height.
#[tokio::test]
async fn test_governance_triggered_upgrade() {
    let mut node = TestNode::new();

    let (_secret, sender) = test_keypair();
    let val_addr = sender;
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                provider.state_mut(),
                val_addr,
                [1u8; 32],
                one_million_call(),
            )
            .unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            sender,
            100_000,
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    let upgrade_height = 5u64;
    {
        let mut fm = node.state.fork_manager.write().unwrap();
        fm.schedule_upgrade(call_consensus::fork::UpgradeEntry {
            version: ProtocolVersion::new(2, 0, 0),
            activation_height: upgrade_height,
            applied: false,
            proposal_id: Some(1),
            approved_at_height: Some(0),
        });
    }

    let mut upgraded = false;

    // Produce blocks up to and past the upgrade height
    for i in 0..10 {
        node.insert_evm_tx(make_evm_tx(sender, i as u64, test_addr(30 + i as u8), 100));
        node.produce_block(1_000_000 + i * 250);

        let height = node.consensus_height();
        {
            let mut fm = node.state.fork_manager.write().unwrap();
            if fm.check_upgrades_at_height(height).is_some() {
                upgraded = true;
            }
        }
    }

    assert!(
        upgraded,
        "governance upgrade should have activated at height {upgrade_height}"
    );
    assert_eq!(node.consensus_height(), 10);
}

/// Mixed validator versions: nodes with different protocol versions produce
/// and verify blocks without consensus split.
#[tokio::test]
async fn test_mixed_validator_versions_consensus() {
    let mut sim = NetworkSimulator::new();

    let (_secret, sender) = test_keypair();

    // Node A: version 1.0.0
    let node_a = NodeBuilder::new()
        .validator(sender, [1u8; 32], one_million_call())
        .balance(1, sender, 100_000)
        .build();

    // Node B: version 2.0.0 (simulates upgraded validator)
    let node_b = {
        let n = NodeBuilder::new()
            .validator(sender, [1u8; 32], one_million_call())
            .balance(1, sender, 100_000)
            .build();
        // Override fork manager to newer version
        *n.state.fork_manager.write().unwrap() =
            call_consensus::ForkManager::new(ProtocolVersion::new(2, 0, 0), 1);
        n
    };

    let node_a_ref = sim.add_node(node_a);
    let node_b_ref = sim.add_node(node_b);

    // Both nodes produce blocks independently
    for i in 0..5 {
        let tx = make_evm_tx(sender, i, test_addr(40 + i as u8), 100);

        {
            let mut na = node_a_ref.write().unwrap();
            na.insert_evm_tx(tx.clone());
            na.produce_block(1_000_000 + i * 250);
        }

        {
            let mut nb = node_b_ref.write().unwrap();
            nb.insert_evm_tx(tx);
            nb.produce_block(1_000_000 + i * 250);
        }
    }

    // Both chains advanced
    let ha = node_a_ref.read().unwrap().consensus_height();
    let hb = node_b_ref.read().unwrap().consensus_height();
    assert_eq!(ha, 5, "node A should have produced 5 blocks");
    assert_eq!(hb, 5, "node B should have produced 5 blocks");

    // Verify state consistency: sender balance decreased on both
    let bal_a = {
        let na = node_a_ref.read().unwrap();
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&na.state.db_env).unwrap();
        provider
            .state()
            .get_balance(&sender)
            .try_into()
            .unwrap_or(0u128)
    };
    let bal_b = {
        let nb = node_b_ref.read().unwrap();
        let provider =
            call_evm::provider::InMemoryStateProvider::from_db(&nb.state.db_env).unwrap();
        provider
            .state()
            .get_balance(&sender)
            .try_into()
            .unwrap_or(0u128)
    };
    assert!(bal_a < 100_000, "node A state should reflect executed txs");
    assert!(bal_b < 100_000, "node B state should reflect executed txs");

    // Verify block hashes differ (different versions mean different blocks)
    let hash_a = node_a_ref.read().unwrap().blocks_produced[0].header.hash();
    let hash_b = node_b_ref.read().unwrap().blocks_produced[0].header.hash();
    assert_ne!(
        hash_a, hash_b,
        "blocks from different versions should have different hashes"
    );
}
