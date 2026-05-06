//! Benchmark: Signature verification throughput
//!
//! Measures Ed25519 and secp256k1 verify performance, including batch
//! verification scenarios.

use call_crypto::{
    ed25519_generate_keypair, ed25519_sign, ed25519_verify, generate_keypair, keccak256,
    secp256k1_sign, secp256k1_verify,
};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

fn bench_ed25519_single(c: &mut Criterion) {
    let (pubkey, signing_key) = ed25519_generate_keypair();
    let msg = b"benchmark message for ed25519";
    let sig = ed25519_sign(&signing_key, msg);

    c.bench_function("crypto/ed25519_verify_single", |b| {
        b.iter(|| {
            black_box(ed25519_verify(&pubkey, &sig, msg).unwrap());
        });
    });
}

fn bench_ed25519_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto/ed25519_verify_batch");

    for batch_size in [10, 100, 1_000].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &size| {
                let items: Vec<_> = (0..size)
                    .map(|_| {
                        let (pk, sk) = ed25519_generate_keypair();
                        let msg = keccak256(&rand::random::<[u8; 32]>());
                        let sig = ed25519_sign(&sk, msg.as_slice());
                        (pk, sig, msg)
                    })
                    .collect();

                b.iter(|| {
                    let mut ok = 0usize;
                    for (pk, sig, msg) in &items {
                        if ed25519_verify(pk, sig, msg.as_slice()).is_ok() {
                            ok += 1;
                        }
                    }
                    black_box(ok);
                });
            },
        );
    }
    group.finish();
}

fn bench_secp256k1_single(c: &mut Criterion) {
    let (secret, pubkey) = generate_keypair();
    let msg_hash = [42u8; 32];
    let sig = secp256k1_sign(&secret, &msg_hash);

    c.bench_function("crypto/secp256k1_verify_single", |b| {
        b.iter(|| {
            black_box(secp256k1_verify(&pubkey, &sig, &msg_hash).unwrap());
        });
    });
}

fn bench_secp256k1_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto/secp256k1_verify_batch");

    for batch_size in [10, 100, 1_000].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &size| {
                let items: Vec<_> = (0..size)
                    .map(|_| {
                        let (secret, pubkey) = generate_keypair();
                        let msg_hash = keccak256(&rand::random::<[u8; 32]>());
                        let sig = secp256k1_sign(&secret, msg_hash.as_ref());
                        (pubkey, sig, msg_hash)
                    })
                    .collect();

                b.iter(|| {
                    let mut ok = 0usize;
                    for (pk, sig, msg) in &items {
                        if secp256k1_verify(pk, sig, msg.as_ref()).is_ok() {
                            ok += 1;
                        }
                    }
                    black_box(ok);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_ed25519_single,
    bench_ed25519_batch,
    bench_secp256k1_single,
    bench_secp256k1_batch
);
criterion_main!(benches);
