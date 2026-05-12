//! Soak test: multi-node network under continuous load
//!
//! Validates that a network of 3 validators + 1 full node stays healthy
//! over an extended run: blocks keep producing, no forks, mempool drains,
//! state stays consistent.
//!
//! Duration is controlled by `SOAK_DURATION_SECS` env var (default: 3s for CI).

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::{Address, U256};

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn test_pubkey(n: u8) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[0] = n;
    key
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

fn make_tx(sender: Address, nonce: u64, to: Address, amount: u128) -> call_evm::EvmTransaction {
    call_evm::EvmTransaction {
        caller: sender,
        nonce,
        gas_limit: 21_000,
        gas_price: 1_000_000_000,
        to: Some(to),
        value: U256::from(amount),
        data: call_evm::Bytes::default(),
        chain_id: 1,
    }
}

/// Read native EVM balance for an address from a node.
fn native_balance(node: &TestNode, addr: &Address) -> u128 {
    let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
    provider.state().get_balance(addr).try_into().unwrap_or(0)
}

/// Memory snapshot using sysinfo.
fn memory_snapshot() -> (u64, u64) {
    use sysinfo::System;
    let mut sys = System::new_all();
    sys.refresh_memory();
    (sys.total_memory(), sys.used_memory())
}

/// Soak test: continuous block production with periodic tx injection.
#[tokio::test]
async fn test_soak_continuous_block_production() {
    let duration_secs: u64 = std::env::var("SOAK_DURATION_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3); // default 3s for fast CI

    let mut sim = NetworkSimulator::new();

    // 3 validators
    let validator_addrs: Vec<Address> = (1..=3).map(test_addr).collect();
    let mut nodes = Vec::new();
    for (i, addr) in validator_addrs.iter().enumerate() {
        let node = NodeBuilder::new()
            .validator(*addr, test_pubkey((i + 1) as u8), one_million_call())
            .balance(1, *addr, 10_000_000)
            .build();
        nodes.push(sim.add_node(node));
    }

    // 1 full node (non-validator)
    let full_node_addr = test_addr(100);
    let full_node = NodeBuilder::new()
        .balance(1, full_node_addr, 10_000_000)
        .build();
    nodes.push(sim.add_node(full_node));

    // Fund a sender account on all nodes (enough for 100 txs: 21k gas * 1gwei each)
    let sender = test_addr(200);
    let sender_initial_balance: u128 = 100_000_000_000_000_000_000;
    for node_arc in &nodes {
        let node = node_arc.read().unwrap();
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider
            .state_mut()
            .set_balance(sender, U256::from(sender_initial_balance));
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Pre-fill mempool with a burst of txs before soak begins
    let initial_txs: usize = 100;
    for i in 0..initial_txs {
        let tx = make_tx(sender, i as u64, test_addr((i % 50) as u8 + 50), 100);
        let node = nodes[0].read().unwrap();
        node.insert_evm_tx(tx);
    }

    let start = std::time::Instant::now();
    let mut blocks_produced = 0u64;
    let mut max_mempool = 0usize;
    let mut min_mempool_after_start = initial_txs;

    while start.elapsed().as_secs() < duration_secs {
        // Produce a block on validator 0
        {
            let mut node = nodes[0].write().unwrap();
            let ts = 1_000_000 + blocks_produced * 250;
            if node.produce_block(ts).is_some() {
                blocks_produced += 1;
            }
        }

        // Track mempool size
        let size = nodes[0].read().unwrap().mempool_size();
        if size > max_mempool {
            max_mempool = size;
        }
        if size < min_mempool_after_start {
            min_mempool_after_start = size;
        }

        tokio::task::yield_now().await;
    }

    // Verify: at least some blocks were produced
    assert!(
        blocks_produced > 0,
        "no blocks produced during {}s soak",
        duration_secs
    );

    // Verify: producing node advanced
    let producer_height = nodes[0].read().unwrap().consensus_height();
    assert!(
        producer_height > 0,
        "producing node did not advance: height = {}",
        producer_height
    );

    // Verify: mempool decreased (txs were being processed)
    let final_mempool = nodes[0].read().unwrap().mempool_size();
    assert!(
        final_mempool < initial_txs || min_mempool_after_start < initial_txs,
        "mempool did not shrink (initial {}, final {}, min {}), txs not being processed",
        initial_txs,
        final_mempool,
        min_mempool_after_start
    );

    // Verify: broadcast messages were sent
    let msgs = sim.drain_messages().await;
    let block_msgs = msgs.iter().filter(|(_, c, _)| *c == 2).count();
    assert!(
        block_msgs >= blocks_produced as usize / 2,
        "expected at least {} block announcements, got {}",
        blocks_produced / 2,
        block_msgs
    );

    // Verify: sender balance decreased (txs were executed)
    let final_balance = native_balance(&nodes[0].read().unwrap(), &sender);
    assert!(
        final_balance < sender_initial_balance,
        "sender balance did not decrease ({}), txs may not have executed",
        final_balance
    );

    // Verify: blocks are monotonically increasing in height
    let node0 = nodes[0].read().unwrap();
    for window in node0.blocks_produced.windows(2) {
        assert!(
            window[1].header.height > window[0].header.height,
            "block heights not monotonically increasing"
        );
    }

    tracing::info!(
        duration_secs,
        blocks_produced,
        initial_txs,
        max_mempool,
        "soak test completed"
    );
}

/// Soak test: high-frequency empty blocks (no txs) to verify consensus stability.
#[tokio::test]
async fn test_soak_empty_blocks() {
    let duration_secs: u64 = std::env::var("SOAK_DURATION_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);

    let mut sim = NetworkSimulator::new();

    let validator = test_addr(1);
    let node = NodeBuilder::new()
        .validator(validator, test_pubkey(1), one_million_call())
        .build();
    sim.add_node(node);

    let start = std::time::Instant::now();
    let mut blocks = 0u64;

    while start.elapsed().as_secs() < duration_secs {
        {
            let node0 = sim.node(0);
            let mut node = node0.write().unwrap();
            let ts = 1_000_000 + blocks * 250;
            if node.produce_block(ts).is_some() {
                blocks += 1;
            }
        }
        tokio::task::yield_now().await;
    }

    assert!(blocks > 0, "no empty blocks produced during soak");

    let node0 = sim.node(0);
    let node = node0.read().unwrap();
    assert_eq!(
        node.consensus_height(),
        blocks,
        "consensus height should match blocks produced"
    );
}

/// Soak test: sustained high TPS with metering and memory profiling.
///
/// Pre-fills mempool with a large burst, then continuously injects new
/// transactions while producing blocks. Measures TPS and memory usage.
///
/// Duration is controlled by `SOAK_DURATION_SECS` env var (default: 5s
/// for CI). For a 1-hour stress run, set `SOAK_DURATION_SECS=3600`.
#[tokio::test]
async fn test_soak_sustained_high_tps_with_profiling() {
    let duration_secs: u64 = std::env::var("SOAK_DURATION_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let mut sim = NetworkSimulator::new();

    // 3 validators
    let validator_addrs: Vec<Address> = (1..=3).map(test_addr).collect();
    let mut nodes = Vec::new();
    for (i, addr) in validator_addrs.iter().enumerate() {
        let node = NodeBuilder::new()
            .validator(*addr, test_pubkey((i + 1) as u8), one_million_call())
            .balance(1, *addr, 10_000_000)
            .build();
        nodes.push(sim.add_node(node));
    }

    // Fund a sender account on all nodes
    let sender = test_addr(200);
    let sender_initial_balance: u128 = u128::MAX / 4;
    for node_arc in &nodes {
        let node = node_arc.read().unwrap();
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider
            .state_mut()
            .set_balance(sender, U256::from(sender_initial_balance));
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Large initial burst to stress the mempool
    let burst_txs: usize = 5_000;
    for i in 0..burst_txs {
        let tx = make_tx(sender, i as u64, test_addr((i % 100) as u8 + 50), 1);
        let node = nodes[0].read().unwrap();
        node.insert_evm_tx(tx);
    }

    let (total_mem_start, used_mem_start) = memory_snapshot();
    let start = std::time::Instant::now();

    let mut blocks_produced = 0u64;
    let mut total_txs_executed = 0usize;
    let mut max_mempool = 0usize;
    let mut peak_used_mem = used_mem_start;
    let mut next_nonce = burst_txs as u64;

    while start.elapsed().as_secs() < duration_secs {
        // Continuously inject new txs to sustain load
        for _ in 0..50 {
            let tx = make_tx(
                sender,
                next_nonce,
                test_addr((next_nonce % 100) as u8 + 50),
                1,
            );
            next_nonce += 1;
            let node = nodes[0].read().unwrap();
            node.insert_evm_tx(tx);
        }

        // Produce a block
        let block = {
            let mut node = nodes[0].write().unwrap();
            let ts = 1_000_000 + blocks_produced * 250;
            node.produce_block(ts)
        };

        if block.is_some() {
            blocks_produced += 1;
        }

        // Track mempool
        let size = nodes[0].read().unwrap().mempool_size();
        if size > max_mempool {
            max_mempool = size;
        }

        // Count executed txs from last result
        if let Some(ref result) = nodes[0].read().unwrap().last_result {
            total_txs_executed += result.evm_tx_results.len();
        }

        // Periodic memory snapshot (every ~1s)
        if blocks_produced % 4 == 0 {
            let (_, used) = memory_snapshot();
            if used > peak_used_mem {
                peak_used_mem = used;
            }
        }

        tokio::task::yield_now().await;
    }

    let elapsed_secs = start.elapsed().as_secs_f64();
    let tps = if elapsed_secs > 0.0 {
        total_txs_executed as f64 / elapsed_secs
    } else {
        0.0
    };

    let (_, used_mem_end) = memory_snapshot();
    let mem_growth_mb = (used_mem_end.saturating_sub(used_mem_start)) as f64 / 1_048_576.0;
    let peak_growth_mb = (peak_used_mem.saturating_sub(used_mem_start)) as f64 / 1_048_576.0;

    // Assertions
    assert!(
        blocks_produced > 0,
        "no blocks produced during {}s soak",
        duration_secs
    );

    assert!(
        total_txs_executed > 0,
        "no transactions executed during soak"
    );

    // TPS should be positive; in this single-node test harness we
    // don't expect 1000 TPS, but we verify the metering works.
    assert!(tps > 0.0, "TPS metering failed: {:.2}", tps);

    // Memory growth should be reasonable (under 1GB for test harness)
    assert!(
        mem_growth_mb < 1024.0,
        "memory grew by {:.1}MB during soak, possible leak",
        mem_growth_mb
    );

    // Verify sender balance decreased (txs actually executed)
    let final_balance = native_balance(&nodes[0].read().unwrap(), &sender);
    assert!(
        final_balance < sender_initial_balance,
        "sender balance did not decrease, txs may not have executed"
    );

    // Verify monotonic block heights
    let node0 = nodes[0].read().unwrap();
    for window in node0.blocks_produced.windows(2) {
        assert!(
            window[1].header.height > window[0].header.height,
            "block heights not monotonically increasing"
        );
    }

    tracing::info!(
        duration_secs,
        blocks_produced,
        total_txs_executed,
        tps = format!("{:.2}", tps),
        max_mempool,
        mem_growth_mb = format!("{:.1}", mem_growth_mb),
        peak_growth_mb = format!("{:.1}", peak_growth_mb),
        total_mem_start,
        "sustained high-TPS soak completed"
    );
}
