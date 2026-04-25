//! Benchmark: Instruction execution throughput (per spec §3.6)
//!
//! Measures hot-path instruction execution: Transfer, BatchTransfer, and
//! mixed workloads at varying batch sizes.

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use call_primitives::{Address, AssetId, Balance};
use call_protocol::{
    Instruction, PaymentMemo,
    AccountState,
    registry::AssetRegistry,
    compliance::ComplianceEngine,
    instructions::execute_instruction,
};
use call_shielded::ShieldedState;

fn setup_state() -> (AccountState, AssetRegistry, ComplianceEngine, ShieldedState) {
    let mut account = AccountState::new();
    let mut registry = AssetRegistry::new();
    let compliance = ComplianceEngine::new();
    let shielded = ShieldedState::new();

    // Register CALL asset
    registry.register_asset("CALL".into(), "Call Token".into(), 18, Address::ZERO, 0, 0, 0).unwrap();

    // Seed sender balance
    let sender = Address::repeat_byte(1);
    account.balances.set_balance(1, sender, 1_000_000_000_000u128).unwrap();

    (account, registry, compliance, shielded)
}

fn bench_transfer(c: &mut Criterion) {
    let mut group = c.benchmark_group("instruction_execute/transfer");

    for batch_size in [1, 10, 100].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &size| {
                b.iter_batched(
                    setup_state,
                    |(mut account, mut registry, mut compliance, mut shielded)| {
                        let sender = Address::repeat_byte(1);
                        let mut total = 0u64;
                        for i in 0..size {
                            let instr = Instruction::Transfer {
                                asset_id: 1,
                                to: Address::repeat_byte((i % 255 + 2) as u8),
                                amount: 1_000u128,
                                memo: None,
                            };
                            let _ = execute_instruction(
                                &instr,
                                &mut account,
                                &mut registry,
                                &mut compliance,
                                &mut shielded,
                                sender,
                                None,
                                &mut None,
                                None,
                            );
                            total += 1;
                        }
                        black_box(total);
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_batch_transfer(c: &mut Criterion) {
    let mut group = c.benchmark_group("instruction_execute/batch_transfer");

    for payment_count in [1, 10, 50, 100].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(payment_count),
            &payment_count,
            |b, &count| {
                let payments: Vec<_> = (0..count)
                    .map(|i| call_protocol::PaymentEntry {
                        to: Address::repeat_byte((i % 255 + 2) as u8),
                        amount: 1_000u128,
                        memo: None,
                    })
                    .collect();

                let instr = Instruction::BatchTransfer {
                    asset_id: 1,
                    payments,
                };

                b.iter_batched(
                    setup_state,
                    |(mut account, mut registry, mut compliance, mut shielded)| {
                        let sender = Address::repeat_byte(1);
                        let result = execute_instruction(
                            &instr,
                            &mut account,
                            &mut registry,
                            &mut compliance,
                            &mut shielded,
                            sender,
                            None,
                            &mut None,
                            None,
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

fn bench_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("instruction_execute/mixed");

    group.bench_function("transfer_approve_transferfrom", |b| {
        let sender = Address::repeat_byte(1);
        let spender = Address::repeat_byte(2);
        let recipient = Address::repeat_byte(3);

        let instructions = vec![
            Instruction::Transfer { asset_id: 1, to: spender, amount: 100_000u128, memo: None },
            Instruction::Approve { asset_id: 1, spender, amount: 50_000u128 },
            Instruction::TransferFrom { asset_id: 1, from: sender, to: recipient, amount: 25_000u128 },
        ];

        b.iter_batched(
            setup_state,
            |(mut account, mut registry, mut compliance, mut shielded)| {
                let mut results = Vec::with_capacity(instructions.len());
                for instr in &instructions {
                    let r = execute_instruction(
                        instr,
                        &mut account,
                        &mut registry,
                        &mut compliance,
                        &mut shielded,
                        sender,
                        None,
                        &mut None,
                        None,
                    );
                    results.push(r);
                }
                black_box(results);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_transfer, bench_batch_transfer, bench_mixed_workload);
criterion_main!(benches);
