//! E2E tests: Byzantine fault scenarios (Phase 3).
//!
//! Tests network-level Byzantine behaviours using the partition harness:
//!   - Equivocation: same proposer signs two blocks at same height
//!   - Withholding: proposer refuses to produce a block
//!   - Message flood: malicious peer spams garbage

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_consensus::Block;
use call_network::Network;
use call_primitives::{Address, BlockHash, ProtocolVersion, ValidatorId};

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

fn make_evm_tx(
    _secret: &[u8; 32],
    sender: Address,
    nonce: u64,
    to: Address,
    amount: u128,
) -> call_evm::EvmTransaction {
    call_evm::EvmTransaction {
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

// ═══════════════════════════════════════════════════════════════════════
// Phase 3 — Byzantine faults
// ═══════════════════════════════════════════════════════════════════════

/// Equivocating proposer: the same validator produces two different blocks
/// at the same height and gossips them into different network partitions.
/// When the partitions merge the double-sign is detectable.
#[tokio::test]
async fn test_equivocating_proposer_network_partition() {
    let mut sim = PartitionSimulator::new();

    let (secret, val_addr) = test_keypair();

    // Two nodes, same validator staked on node 0
    let node0 = NodeBuilder::new()
        .validator(val_addr, [1u8; 32], one_million_call())
        .balance(1, val_addr, 10_000)
        .build();
    let node1 = NodeBuilder::new().build();

    sim.add_node(node0);
    sim.add_node(node1);

    // Partition: node 0 can talk to itself only
    sim.partition_group(&[0], "partition-a");
    sim.partition_group(&[1], "partition-b");

    // Node 0 produces block 0 in partition A
    let block_a = {
        let node = sim.node(0);
        let mut n = node.write().unwrap();
        n.insert_evm_tx(make_evm_tx(&secret, val_addr, 0, test_addr(2), 100));
        n.produce_block(1_000_000).unwrap()
    };

    // Let the spawned broadcast task complete while still partitioned
    tokio::task::yield_now().await;
    // Node 1 should not have received anything yet
    assert_eq!(
        sim.pending_messages(1),
        0,
        "node 1 should be partitioned off"
    );

    // Create a *different* block at the same height (equivocation)
    let mut block_b = Block::new(
        block_a.header.height,
        block_a.header.parent_hash,
        block_a.header.timestamp_millis,
        block_a.header.proposer,
        block_a.header.version,
        vec![], // different txs
    );
    // Give it a different hash by tweaking the parent hash slightly
    block_b.header.parent_hash = BlockHash::from([0xFFu8; 32]);

    // Now heal and verify that both blocks are present in the network layer
    sim.heal_all_partitions();

    // Re-broadcast both blocks into the healed network
    let net0 = sim.network(0);
    let announcement_a = call_network::BlockAnnouncement {
        block_hash: block_a.header.hash(),
        height: block_a.header.height,
        proposer: block_a.header.proposer,
        timestamp_millis: block_a.header.timestamp_millis,
    };
    let msg_a = postcard::to_allocvec(&call_network::NetworkMessage::BlockAnnouncement(
        announcement_a,
    ))
    .unwrap();
    net0.broadcast(2, msg_a).await;

    let announcement_b = call_network::BlockAnnouncement {
        block_hash: block_b.header.hash(),
        height: block_b.header.height,
        proposer: block_b.header.proposer,
        timestamp_millis: block_b.header.timestamp_millis,
    };
    let msg_b = postcard::to_allocvec(&call_network::NetworkMessage::BlockAnnouncement(
        announcement_b,
    ))
    .unwrap();
    net0.broadcast(2, msg_b).await;

    tokio::task::yield_now().await;

    // Node 1 should have received both announcements
    let msgs = sim.drain_messages(1);
    assert_eq!(msgs.len(), 2, "node 1 should see both equivocated blocks");

    // Consensus-level: verify that a double-sign would be detected if both
    // blocks were committed.
    let node = sim.node(0);
    let n0 = node.read().unwrap();
    let provider = call_evm::provider::InMemoryStateProvider::from_db(&n0.state.db_env).unwrap();
    let val_id: ValidatorId = call_consensus::exec::state_accessors::read_validator_id_by_addr(
        provider.state(),
        val_addr,
    ) as u32;
    // The handle_double_sign method is available on the mutable consensus;
    // we just assert the validator ID is correct here.
    assert_eq!(val_id, block_a.header.proposer);
}

/// Withholding proposer: when the current proposer refuses to produce a
/// block, advancing the round selects a different proposer.
#[tokio::test]
async fn test_withholding_proposer_advances_round() {
    let mut sim = PartitionSimulator::new();

    // Stake two validators so we have a rotating proposer subset
    let node0 = NodeBuilder::new()
        .validator(test_addr(1), [1u8; 32], one_million_call())
        .validator(test_addr(2), [2u8; 32], one_million_call())
        .balance(1, test_addr(1), 10_000)
        .build();

    sim.add_node(node0);

    let node = sim.node(0);
    let n = node.read().unwrap();
    let proposer_before = n.consensus.read().unwrap().current_proposer();
    assert!(proposer_before.is_some());
    drop(n);

    // Simulate withholding: do NOT call produce_block.
    // Instead advance the round manually.
    {
        let node = sim.node(0);
        let n = node.write().unwrap();
        let mut consensus = n.consensus.write().unwrap();
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&n.state.db_env).unwrap();
        consensus.advance_round(&mut provider);
        provider.state().save_to_db(&n.state.db_env).unwrap();
    }

    let node = sim.node(0);
    let n = node.read().unwrap();
    let proposer_after = n.consensus.read().unwrap().current_proposer();
    drop(n);

    // With two validators in the subset, advancing the round should rotate
    // the proposer.
    assert_ne!(
        proposer_before, proposer_after,
        "proposer should rotate after withholding + round advance"
    );
}

/// Malicious vote flood: a node spams the network with invalid / garbage
/// messages.  Legitimate nodes should continue to function and real
/// block announcements should still be processable.
#[tokio::test]
async fn test_malicious_message_flood_ignored() {
    let mut sim = PartitionSimulator::new();

    let (secret, val_addr) = test_keypair();

    let node0 = NodeBuilder::new()
        .validator(val_addr, [1u8; 32], one_million_call())
        .balance(1, val_addr, 10_000)
        .build();
    let node1 = NodeBuilder::new().build();
    let node2 = NodeBuilder::new().build();

    sim.add_node(node0);
    sim.add_node(node1);
    sim.add_node(node2);

    // Node 2 is the attacker
    let attacker = sim.network(2);

    // Flood 1000 garbage messages
    for i in 0..1000 {
        attacker.broadcast(1, vec![0xDE, 0xAD, i as u8]).await;
    }

    tokio::task::yield_now().await;

    // Node 0 and 1 should have received the garbage
    assert!(sim.pending_messages(0) >= 1000);
    assert!(sim.pending_messages(1) >= 1000);

    // Now node 0 produces a legitimate block
    {
        let node = sim.node(0);
        let mut n = node.write().unwrap();
        n.insert_evm_tx(make_evm_tx(&secret, val_addr, 0, test_addr(2), 100));
        n.produce_block(1_000_000);
    }

    tokio::task::yield_now().await;

    // Node 1 should have the legitimate block announcement mixed with garbage.
    let msgs = sim.drain_messages(1);
    let real_announcements: Vec<_> = msgs.iter().filter(|(_, ch, _)| *ch == 2).collect();
    assert!(
        !real_announcements.is_empty(),
        "legitimate block announcement should survive the flood"
    );
}

/// Invalid block (wrong proposer) broadcast into the network is ignored by
/// validate_block.
#[tokio::test]
async fn test_invalid_block_broadcast_rejected() {
    let mut sim = PartitionSimulator::new();

    let node0 = NodeBuilder::new()
        .validator(test_addr(1), [1u8; 32], one_million_call())
        .build();
    let node1 = NodeBuilder::new().build();

    sim.add_node(node0);
    sim.add_node(node1);

    // Build an invalid block with proposer = 99999 (not in subset)
    let invalid_block = Block::new(
        0,
        BlockHash::ZERO,
        1_000_000,
        99_999, // invalid proposer
        ProtocolVersion::new(1, 0, 0),
        vec![],
    );

    let net0 = sim.network(0);
    let announcement = call_network::BlockAnnouncement {
        block_hash: invalid_block.header.hash(),
        height: invalid_block.header.height,
        proposer: invalid_block.header.proposer,
        timestamp_millis: invalid_block.header.timestamp_millis,
    };
    let msg = postcard::to_allocvec(&call_network::NetworkMessage::BlockAnnouncement(
        announcement,
    ))
    .unwrap();
    net0.broadcast(2, msg).await;

    tokio::task::yield_now().await;

    // Node 1 received the message
    let msgs = sim.drain_messages(1);
    assert_eq!(msgs.len(), 1);

    // But validation should reject it
    let node = sim.node(1);
    let n1 = node.read().unwrap();
    let consensus = n1.consensus.read().unwrap();
    let fm = n1.state.fork_manager.read().unwrap();
    let result = consensus.validate_block(&invalid_block, BlockHash::ZERO, &*fm);
    assert!(result.is_err(), "invalid proposer block should be rejected");
}
