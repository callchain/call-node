//! Benchmark: MDBX read/write latency
//!
//! Measures key-value put/get/delete throughput and p99 latency
//! using the reth-db MDBX backend.

use call_storage::{db_get, db_put, open_test_db, CallMetadataChainId};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

fn bench_db_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage/mdbx_write");

    for kv_count in [100, 1_000].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(kv_count),
            &kv_count,
            |b, &count| {
                b.iter_batched(
                    || open_test_db().expect("open test db"),
                    |db| {
                        let mut ok = 0usize;
                        for i in 0..count {
                            let key = format!("key_{:08}", i);
                            let value = format!("value_{}", i).repeat(10);
                            if db_put::<CallMetadataChainId>(
                                &db.db,
                                key.into_bytes(),
                                value.into_bytes(),
                            )
                            .is_ok()
                            {
                                ok += 1;
                            }
                        }
                        black_box(ok);
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

fn bench_db_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage/mdbx_read");

    for kv_count in [100, 1_000].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(kv_count),
            &kv_count,
            |b, &count| {
                let db = open_test_db().expect("open test db");
                // Pre-populate
                {
                    for i in 0..count {
                        let key = format!("key_{:08}", i);
                        let value = format!("value_{}", i).repeat(10);
                        let _ = db_put::<CallMetadataChainId>(
                            &db.db,
                            key.into_bytes(),
                            value.into_bytes(),
                        );
                    }
                }

                b.iter(|| {
                    let mut found = 0usize;
                    for i in 0..count {
                        let key = format!("key_{:08}", i);
                        if db_get::<CallMetadataChainId>(&db.db, key.as_bytes())
                            .unwrap_or(None)
                            .is_some()
                        {
                            found += 1;
                        }
                    }
                    black_box(found);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_db_write, bench_db_read);
criterion_main!(benches);
