//! Benchmark: Block production hot paths
//!
//! Measures block execution throughput with varying transaction counts.

use call_consensus::Block;
use call_evm::EvmTransaction;
use call_node::CallNode;
use call_primitives::{Address, U256};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::path::PathBuf;

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

/// Lazily-generated secp256k1 keypair shared across benchmarks.
fn test_sender() -> (Address, [u8; 32]) {
    let (secret, pubkey) = call_crypto::generate_keypair();
    let addr = call_crypto::pubkey_to_address(&pubkey);
    (addr, secret)
}

fn make_evm_tx(nonce: u64, sender: Address) -> EvmTransaction {
    EvmTransaction {
        caller: sender,
        nonce,
        gas_limit: 21_000,
        gas_price: 10,
        to: Some(test_addr(2)),
        value: U256::from(100),
        data: call_evm::Bytes::default(),
        chain_id: 1,
    }
}

fn setup_node() -> (CallNode, Address) {
    let tmp = PathBuf::from(format!(
        "/tmp/call-bench-node-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);

    let node = CallNode::new(tmp.clone()).expect("node creation");
    let (sender_addr, _) = test_sender();

    // Stake a validator so proposer selection works
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        let mut consensus = node.consensus.write().unwrap();
        let mut key = [0u8; 32];
        key[0] = 1;
        consensus
            .stake_validator(
                &mut provider,
                test_addr(1),
                call_primitives::Ed25519PublicKey::from(key),
                1_000_000 * 10u128.pow(18),
            )
            .expect("stake");
        consensus.refresh_proposer_subset(&provider);
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    // Fund sender balance
    {
        let mut provider =
            call_evm::provider::InMemoryStateProvider::from_db(&node.state.db_env).unwrap();
        provider
            .state_mut()
            .set_balance(sender_addr, U256::from(10_000_000_000_000u128));
        provider.state().save_to_db(&node.state.db_env).unwrap();
    }

    (node, sender_addr)
}

fn build_block(node: &CallNode, evm_txs: Vec<EvmTransaction>) -> Block {
    let proposer = node
        .consensus
        .read()
        .unwrap()
        .current_proposer()
        .expect("proposer");
    let height = node.consensus.read().unwrap().current_height();
    let version = node.state.fork_manager.read().unwrap().current_version();

    let evm_tx_data: Vec<Vec<u8>> = evm_txs.into_iter().map(|e| {
        // Minimal RLP encoding for benchmark tx
        let mut buf = Vec::new();
        buf.extend_from_slice(&e.nonce.to_be_bytes());
        buf.extend_from_slice(&e.gas_price.to_be_bytes());
        buf.extend_from_slice(&e.gas_limit.to_be_bytes());
        if let Some(to) = e.to {
            buf.extend_from_slice(to.as_slice());
        }
        buf.extend_from_slice(e.value.to_be_bytes_vec().as_slice());
        buf.extend_from_slice(e.data.as_ref());
        buf
    }).collect();

    Block::new(height, node.parent_hash, 1_000, proposer, version, evm_tx_data)
}

fn bench_block_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("node/block_execution");

    for tx_count in [1, 10, 100].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(tx_count),
            &tx_count,
            |b, &count| {
                b.iter_batched(
                    || {
                        let (node, sender_addr) = setup_node();
                        let txs: Vec<EvmTransaction> = (0..count)
                            .map(|i| make_evm_tx(i as u64, sender_addr))
                            .collect();
                        let block = build_block(&node, txs);
                        let height = node.consensus.read().unwrap().current_height();
                        (node, block, height)
                    },
                    |(node, block, height)| {
                        let result = node
                            .state
                            .write_all()
                            .execute_block_no_subsystems(&block, height)
                            .expect("execution");
                        black_box(result);
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_block_execution);
criterion_main!(benches);
