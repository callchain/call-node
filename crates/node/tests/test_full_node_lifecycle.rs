//! E2E test: Full node lifecycle
//!
//! Fresh node -> genesis -> sync -> process txs -> restart -> recovery

#[path = "e2e/mod.rs"]
mod e2e;
use e2e::harness::*;

use call_primitives::{Address, BlockHash};

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn one_million_call() -> u128 {
    1_000_000 * 10u128.pow(18)
}

fn make_tx(
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
        gas_price: 10,
        to: Some(to),
        value: call_primitives::U256::from(amount),
        data: call_evm::Bytes::default(),
        chain_id: 1,
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
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        consensus
            .stake_validator(
                provider.state_mut(),
                test_addr(1),
                [1u8; 32],
                one_million_call(),
            )
            .unwrap();
        consensus.refresh_proposer_subset(provider.state());
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Setup balance
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            test_addr(1),
            10_000,
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Produce first block
    let block = node.produce_block(1_000_000).expect("should produce block");
    assert_eq!(block.header.height, 0);
    assert_eq!(node.consensus_height(), 1);
    assert_eq!(node.height, 1);
}

/// Node processes transactions through mempool -> block -> balance update.
#[tokio::test]
async fn test_node_process_transactions() {
    let mut node = TestNode::new();

    let (secret, sender) = test_keypair();
    let receiver = test_addr(2);

    // Stake validator
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

    // Seed EVM storage with CALL balance for fees + native balance for gas
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        call_consensus::exec::state_accessors::seed_balance(
            provider.state_mut(),
            call_protocol::CALL_ASSET_ID,
            sender,
            10_000_000,
        );
        provider
            .state_mut()
            .set_balance(sender, call_primitives::U256::from(100_000_000_000u128));
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Insert tx
    node.insert_evm_tx(make_tx(&secret, sender, 0, receiver, 3_000));
    assert_eq!(node.mempool_size(), 1);

    // Produce block
    let block = node.produce_block(1_000_000).expect("should produce block");

    // Mempool cleared, native EVM balance updated
    assert_eq!(node.mempool_size(), 0);
    let provider = call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
    let receiver_native = provider.state().get_balance(&receiver);
    let sender_native = provider.state().get_balance(&sender);
    assert_eq!(receiver_native, call_primitives::U256::from(3_000));
    assert!(sender_native < call_primitives::U256::from(100_000_000_000u128));
    assert_eq!(block.header.height, 0);
}

/// Node persists blocks to disk and can read them back.
#[tokio::test]
async fn test_node_persist_and_recover() {
    let mut node = TestNode::new();

    let (secret, sender) = test_keypair();
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
            10_000_000,
        );
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Produce 3 blocks
    node.insert_evm_tx(make_tx(&secret, sender, 0, test_addr(2), 1_000));
    node.produce_block(1_000_000);

    node.insert_evm_tx(make_tx(&secret, sender, 1, test_addr(3), 2_000));
    node.produce_block(1_000_250);

    node.insert_evm_tx(make_tx(&secret, sender, 2, test_addr(4), 500));
    node.produce_block(1_000_500);

    assert_eq!(node.consensus_height(), 3);

    // Persist all blocks to MDBX
    for block in &node.blocks_produced {
        let height = block.header.height;
        let key = height.to_be_bytes().to_vec();
        let value = serde_json::to_vec(block).unwrap();
        call_storage::reth_db::db_put::<call_storage::reth_db::CallConsensusBlocks>(
            &node.state.db_env, key, value,
        )
        .unwrap();
    }

    // Read blocks back from MDBX and verify
    for block in &node.blocks_produced {
        let height = block.header.height;
        let key = height.to_be_bytes().to_vec();
        let data = call_storage::reth_db::db_get::<call_storage::reth_db::CallConsensusBlocks>(
            &node.state.db_env, &key,
        )
        .unwrap()
        .expect("block should exist");
        let restored: call_consensus::Block = serde_json::from_slice(&data).unwrap();
        assert_eq!(restored.header.height, height);
        assert_eq!(restored.header.hash(), block.header.hash());
    }
}

/// Multiple blocks in sequence maintain correct parent hash chain.
#[tokio::test]
async fn test_block_chain_continuity() {
    let mut node = TestNode::new();

    let (secret, sender) = test_keypair();
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

    let mut prev_hash = BlockHash::ZERO;
    for i in 0..10 {
        node.insert_evm_tx(make_tx(
            &secret,
            sender,
            i as u64,
            test_addr(10 + i as u8),
            100,
        ));
        let block = node
            .produce_block(1_000_000 + i * 250)
            .expect("produce block");
        assert_eq!(block.header.parent_hash, prev_hash);
        assert_eq!(block.header.height, i);
        prev_hash = block.header.hash();
    }

    assert_eq!(node.consensus_height(), 10);
}
