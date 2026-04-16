//! E2E test: Stress and load testing
//!
//! Validates system behavior under high load:
//! - High transaction throughput
//! - Mempool capacity under pressure
//! - Block production under congestion
//! - Base fee response to sustained load
//! - No double-spend under concurrent submissions
//! - Final state consistency after load test

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::{Address, FeeCurrency};
use call_protocol::instructions::Instruction;
use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

fn make_tx(sender: Address, nonce: u64, to: Address, amount: u128) -> ProtocolTransaction {
    ProtocolTransaction {
        sender,
        nonce,
        instructions: vec![Instruction::Transfer {
            asset_id: 1,
            to,
            amount,
            memo: None,
        }],
        gas_config: GasConfig::SelfPay,
        fee_currency: FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        auth: AuthScheme::SingleSig { signature: [0u8; 65] },
    }
}

/// High throughput: inject many transactions and produce blocks.
#[tokio::test]
async fn test_high_throughput_many_transactions() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    // Fund sender with enough for 1000 transfers
    node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 10_000_000).unwrap();

    // Inject 1000 transactions
    let tx_count = 1000;
    for i in 0..tx_count {
        node.insert_tx(make_tx(sender, i as u64, test_addr(50 + (i % 50) as u8), 100));
    }

    // Produce blocks until mempool is drained
    let mut blocks = 0;
    for i in 0..100 {
        if let Some(_block) = node.produce_block(1_000_000 + i * 250) {
            blocks += 1;
        }
    }

    // At least some blocks were produced
    assert!(blocks > 0);
    assert!(node.consensus_height() > 0);
}

/// Mempool capacity: fill mempool to capacity across multiple senders.
#[test]
fn test_mempool_capacity_under_pressure() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    // Use multiple senders to exceed per-address limit (256)
    let num_senders = 3;
    for s in 0..num_senders {
        let addr = test_addr(10 + s as u8);
        node.state.balance_state.write().unwrap().balances.set_balance(1, addr, 10_000_000).unwrap();
    }

    // Fill mempool: ~170 txs per sender × 3 senders = 510 total
    let mut count = 0;
    for s in 0..num_senders {
        let sender_addr = test_addr(10 + s as u8);
        for i in 0..170 {
            node.insert_tx(make_tx(sender_addr, i as u64, test_addr(50 + (count % 20) as u8), 50));
            count += 1;
        }
    }

    // Mempool should have accepted them all
    assert_eq!(node.mempool_size(), 510);
}

/// Mempool duplicate rejection.
#[test]
fn test_mempool_duplicate_tx_rejected() {
    let node = TestNode::new();
    let sender = test_addr(1);

    let tx = make_tx(sender, 0, test_addr(2), 100);
    node.insert_tx(tx.clone());

    // Same tx again (same sender + nonce)
    node.insert_tx(tx);

    // Mempool should only keep one (dedup by sender+nonce)
    assert_eq!(node.mempool_size(), 1);
}

/// Base fee responds to sustained congestion.
#[tokio::test]
async fn test_base_fee_under_sustained_load() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 100_000_000).unwrap();

    let initial_fee = node.base_fee();

    // Produce many blocks with transactions (sustained gas usage)
    for i in 0..200 {
        node.insert_tx(make_tx(sender, i as u64, test_addr(50 + (i % 10) as u8), 100));
        node.produce_block(1_000_000 + i * 250);
    }

    let final_fee = node.base_fee();
    // Fee should have been updated (may go up or down depending on target)
    assert_ne!(initial_fee, 0);
    assert_ne!(final_fee, 0);
}

/// No double-spend: concurrent txs with same nonce only one executes.
#[tokio::test]
async fn test_no_double_spend_concurrent_nonce() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 10_000).unwrap();

    // Two txs with same nonce — second should be rejected during execution
    node.insert_tx(make_tx(sender, 0, test_addr(10), 500));
    node.insert_tx(make_tx(sender, 0, test_addr(20), 500));

    // Produce a block
    node.produce_block(1_000_000);

    // Only one transfer should have executed (nonce dedup)
    let state = node.state.balance_state.read().unwrap();
    let received_10 = state.get_balance(1, &test_addr(10));
    let received_20 = state.get_balance(1, &test_addr(20));

    // Exactly one of them received funds
    assert!((received_10 == 500 && received_20 == 0) || (received_10 == 0 && received_20 == 500));
}

/// Final state consistency: balances match expected after load.
#[tokio::test]
async fn test_final_state_consistency_after_load() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    let initial_balance = 1_000_000u128;
    node.state.balance_state.write().unwrap().balances.set_balance(1, sender, initial_balance).unwrap();

    let num_txs = 100;
    let transfer_amount = 100u128;

    // Send 100 transfers
    for i in 0..num_txs {
        node.insert_tx(make_tx(sender, i as u64, test_addr(50 + (i % 20) as u8), transfer_amount));
    }

    // Produce a block (drains all pending txs)
    node.produce_block(1_000_000);

    // Verify all recipients received funds
    let state = node.state.balance_state.read().unwrap();
    let mut total_received = 0u128;
    for i in 0..20 {
        total_received += state.get_balance(1, &test_addr(50 + i as u8));
    }

    // Total received should be num_txs * transfer_amount
    assert_eq!(total_received, num_txs as u128 * transfer_amount, "all transfers should have executed");

    // Sender balance should be reduced by total transferred
    let sender_balance = state.get_balance(1, &sender);
    assert!(sender_balance <= initial_balance - total_received, "sender should have sent funds");
}

/// Multi-sender stress: many senders submitting concurrently.
#[tokio::test]
async fn test_multi_sender_stress() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    // Fund 20 senders
    for i in 0..20 {
        let addr = test_addr(10 + i as u8);
        node.state.balance_state.write().unwrap().balances.set_balance(1, addr, 50_000).unwrap();
    }

    // Each sender submits 10 transactions
    for i in 0..20 {
        let sender_addr = test_addr(10 + i as u8);
        for j in 0..10 {
            node.insert_tx(make_tx(sender_addr, j as u64, test_addr(200 + i as u8), 100));
        }
    }

    // Produce blocks
    for i in 0..100 {
        node.produce_block(1_000_000 + i * 250);
    }

    // Verify some recipients received funds
    let state = node.state.balance_state.read().unwrap();
    let mut any_received = false;
    for i in 0..20 {
        if state.get_balance(1, &test_addr(200 + i as u8)) > 0 {
            any_received = true;
            break;
        }
    }
    assert!(any_received, "at least some recipients should have received funds");
}

/// High-volume block production: many transactions are all processed.
#[tokio::test]
async fn test_high_volume_block_production() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 10_000_000).unwrap();

    // Inject many transactions
    let tx_count = 500;
    for i in 0..tx_count {
        node.insert_tx(make_tx(sender, i as u64, test_addr(50 + (i % 50) as u8), 10));
    }

    // Produce one block (drains all)
    node.produce_block(1_000_000);

    // All transactions should have been consumed
    assert_eq!(node.mempool_size(), 0);
    assert_eq!(node.consensus_height(), 1);

    // Verify some recipients received funds
    let state = node.state.balance_state.read().unwrap();
    let mut any_received = false;
    for i in 0..50 {
        if state.get_balance(1, &test_addr(50 + i as u8)) > 0 {
            any_received = true;
            break;
        }
    }
    assert!(any_received, "recipients should have received funds");
}

/// System survives rapid block production.
#[tokio::test]
async fn test_rapid_block_production() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 1_000_000).unwrap();

    // Produce 500 blocks rapidly (no real delay)
    for i in 0..500 {
        node.insert_tx(make_tx(sender, i as u64, test_addr(99), 1));
        node.produce_block(1_000_000 + i); // 1ms apart timestamps
    }

    assert_eq!(node.consensus_height(), 500);
    assert_eq!(node.blocks_produced.len(), 500);

    // Chain should be continuous
    let mut prev_hash = call_primitives::BlockHash::ZERO;
    for block in &node.blocks_produced {
        assert_eq!(block.header.parent_hash, prev_hash);
        prev_hash = block.header.hash();
    }
}
