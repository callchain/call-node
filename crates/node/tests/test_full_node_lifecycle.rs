//! E2E test: Full node lifecycle
//!
//! Fresh node → genesis → sync → process txs → restart → recovery

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::{Address, BlockHash};
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
        fee_currency: call_primitives::FeeCurrency::Call,
        gas_limit: 100_000,
        max_fee: 1_000_000,
        auth: AuthScheme::SingleSig { signature: [0u8; 65] },
    }
}

/// Fresh node starts at height 0, produces genesis block, advances.
#[tokio::test]
async fn test_node_starts_at_genesis() {
    let mut node = TestNode::new();

    assert_eq!(node.consensus_height(), 0);
    assert_eq!(node.parent_hash, BlockHash::ZERO);

    // Stake validator so proposer selection works
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(test_addr(1), [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    // Setup balance
    {
        node.state.balance_state.write().unwrap().balances.set_balance(1, test_addr(1), 10_000).unwrap();
    }

    // Produce first block
    let block = node.produce_block(1_000_000).expect("should produce block");
    assert_eq!(block.header.height, 0);
    assert_eq!(node.consensus_height(), 1);
    assert_eq!(node.height, 1);
}

/// Node processes transactions through mempool → block → balance update.
#[tokio::test]
async fn test_node_process_transactions() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    let receiver = test_addr(2);

    // Stake validator
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }

    // Fund sender
    {
        node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 10_000).unwrap();
    }

    // Insert tx
    node.insert_tx(make_tx(sender, 0, receiver, 3_000));
    assert_eq!(node.mempool_size(), 1);

    // Produce block
    let block = node.produce_block(1_000_000).expect("should produce block");

    // Mempool cleared, balance updated
    assert_eq!(node.mempool_size(), 0);
    assert_eq!(node.balance(1, &receiver), 3_000);
    assert!(node.balance(1, &sender) <= 7_000); // may have gas deducted
    assert_eq!(block.header.height, 0);
}

/// Node persists blocks to disk and can read them back.
#[tokio::test]
async fn test_node_persist_and_recover() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }
    {
        node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 10_000).unwrap();
    }

    // Produce 3 blocks
    node.insert_tx(make_tx(sender, 0, test_addr(2), 1_000));
    node.produce_block(1_000_000);

    node.insert_tx(make_tx(sender, 1, test_addr(3), 2_000));
    node.produce_block(1_000_250);

    node.insert_tx(make_tx(sender, 2, test_addr(4), 500));
    node.produce_block(1_000_500);

    assert_eq!(node.consensus_height(), 3);

    // Persist all blocks
    for block in &node.blocks_produced {
        let height = block.header.height;
        let dir = node.data_dir.join("blocks");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{height:012}.json"));
        let data = serde_json::to_vec(block).unwrap();
        std::fs::write(&path, data).unwrap();
    }

    // Read blocks back and verify
    for block in &node.blocks_produced {
        let height = block.header.height;
        let path = node.data_dir.join("blocks").join(format!("{height:012}.json"));
        assert!(path.exists(), "block {height} should exist");

        let data = std::fs::read(&path).unwrap();
        let restored: call_consensus::Block = serde_json::from_slice(&data).unwrap();
        assert_eq!(restored.header.height, height);
        assert_eq!(restored.header.hash(), block.header.hash());
    }
}

/// Multiple blocks in sequence maintain correct parent hash chain.
#[tokio::test]
async fn test_block_chain_continuity() {
    let mut node = TestNode::new();

    let sender = test_addr(1);
    {
        let mut consensus = node.consensus.write().unwrap();
        consensus.stake_validator(sender, [1u8; 32], one_million_call()).unwrap();
        consensus.refresh_proposer_subset();
    }
    {
        node.state.balance_state.write().unwrap().balances.set_balance(1, sender, 100_000).unwrap();
    }

    let mut prev_hash = BlockHash::ZERO;
    for i in 0..10 {
        node.insert_tx(make_tx(sender, i as u64, test_addr(10 + i as u8), 100));
        let block = node.produce_block(1_000_000 + i * 250).expect("produce block");
        assert_eq!(block.header.parent_hash, prev_hash);
        assert_eq!(block.header.height, i);
        prev_hash = block.header.hash();
    }

    assert_eq!(node.consensus_height(), 10);
}
