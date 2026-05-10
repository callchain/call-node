//! Benchmark: Protocol security hot paths
//!
//! Measures ReplayProtector and RateLimiter throughput under load.

use call_primitives::{Address, TxHash};
use call_protocol::security::{RateLimiter, ReplayProtector};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

fn bench_replay_protector(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/replay_protector");

    for count in [100, 1_000, 10_000].iter().copied() {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &size| {
            b.iter_batched(
                || ReplayProtector::new(100_000),
                |mut protector| {
                    let mut accepted = 0usize;
                    for i in 0..size {
                        let hash = TxHash::repeat_byte((i % 256) as u8);
                        if protector.check_and_record(hash) {
                            accepted += 1;
                        }
                    }
                    black_box(accepted);
                },
                criterion::BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn bench_rate_limiter(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/rate_limiter");

    for count in [100, 1_000, 10_000].iter().copied() {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &size| {
            b.iter_batched(
                || RateLimiter::new(1_000, 60_000),
                |mut limiter| {
                    let mut allowed = 0usize;
                    for i in 0..size {
                        let addr = Address::repeat_byte((i % 256) as u8);
                        if limiter.allow(addr, i as u64 * 100) {
                            allowed += 1;
                        }
                    }
                    black_box(allowed);
                },
                criterion::BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_replay_protector, bench_rate_limiter);
criterion_main!(benches);
