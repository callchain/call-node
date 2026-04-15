//! E2E test: Multi-node network
//!
//! Nodes connected via shared network, block propagation, transaction gossip.

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::{Address, TxHash};
use call_protocol::instructions::Instruction;
use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};
use call_network::{Network, NetworkMessage, TransactionMessage};

const BLOCK_CHANNEL: u64 = 2;
const TX_CHANNEL: u64 = 1;

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

/// Two nodes connected via shared network — node1 produces blocks and broadcasts.
#[tokio::test]
async fn test_two_nodes_block_propagation() {
    let mut sim = NetworkSimulator::new();

    let val_addr = test_addr(1);

    let node1 = NodeBuilder::new()
        .validator(val_addr, [1u8; 32], one_million_call())
        .balance(1, val_addr, 10_000)
        .build();

    sim.add_node(node1);
    sim.add_node(TestNode::new());

    // Produce blocks on node 0
    let node0 = sim.node(0);
    {
        let mut n = node0.write().unwrap();
        for i in 0..5 {
            n.insert_tx(make_tx(val_addr, i, test_addr(20 + i as u8), 100));
            n.produce_block(1_000_000 + i * 250);
        }
    }

    // Verify node 0 produced 5 blocks
    let n0 = sim.node(0);
    let guard = n0.read().unwrap();
    let h0 = guard.consensus_height();
    let b0 = guard.blocks_produced.len();
    drop(guard);
    assert_eq!(h0, 5);
    assert_eq!(b0, 5);

    // Drain broadcast messages (allow spawned tasks to complete)
    tokio::task::yield_now().await;
    let msgs = sim.drain_messages().await;
    let block_count = msgs.iter().filter(|(_, c, _)| *c == BLOCK_CHANNEL).count();
    assert_eq!(block_count, 5);
}

/// Transaction propagation via shared network.
#[tokio::test]
async fn test_transaction_propagation() {
    let mut sim = NetworkSimulator::new();

    let sender = test_addr(1);
    let node1 = NodeBuilder::new()
        .validator(sender, [1u8; 32], one_million_call())
        .balance(1, sender, 10_000)
        .build();

    sim.add_node(node1);
    sim.add_node(TestNode::new());

    let net = sim.network();
    let net_ref: &dyn Network = net.as_ref();

    // Insert tx on node 0
    let node0 = sim.node(0);
    {
        let n = node0.read().unwrap();
        assert_eq!(n.mempool_size(), 0);
        n.insert_tx(make_tx(sender, 0, test_addr(2), 1_000));
        assert_eq!(n.mempool_size(), 1);
    }

    // Manually broadcast tx to network (simulating gossip)
    let tx_data = serde_json::to_vec(&make_tx(sender, 0, test_addr(2), 1_000)).unwrap();
    let tx_msg = TransactionMessage::new(tx_data, TxHash::repeat_byte(0));
    let msg_data = serde_json::to_vec(&NetworkMessage::Transaction(tx_msg)).unwrap();
    net_ref.broadcast(TX_CHANNEL, msg_data).await;

    // Node receives the message
    let (_, channel, data) = net_ref.receive().await.unwrap();
    assert_eq!(channel, TX_CHANNEL);

    // Verify it parses
    let msg = serde_json::from_slice::<NetworkMessage>(&data).unwrap();
    assert!(matches!(msg, NetworkMessage::Transaction(_)));
}

/// Multiple nodes producing blocks.
#[tokio::test]
async fn test_multiple_nodes_produce_blocks() {
    let mut sim = NetworkSimulator::new();

    for i in 1..=3 {
        let node = NodeBuilder::new()
            .validator(test_addr(i), [i as u8; 32], one_million_call())
            .balance(1, test_addr(i), 100_000)
            .build();
        sim.add_node(node);
    }

    // Produce blocks on node 0
    let node0 = sim.node(0);
    {
        let mut n = node0.write().unwrap();
        for i in 0..10 {
            n.insert_tx(make_tx(test_addr(1), i, test_addr(50 + i as u8), 100));
            n.produce_block(1_000_000 + i * 250);
        }
    }

    let n0 = sim.node(0);
    let guard = n0.read().unwrap();
    let h0 = guard.consensus_height();
    drop(guard);
    assert_eq!(h0, 10);

    tokio::task::yield_now().await;
    let msgs = sim.drain_messages().await;
    let block_count = msgs.iter().filter(|(_, c, _)| *c == BLOCK_CHANNEL).count();
    assert_eq!(block_count, 10);
}
