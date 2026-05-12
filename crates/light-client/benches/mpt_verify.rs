//! Benchmark: MPT proof verification hot path
//!
//! Measures `verify_mpt_proof` throughput for leaf, extension, and branch
//! node combinations at varying proof depths.

use alloy_primitives::{keccak256, B256};
use call_light_client::{bytes_to_nibbles, verify_mpt_proof};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

// ── Inline helpers (benchmarks cannot access #[cfg(test)] items) ──

fn compact_encode(nibbles: &[u8], is_leaf: bool) -> Vec<u8> {
    let has_odd = nibbles.len() % 2 != 0;
    let first_byte = if has_odd {
        let type_low = if is_leaf { 3 } else { 1 };
        (nibbles[0] << 4) | type_low
    } else if is_leaf {
        0x20
    } else {
        0x00
    };
    let mut out = Vec::with_capacity(1 + nibbles.len() / 2 + 1);
    out.push(first_byte);
    let start = if has_odd { 1 } else { 0 };
    for i in (start..nibbles.len()).step_by(2) {
        if i + 1 < nibbles.len() {
            out.push((nibbles[i] << 4) | nibbles[i + 1]);
        }
    }
    out
}

fn rlp_encode_short_bytes(data: &[u8]) -> Vec<u8> {
    if data.len() == 1 && data[0] < 0x80 {
        data.to_vec()
    } else if data.len() < 56 {
        let mut out = Vec::with_capacity(1 + data.len());
        out.push(0x80 + data.len() as u8);
        out.extend(data);
        out
    } else {
        let len_bytes = data.len().to_be_bytes();
        let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(len_bytes.len());
        let num_len_bytes = len_bytes.len() - skip;
        let mut out = Vec::with_capacity(1 + num_len_bytes + data.len());
        out.push(0xB7 + num_len_bytes as u8);
        out.extend_from_slice(&len_bytes[skip..]);
        out.extend(data);
        out
    }
}

fn rlp_encode_list(items: &[Vec<u8>]) -> Vec<u8> {
    let total_len: usize = items.iter().map(|i| i.len()).sum();
    let mut out = Vec::with_capacity(1 + total_len);
    if total_len < 56 {
        out.push(0xC0 + total_len as u8);
    } else {
        let len_bytes = total_len.to_be_bytes();
        let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(len_bytes.len());
        let num_len_bytes = len_bytes.len() - skip;
        out.push(0xF7 + num_len_bytes as u8);
        out.extend_from_slice(&len_bytes[skip..]);
    }
    for item in items {
        out.extend(item);
    }
    out
}

fn make_leaf_node_rlp(key_nibbles: &[u8], value: &[u8]) -> Vec<u8> {
    let compact = compact_encode(key_nibbles, true);
    let compact_rlp = rlp_encode_short_bytes(&compact);
    let value_rlp = rlp_encode_short_bytes(value);
    rlp_encode_list(&[compact_rlp, value_rlp])
}

fn make_extension_node_rlp(key_nibbles: &[u8], child_hash: B256) -> Vec<u8> {
    let compact = compact_encode(key_nibbles, false);
    let compact_rlp = rlp_encode_short_bytes(&compact);
    let child_rlp = rlp_encode_short_bytes(&child_hash.0);
    rlp_encode_list(&[compact_rlp, child_rlp])
}

fn make_branch_node_rlp(children: &[Option<B256>; 16], value: Option<&[u8]>) -> Vec<u8> {
    let mut items = Vec::with_capacity(17);
    for i in 0..16 {
        if let Some(hash) = children[i] {
            items.push(rlp_encode_short_bytes(&hash.0));
        } else {
            items.push(vec![0x80]);
        }
    }
    if let Some(v) = value {
        items.push(rlp_encode_short_bytes(v));
    } else {
        items.push(vec![0x80]);
    }
    rlp_encode_list(&items)
}

// ── Proof builders ──

fn build_leaf_proof() -> (B256, Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
    let key_bytes = [0x12u8, 0x34];
    let key_nibbles = bytes_to_nibbles(&key_bytes);
    let value = b"hello";
    let leaf_rlp = make_leaf_node_rlp(&key_nibbles, value);
    let root = keccak256(&leaf_rlp);
    (root, key_bytes.to_vec(), vec![leaf_rlp], value.to_vec())
}

fn build_extension_leaf_proof() -> (B256, Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
    let key_bytes = [0x01u8, 0x02];
    let key_nibbles = bytes_to_nibbles(&key_bytes);
    let value = b"found";
    let leaf_key = &key_nibbles[1..];
    let leaf_rlp = make_leaf_node_rlp(leaf_key, value);
    let leaf_hash = keccak256(&leaf_rlp);
    let ext_rlp = make_extension_node_rlp(&[0], leaf_hash);
    let root = keccak256(&ext_rlp);
    (root, key_bytes.to_vec(), vec![ext_rlp, leaf_rlp], value.to_vec())
}

fn build_branch_leaf_proof() -> (B256, Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
    let key_bytes = [0x5Au8];
    let key_nibbles = bytes_to_nibbles(&key_bytes);
    let value = b"branch_val";
    let leaf_rlp = make_leaf_node_rlp(&key_nibbles[1..], value);
    let leaf_hash = keccak256(&leaf_rlp);
    let mut children: [Option<B256>; 16] = [None; 16];
    children[5] = Some(leaf_hash);
    let branch_rlp = make_branch_node_rlp(&children, None);
    let root = keccak256(&branch_rlp);
    (root, key_bytes.to_vec(), vec![branch_rlp, leaf_rlp], value.to_vec())
}

fn build_deep_proof() -> (B256, Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
    // Trie: root(extension [0]) -> branch -> leaf
    let key_bytes = [0x0Au8, 0x5Bu8];
    let key_nibbles = bytes_to_nibbles(&key_bytes);
    let value = b"deep_value";

    // Leaf at the bottom (remaining nibbles after ext [0] + branch [A])
    let leaf_rlp = make_leaf_node_rlp(&key_nibbles[2..], value);
    let leaf_hash = keccak256(&leaf_rlp);

    // Branch with child at nibble A (10)
    let mut children: [Option<B256>; 16] = [None; 16];
    children[10] = Some(leaf_hash);
    let branch_rlp = make_branch_node_rlp(&children, None);
    let branch_hash = keccak256(&branch_rlp);

    // Extension covering nibble [0]
    let ext_rlp = make_extension_node_rlp(&[0], branch_hash);
    let root = keccak256(&ext_rlp);

    (root, key_bytes.to_vec(), vec![ext_rlp, branch_rlp, leaf_rlp], value.to_vec())
}

// ── Benchmarks ──

fn bench_mpt_verify_leaf(c: &mut Criterion) {
    let (root, key, proof, _expected) = build_leaf_proof();
    c.bench_function("light_client/mpt_verify_leaf", |b| {
        b.iter(|| {
            let result = verify_mpt_proof(root, &key, &proof).unwrap();
            black_box(result);
        });
    });
}

fn bench_mpt_verify_extension_leaf(c: &mut Criterion) {
    let (root, key, proof, _expected) = build_extension_leaf_proof();
    c.bench_function("light_client/mpt_verify_extension_leaf", |b| {
        b.iter(|| {
            let result = verify_mpt_proof(root, &key, &proof).unwrap();
            black_box(result);
        });
    });
}

fn bench_mpt_verify_branch_leaf(c: &mut Criterion) {
    let (root, key, proof, _expected) = build_branch_leaf_proof();
    c.bench_function("light_client/mpt_verify_branch_leaf", |b| {
        b.iter(|| {
            let result = verify_mpt_proof(root, &key, &proof).unwrap();
            black_box(result);
        });
    });
}

fn bench_mpt_verify_deep(c: &mut Criterion) {
    let (root, key, proof, _expected) = build_deep_proof();
    c.bench_function("light_client/mpt_verify_deep", |b| {
        b.iter(|| {
            let result = verify_mpt_proof(root, &key, &proof).unwrap();
            black_box(result);
        });
    });
}

fn bench_mpt_verify_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("light_client/mpt_verify_batch");

    let proofs = vec![
        build_leaf_proof(),
        build_extension_leaf_proof(),
        build_branch_leaf_proof(),
        build_deep_proof(),
    ];

    for batch_size in [10, 100, 1_000].iter().copied() {
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &size| {
                b.iter(|| {
                    let mut ok = 0usize;
                    for i in 0..size {
                        let (root, key, proof, _) = &proofs[i % proofs.len()];
                        if verify_mpt_proof(*root, key, proof).unwrap().is_some() {
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
    bench_mpt_verify_leaf,
    bench_mpt_verify_extension_leaf,
    bench_mpt_verify_branch_leaf,
    bench_mpt_verify_deep,
    bench_mpt_verify_batch
);
criterion_main!(benches);
