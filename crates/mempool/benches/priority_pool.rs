//! Benchmark: Mempool Priority Pool hot paths
//!
//! Measures insert, drain_sorted, and evict_lowest throughput under
//! varying pool sizes. These are critical paths during block production.

use call_mempool::{MempoolEntry, PoolKind, PriorityPool};
use call_primitives::{Address, TxHash};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

fn make_entry(nonce: u64, score: u128, sender_byte: u8) -> MempoolEntry {
    MempoolEntry::new(
        TxHash::repeat_byte((nonce % 256) as u8),
        score,
        Address::repeat_byte(sender_byte),
        nonce,
        PoolKind::Evm,
        vec![sender_byte; 100],
        0,
    )
}

fn bench_pool_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("mempool/pool_insert");

    for pool_size in [100, 1_000, 10_000].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(pool_size),
            &pool_size,
            |b, &size| {
                b.iter_batched(
                    || PriorityPool::new(),
                    |mut pool| {
                        for i in 0..size {
                            let entry = make_entry(i as u64, i as u128, (i % 256) as u8);
                            pool.insert(entry);
                        }
                        black_box(pool.len());
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

fn bench_pool_drain_sorted(c: &mut Criterion) {
    let mut group = c.benchmark_group("mempool/pool_drain_sorted");

    for pool_size in [100, 1_000, 10_000].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(pool_size),
            &pool_size,
            |b, &size| {
                let mut pool = PriorityPool::new();
                for i in 0..size {
                    let entry = make_entry(i as u64, (i * 7) as u128, (i % 256) as u8);
                    pool.insert(entry);
                }

                b.iter(|| {
                    let drained = pool.drain_sorted();
                    black_box(drained.len());
                });
            },
        );
    }
    group.finish();
}

fn bench_pool_evict_lowest(c: &mut Criterion) {
    let mut group = c.benchmark_group("mempool/pool_evict_lowest");

    for pool_size in [100, 1_000, 10_000].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(pool_size),
            &pool_size,
            |b, &size| {
                b.iter_batched(
                    || {
                        let mut pool = PriorityPool::new();
                        for i in 0..size {
                            let entry = make_entry(i as u64, (i * 3) as u128, (i % 256) as u8);
                            pool.insert(entry);
                        }
                        pool
                    },
                    |mut pool| {
                        let mut evicted = 0usize;
                        while pool.evict_lowest().is_some() {
                            evicted += 1;
                        }
                        black_box(evicted);
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_pool_insert,
    bench_pool_drain_sorted,
    bench_pool_evict_lowest
);
criterion_main!(benches);
