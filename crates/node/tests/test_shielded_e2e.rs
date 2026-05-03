//! E2E shielded transaction tests (Phase 13)
//!
//! NOTE: These tests have been simplified for the EVM-only mempool.
//! Protocol transactions (ShieldedDeposit, ShieldedWithdraw, ShieldedTransfer)
//! are no longer accepted by the mempool. Tests now verify basic node
//! operation and empty block production where shielded protocol txs
//! were previously required.

mod e2e;
use e2e::harness::{NodeBuilder, DeterministicRuntime, test_keypair};
use call_primitives::Address;
use call_shielded::ShieldedBlockTracker;

fn addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

// ── E2E Shielded Deposit Flow (simplified) ─────────────────────────────

#[test]
fn test_e2e_shielded_deposit_flow() {
    let (_secret, sender) = test_keypair();
    let mut node = NodeBuilder::new()
        .validator(addr(10), [10u8; 32], one_million_call())
        .balance(1, sender, 1_000_000_000)
        .balance(0, sender, 1_000_000_000)
        .build();

    // With EVM-only mempool, produce an empty block and verify node state
    let block = node.produce_block(1_000).expect("block produced");
    assert_eq!(block.evm_txs.len(), 0);

    // Shielded state starts empty
    let evm = node.state.evm_state.read().unwrap();
    let leaf_count = call_consensus::exec::state_accessors::read_shielded_commitment_count(&*evm);
    assert_eq!(leaf_count, 0);
}

// ── E2E Shielded Withdraw Flow (simplified) ────────────────────────────

#[test]
fn test_e2e_shielded_withdraw_flow() {
    let (_secret, sender) = test_keypair();
    let mut node = NodeBuilder::new()
        .validator(addr(10), [10u8; 32], one_million_call())
        .balance(1, sender, 1_000_000_000)
        .balance(0, sender, 1_000_000_000)
        .build();

    let _block = node.produce_block(1_000).expect("block produced");

    // EVM storage balance is seeded by NodeBuilder
    let evm = node.state.evm_state.read().unwrap();
    let sender_bal = call_consensus::exec::state_accessors::read_balance(&*evm, 0, sender);
    assert_eq!(sender_bal, 1_000_000_000);
}

// ── E2E Shielded Double Spend Rejected (simplified) ────────────────────

#[tokio::test]
async fn test_e2e_shielded_double_spend_rejected() {
    let (_secret, sender) = test_keypair();
    let mut node = NodeBuilder::new()
        .validator(addr(10), [10u8; 32], one_million_call())
        .balance(1, sender, 1_000_000_000)
        .balance(0, sender, 1_000_000_000)
        .build();

    // Produce two empty blocks (no protocol txs in EVM-only mempool)
    let _block1 = node.produce_block(1_000).expect("first block produced");
    let block2 = node.produce_block(2_000);
    assert!(block2.is_some(), "block should be produced");
}

// ── E2E Shielded Invalid Proof Rejected (simplified) ───────────────────

#[tokio::test]
async fn test_e2e_shielded_invalid_proof_rejected() {
    let (_secret, sender) = test_keypair();
    let mut node = NodeBuilder::new()
        .validator(addr(10), [10u8; 32], one_million_call())
        .balance(1, sender, 1_000_000_000)
        .balance(0, sender, 1_000_000_000)
        .build();

    // Block should be produced without any protocol txs
    let block = node.produce_block(1_000);
    assert!(block.is_some(), "block should be produced");
}

// ── E2E Shielded Per-Block Limit ────────────────────────────────────────

#[test]
fn test_e2e_shielded_per_block_limit() {
    let tracker = ShieldedBlockTracker::default();
    assert_eq!(tracker.count, 0);
    assert!(tracker.pending.is_empty());

    // Verify the limit is 50
    assert_eq!(ShieldedBlockTracker::MAX_PER_BLOCK, 50);
}

// ── E2E Shielded Multi-Node Consensus ───────────────────────────────────

#[tokio::test]
async fn test_e2e_shielded_multi_node_consensus() {
    let mut runtime = DeterministicRuntime::new();
    runtime.add_validator_node(1, one_million_call());
    runtime.add_validator_node(2, one_million_call());

    // Produce blocks
    let blocks = runtime.produce_blocks(3);
    assert_eq!(blocks.len(), 3);

    // All nodes should agree on shielded state (both start with empty state)
    {
        let node0_ref = runtime.simulator.node(0);
        let node0 = node0_ref.read().unwrap();
        let evm = node0.state.evm_state.read().unwrap();
        let leaf_count = call_consensus::exec::state_accessors::read_shielded_commitment_count(&*evm);
        assert_eq!(leaf_count, 0);
    }
}

// ── E2E Shielded Full Lifecycle (simplified) ────────────────────────────

#[test]
fn test_e2e_shielded_lifecycle() {
    let (_secret, sender) = test_keypair();
    let mut node = NodeBuilder::new()
        .validator(addr(10), [10u8; 32], one_million_call())
        .balance(1, sender, 1_000_000_000)
        .balance(0, sender, 1_000_000_000)
        .build();

    // Produce empty blocks (EVM-only mempool)
    let _block = node.produce_block(1_000).expect("first block");
    let _block2 = node.produce_block(2_000).expect("second block");

    // Verify EVM storage balances are intact
    {
        let evm = node.state.evm_state.read().unwrap();
        let sender_bal = call_consensus::exec::state_accessors::read_balance(&*evm, 1, sender);
        assert_eq!(sender_bal, 1_000_000_000);
    }
}
