//! Benchmark: Block production and execution (per spec §19)
//!
//! Measures block construction time and execution throughput with varying
//! transaction counts.

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use call_consensus::{Block, BlockContext, ConsensusParams, ExecutionState, SimplexConsensus, Subsystems};
use call_primitives::{Address, BlockHash, ValidatorId};
use call_protocol::{
    instructions::Instruction,
    transaction::{AuthScheme, GasConfig, ProtocolTransaction},
};
use call_evm::EvmState;

fn setup_consensus() -> SimplexConsensus {
    let mut consensus = SimplexConsensus::new(
        ConsensusParams::default(),
        call_consensus::ValidatorStateManager::default(),
    );
    // Stake a validator so there is a proposer
    consensus.stake_validator(Address::repeat_byte(1), [1u8; 32], 1_000_000u128).unwrap();
    consensus.refresh_proposer_subset();
    consensus
}

fn make_txs(count: usize) -> Vec<ProtocolTransaction> {
    (0..count)
        .map(|i| ProtocolTransaction {
            sender: Address::repeat_byte(1),
            nonce: i as u64,
            instructions: vec![Instruction::Transfer {
                asset_id: 1,
                to: Address::repeat_byte(((i % 255) + 2) as u8),
                amount: 1_000u128,
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            max_priority_fee: 1,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: [0u8; 65] },
        })
        .collect()
}

fn bench_block_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_production/execute");

    for tx_count in [1, 10, 50, 100].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(tx_count),
            &tx_count,
            |b, &count| {
                let protocol_txs = make_txs(count);
                let version = call_primitives::ProtocolVersion::new(0, 1, 0);

                b.iter_batched(
                    || {
                        let mut evm_state = EvmState::new();
                        let mut fee_params = call_protocol::transaction::FeeParams::default();
                        let bridge_config = call_bridge::BridgeConfig::default();

                        (evm_state, fee_params, bridge_config)
                    },
                    |(mut evm_state, mut fee_params, bridge_config)| {
                        let mut block = Block::new(
                            1,
                            BlockHash::ZERO,
                            1_000_000,
                            1,
                            version,
                            protocol_txs.clone(),
                            vec![],
                            vec![],
                        );
                        let result = block.execute(
                            &mut ExecutionState::new(
                                &mut call_shielded::ShieldedState::new(),
                                &mut evm_state,
                            ),
                            &mut BlockContext {
                                current_block_height: 1,
                                fee_params: &mut fee_params,
                                bridge_config: Some(&bridge_config),
                                validators: None,
                            },
                            &mut Subsystems::none(),
                        );
                        black_box(result);
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_block_execution);
criterion_main!(benches);
