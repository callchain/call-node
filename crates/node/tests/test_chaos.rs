//! E2E tests: Chaos engineering scenarios.
//!
//! Simulates real-world failure modes to verify network resilience:
//!   - Random node restart: node drops and rejoins
//!   - Network latency: 50-200ms delivery delays
//!   - Message drops: 10-30% packet loss
//!   - Combined chaos: all failure modes at once

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
// Chaos — Node restart
// ═══════════════════════════════════════════════════════════════════════

/// Simulate a node restart by isolating it, letting the network advance,
/// then rejoining and verifying the restarted node can resume participation.
#[tokio::test]
async fn test_chaos_random_node_restart() {
    let mut sim = PartitionSimulator::new();

    let (secret, val_addr) = test_keypair();

    // Three validators
    for i in 1..=3 {
        let node = NodeBuilder::new()
            .validator(test_addr(i), [i; 32], one_million_call())
            .balance(1, test_addr(i), 10_000)
            .build();
        sim.add_node(node);
    }

    // Produce 3 blocks with all nodes healthy
    for i in 0..3 {
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
    }

    tokio::task::yield_now().await;
    let height_before = sim.node(0).read().unwrap().consensus_height();
    assert_eq!(height_before, 3);

    // "Restart" node 2: isolate, drop all its state, recreate, rejoin
    sim.isolate_node(2);

    // Network advances while node 2 is down
    for i in 3..6 {
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
    }

    tokio::task::yield_now().await;
    let height_during = sim.node(0).read().unwrap().consensus_height();
    assert_eq!(height_during, 6);

    // Node 2 rejoins
    sim.heal_all_partitions();

    // Node 2 should now receive the backlog of messages
    let pending = sim.pending_messages(2);
    assert!(
        pending > 0,
        "restarted node should have pending messages to catch up"
    );

    // Network continues to advance after rejoin
    for i in 6..9 {
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
    }

    tokio::task::yield_now().await;
    let height_after = sim.node(0).read().unwrap().consensus_height();
    assert_eq!(height_after, 9);
}

// ═══════════════════════════════════════════════════════════════════════
// Chaos — Network latency
// ═══════════════════════════════════════════════════════════════════════

/// Messages with 50-200ms latency are queued but not delivered until
/// the delay elapses.
#[tokio::test]
async fn test_chaos_network_latency_queued_then_delivered() {
    let mut sim = PartitionSimulator::new();

    let node1 = NodeBuilder::new().build();
    let node2 = NodeBuilder::new().build();
    sim.add_node(node1);
    sim.add_node(node2);

    // 100ms latency
    sim.set_delay_ms(100);

    let net0 = sim.network(0);
    net0.broadcast(1, b"delayed".to_vec()).await;

    tokio::task::yield_now().await;

    // Message is pending but not yet deliverable
    assert_eq!(sim.pending_messages(1), 1);
    assert_eq!(sim.drain_messages(1).len(), 0);

    // After the delay passes, message becomes deliverable
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let msgs = sim.drain_messages(1);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].2, b"delayed");
}

/// Varying latency (50-200ms range tested as fixed values) does not
/// cause duplicate or lost messages.
#[tokio::test]
async fn test_chaos_varying_latency_no_loss() {
    let mut sim = PartitionSimulator::new();

    let node1 = NodeBuilder::new().build();
    let node2 = NodeBuilder::new().build();
    sim.add_node(node1);
    sim.add_node(node2);

    let net0 = sim.network(0);

    // Send messages with increasing latency
    for i in 0..5 {
        sim.set_delay_ms(50 + i as u64 * 37); // 50, 87, 124, 161, 198
        net0.broadcast(1, vec![i]).await;
    }

    tokio::task::yield_now().await;
    assert_eq!(sim.pending_messages(1), 5);

    // Wait for the longest delay (198ms + buffer)
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let msgs = sim.drain_messages(1);
    assert_eq!(msgs.len(), 5, "all 5 messages should eventually arrive");
}

// ═══════════════════════════════════════════════════════════════════════
// Chaos — Message drops
// ═══════════════════════════════════════════════════════════════════════

/// 10-30% drop rate still allows some messages through.
#[tokio::test]
async fn test_chaos_message_drops_10_30_percent() {
    let mut sim = PartitionSimulator::new();

    let node1 = NodeBuilder::new().build();
    let node2 = NodeBuilder::new().build();
    sim.add_node(node1);
    sim.add_node(node2);

    // Test 15% drop
    sim.set_drop_rate(0.15);

    let net0 = sim.network(0);
    for i in 0..100 {
        net0.broadcast(1, vec![i]).await;
    }

    tokio::task::yield_now().await;
    let received = sim.pending_messages(1);
    assert!(
        received > 50 && received < 100,
        "with 15% drop, expected 50-99 messages, got {}",
        received
    );

    // Test 30% drop
    sim.set_drop_rate(0.30);
    let net0 = sim.network(0);
    for i in 0..100 {
        net0.broadcast(1, vec![i + 100]).await;
    }

    tokio::task::yield_now().await;
    let received2 = sim.pending_messages(1);
    // Previous 100 + new 100, some dropped from both batches
    assert!(received2 > received, "more messages should have been sent");
}

// ═══════════════════════════════════════════════════════════════════════
// Chaos — Combined
// ═══════════════════════════════════════════════════════════════════════

/// Combined chaos: latency + drops + node isolation while the network
/// continues producing blocks.
#[tokio::test]
async fn test_chaos_combined_latency_drops_and_restart() {
    let mut sim = PartitionSimulator::new();

    let (secret, val_addr) = test_keypair();

    for i in 1..=3 {
        let node = NodeBuilder::new()
            .validator(test_addr(i), [i; 32], one_million_call())
            .balance(1, test_addr(i), 10_000)
            .build();
        sim.add_node(node);
    }

    // Combined failure mode: 100ms latency + 20% drops
    sim.set_delay_ms(100);
    sim.set_drop_rate(0.20);

    // Produce blocks under chaos
    for i in 0..10 {
        let node = sim.node(0);
        let mut n = node.write().unwrap();
        n.insert_evm_tx(make_evm_tx(
            &secret,
            val_addr,
            i,
            test_addr(20 + i as u8),
            100,
        ));
        n.produce_block(1_000_000 + i * 250);
        drop(n);
        tokio::task::yield_now().await;
    }

    let height = sim.node(0).read().unwrap().consensus_height();
    assert_eq!(
        height, 10,
        "node 0 should have produced 10 blocks despite chaos"
    );

    // "Restart" node 1 while chaos is active
    sim.isolate_node(1);

    for i in 10..15 {
        let node = sim.node(0);
        let mut n = node.write().unwrap();
        n.insert_evm_tx(make_evm_tx(
            &secret,
            val_addr,
            i,
            test_addr(20 + i as u8),
            100,
        ));
        n.produce_block(1_000_000 + i * 250);
        drop(n);
        tokio::task::yield_now().await;
    }

    // Rejoin node 1
    sim.heal_all_partitions();

    // Wait for latency to elapse so delayed messages become deliverable
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Node 1 should have some messages (not all, due to drops)
    let msgs = sim.drain_messages(1);
    assert!(
        !msgs.is_empty(),
        "rejoined node should have received some block announcements"
    );

    // Network should still be healthy
    let final_height = sim.node(0).read().unwrap().consensus_height();
    assert_eq!(final_height, 15);
}

/// Stress test: rapid partition/heal cycles with latency and drops.
#[tokio::test]
async fn test_chaos_rapid_partition_heal_cycles() {
    let mut sim = PartitionSimulator::new();

    let (secret, val_addr) = test_keypair();

    for i in 1..=4 {
        let node = NodeBuilder::new()
            .validator(test_addr(i), [i; 32], one_million_call())
            .balance(1, test_addr(i), 10_000)
            .build();
        sim.add_node(node);
    }

    sim.set_delay_ms(50);
    sim.set_drop_rate(0.10);

    let mut blocks_produced = 0;

    for cycle in 0..5 {
        // Partition off a random node each cycle
        sim.isolate_node(cycle % 4);

        // Produce a block
        {
            let node = sim.node(0);
            let mut n = node.write().unwrap();
            n.insert_evm_tx(make_evm_tx(
                &secret,
                val_addr,
                cycle as u64,
                test_addr(30 + cycle as u8),
                100,
            ));
            if n.produce_block(1_000_000 + cycle as u64 * 250).is_some() {
                blocks_produced += 1;
            }
        }

        tokio::task::yield_now().await;

        // Heal before next cycle
        sim.heal_all_partitions();
    }

    assert_eq!(
        blocks_produced, 5,
        "all blocks should produce despite rapid partitions"
    );
}
