//! E2E tests: Network partition scenarios (Phase 1 & Phase 2).
//!
//! Phase 1 — PartitionableNetwork infrastructure:
//!   - Partition groups block cross-group messages
//!   - Drop rate probabilistically discards messages
//!   - Isolation and healing work correctly
//!
//! Phase 2 — Consensus-aware partition tests:
//!   - Block gossip stops across a partition
//!   - Block gossip resumes after healing
//!   - Partial drops don't crash the system

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_network::Network;
use call_primitives::Address;

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

// ═══════════════════════════════════════════════════════════════════════
// Phase 1 — Infrastructure
// ═══════════════════════════════════════════════════════════════════════

/// Nodes in different partition groups cannot exchange messages.
#[tokio::test]
async fn test_partition_blocks_messages() {
    let mut sim = PartitionSimulator::new();

    let node1 = NodeBuilder::new()
        .validator(test_addr(1), [1u8; 32], one_million_call())
        .build();
    let node2 = NodeBuilder::new()
        .validator(test_addr(2), [2u8; 32], one_million_call())
        .build();

    sim.add_node(node1);
    sim.add_node(node2);

    // Split into two groups
    sim.partition_group(&[0], "group-a");
    sim.partition_group(&[1], "group-b");

    // Node 0 broadcasts
    let net0 = sim.network(0);
    net0.broadcast(1, b"hello".to_vec()).await;

    // Node 1 should receive nothing
    tokio::task::yield_now().await;
    assert_eq!(sim.pending_messages(1), 0);

    // Node 0 should have received its own broadcast (same group)
    assert_eq!(sim.pending_messages(0), 1);
}

/// Healing a partition restores message flow.
#[tokio::test]
async fn test_partition_heal_restores_communication() {
    let mut sim = PartitionSimulator::new();

    let node1 = NodeBuilder::new()
        .validator(test_addr(1), [1u8; 32], one_million_call())
        .build();
    let node2 = NodeBuilder::new()
        .validator(test_addr(2), [2u8; 32], one_million_call())
        .build();

    sim.add_node(node1);
    sim.add_node(node2);

    // Split
    sim.partition_group(&[0], "group-a");
    sim.partition_group(&[1], "group-b");

    // Heal
    sim.heal_all_partitions();

    // Node 0 broadcasts after healing
    let net0 = sim.network(0);
    net0.broadcast(1, b"after-heal".to_vec()).await;

    tokio::task::yield_now().await;
    assert_eq!(sim.pending_messages(1), 1);
}

/// Drop rate probabilistically discards messages.
#[tokio::test]
async fn test_drop_rate_discards_messages() {
    let mut sim = PartitionSimulator::new();

    let node1 = NodeBuilder::new().build();
    let node2 = NodeBuilder::new().build();

    sim.add_node(node1);
    sim.add_node(node2);

    // 50 % drop rate
    sim.set_drop_rate(0.5);

    let net0 = sim.network(0);
    for i in 0..100 {
        net0.broadcast(1, vec![i]).await;
    }

    tokio::task::yield_now().await;
    let received = sim.pending_messages(1);
    // With 50 % drop, we expect roughly 50 messages.  Be generous with
    // the bounds so the test doesn't flake.
    assert!(
        received > 20 && received < 80,
        "expected ~50 messages, got {}",
        received
    );
}

/// An isolated node cannot send or receive anything.
#[tokio::test]
async fn test_isolated_node_cannot_send_or_receive() {
    let mut sim = PartitionSimulator::new();

    for i in 1..=3 {
        let node = NodeBuilder::new()
            .validator(test_addr(i), [i; 32], one_million_call())
            .build();
        sim.add_node(node);
    }

    // Isolate node 1
    sim.isolate_node(1);

    // Node 0 broadcasts to the default group
    let net0 = sim.network(0);
    net0.broadcast(1, b"test".to_vec()).await;

    tokio::task::yield_now().await;

    // Node 0 (default group) gets its own message
    assert_eq!(sim.pending_messages(0), 1);
    // Node 1 (isolated) gets nothing
    assert_eq!(sim.pending_messages(1), 0);
    // Node 2 (default group) gets node 0's message
    assert_eq!(sim.pending_messages(2), 1);

    // Node 1 tries to broadcast — nobody should receive it
    let net1 = sim.network(1);
    net1.broadcast(1, b"from-isolated".to_vec()).await;

    tokio::task::yield_now().await;
    assert_eq!(sim.pending_messages(0), 1);
    assert_eq!(sim.pending_messages(2), 1);
}

// ═══════════════════════════════════════════════════════════════════════
// Phase 2 — Consensus-aware partition tests
// ═══════════════════════════════════════════════════════════════════════

/// Block broadcast from a partitioned node does not reach the other partition.
#[tokio::test]
async fn test_partition_prevents_block_gossip() {
    let mut sim = PartitionSimulator::new();

    let (secret, val_addr) = test_keypair();

    let node1 = NodeBuilder::new()
        .validator(val_addr, [1u8; 32], one_million_call())
        .balance(1, val_addr, 10_000)
        .build();
    let node2 = NodeBuilder::new().build();

    sim.add_node(node1);
    sim.add_node(node2);

    // Split
    sim.partition_group(&[0], "validators");
    sim.partition_group(&[1], "observers");

    // Produce a block on node 0
    {
        let node = sim.node(0);
        let mut n = node.write().unwrap();
        n.insert_evm_tx(make_evm_tx(&secret, val_addr, 0, test_addr(2), 100));
        n.produce_block(1_000_000);
    }

    tokio::task::yield_now().await;

    // Node 0 should have broadcast messages
    assert!(sim.pending_messages(0) > 0);
    // Node 1 (other partition) should have none
    assert_eq!(sim.pending_messages(1), 0);
}

/// After healing, block gossip resumes.
#[tokio::test]
async fn test_heal_resumes_block_gossip() {
    let mut sim = PartitionSimulator::new();

    let (secret, val_addr) = test_keypair();

    let node1 = NodeBuilder::new()
        .validator(val_addr, [1u8; 32], one_million_call())
        .balance(1, val_addr, 10_000)
        .build();
    let node2 = NodeBuilder::new().build();

    sim.add_node(node1);
    sim.add_node(node2);

    // Split, produce a block, then heal
    sim.partition_group(&[0], "a");
    sim.partition_group(&[1], "b");

    {
        let node = sim.node(0);
        let mut n = node.write().unwrap();
        n.insert_evm_tx(make_evm_tx(&secret, val_addr, 0, test_addr(2), 100));
        n.produce_block(1_000_000);
    }

    tokio::task::yield_now().await;
    // Pre-heal: node 1 has nothing
    assert_eq!(sim.pending_messages(1), 0);

    // Heal
    sim.heal_all_partitions();

    // Produce another block post-heal
    {
        let node = sim.node(0);
        let mut n = node.write().unwrap();
        n.insert_evm_tx(make_evm_tx(&secret, val_addr, 1, test_addr(3), 200));
        n.produce_block(1_000_250);
    }

    tokio::task::yield_now().await;
    // Post-heal: node 1 should have received the broadcast
    assert!(sim.pending_messages(1) > 0);
}

/// Partial message drops don't crash the system.
#[tokio::test]
async fn test_partial_drops_dont_crash() {
    let mut sim = PartitionSimulator::new();

    let (secret, val_addr) = test_keypair();

    let node1 = NodeBuilder::new()
        .validator(val_addr, [1u8; 32], one_million_call())
        .balance(1, val_addr, 10_000)
        .build();
    let node2 = NodeBuilder::new().build();

    sim.add_node(node1);
    sim.add_node(node2);

    // 30 % drop rate
    sim.set_drop_rate(0.30);

    // Produce several blocks
    for i in 0..10 {
        let node = sim.node(0);
        let mut n = node.write().unwrap();
        n.insert_evm_tx(make_evm_tx(
            &secret,
            val_addr,
            i,
            test_addr(10 + i as u8),
            100,
        ));
        n.produce_block(1_000_000 + i * 250);
        drop(n);
        tokio::task::yield_now().await;
    }

    // Node 0 produced 10 blocks
    let node = sim.node(0);
    let n0 = node.read().unwrap();
    assert_eq!(n0.blocks_produced.len(), 10);
    drop(n0);

    // Node 1 received *some* messages but not necessarily all
    let received = sim.pending_messages(1);
    assert!(
        received < 10 * 2,
        "node 1 should not have received every broadcast"
    );
}

// ── Helpers ───────────────────────────────────────────────────────────

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
    }
}
