//! Benchmark: ZK proof generation for shielded transactions
//!
//! Measures deposit and transfer proof generation time using
//! the real Groth16 prover. Requires `real-prover` feature.

#[cfg(feature = "real-prover")]
use criterion::{black_box, criterion_group, criterion_main, Criterion};

#[cfg(feature = "real-prover")]
fn bench_proof_deposit(c: &mut Criterion) {
    use call_shielded::{
        circuit_deposit::{DepositCircuit, DepositWitness},
        poseidon::{bytes_to_fr, fr_to_bytes, poseidon_hash, poseidon_hash_tagged},
        ViewingKey,
    };
    use call_shielded::prover::RealProver;

    let prover = RealProver::setup();

    // Helper: compute commitment for a deposit note
    fn compute_commitment(value: u128, asset_id: u64, rcm: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let mut value_bytes = [0u8; 32];
        value_bytes[..16].copy_from_slice(&value.to_le_bytes());
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rcm_fr = bytes_to_fr(rcm);
        let rho_fr = bytes_to_fr(rho);
        let cm_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
        fr_to_bytes(&cm_fr)
    }

    c.bench_function("shielded/proof_deposit", |b| {
        b.iter(|| {
            let sk = [1u8; 32];
            let vk = ViewingKey::generate(&sk);
            let rho = [2u8; 32];
            let value = 1000u128;
            let asset_id = 1u64;

            // Compute RCM deterministically
            let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
            let mut value_bytes = [0u8; 32];
            value_bytes[..16].copy_from_slice(&value.to_le_bytes());
            let value_fr = bytes_to_fr(&value_bytes);
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
            let asset_fr = bytes_to_fr(&asset_bytes);
            let rho_fr = bytes_to_fr(&rho);
            let rcm_fr = poseidon_hash_tagged("rcm", &[ivk_fr, value_fr, asset_fr, rho_fr]);
            let rcm = fr_to_bytes(&rcm_fr);

            let commitment = compute_commitment(value, asset_id, &rcm, &rho);

            let witness = DepositWitness {
                value,
                rcm,
                recipient_ivk: vk.incoming_view_key,
                rho,
            };

            let circuit = DepositCircuit::new(commitment, asset_id, witness);
            let proof = prover.prove_deposit(&circuit);
            black_box(proof);
        });
    });
}

#[cfg(feature = "real-prover")]
fn bench_proof_transfer(c: &mut Criterion) {
    use call_shielded::{
        circuit_transfer::{TransferCircuit, InputNoteWitness, OutputNoteWitness},
        merkle_poseidon::PoseidonMerkleTree,
        poseidon::{bytes_to_fr, fr_to_bytes, poseidon_hash, poseidon_hash_tagged},
        ViewingKey,
    };
    use call_shielded::prover::RealProver;

    let prover = RealProver::setup();

    fn value_to_fr_bytes(value: u128) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&value.to_le_bytes());
        bytes
    }

    fn compute_rcm(vk: &ViewingKey, value: u128, asset_id: u64, rho: &[u8; 32]) -> [u8; 32] {
        let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
        let value_fr = bytes_to_fr(&value_to_fr_bytes(value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rho_fr = bytes_to_fr(rho);
        let rcm_fr = poseidon_hash_tagged("rcm", &[ivk_fr, value_fr, asset_fr, rho_fr]);
        fr_to_bytes(&rcm_fr)
    }

    fn compute_commitment(value: u128, asset_id: u64, rcm: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let value_fr = bytes_to_fr(&value_to_fr_bytes(value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rcm_fr = bytes_to_fr(rcm);
        let rho_fr = bytes_to_fr(rho);
        let cm_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
        fr_to_bytes(&cm_fr)
    }

    fn derive_nullifier(ivk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let fvk_tag = bytes_to_fr(&{
            let mut b = [0u8; 32];
            b[..16].copy_from_slice("fvk_from_ivk".as_bytes());
            b
        });
        let ivk_fr = bytes_to_fr(ivk);
        let rho_fr = bytes_to_fr(rho);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
        let nf_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);
        fr_to_bytes(&nf_fr)
    }

    c.bench_function("shielded/proof_transfer", |b| {
        b.iter(|| {
            let asset_id: u64 = 1;

            let sk_in = [1u8; 32];
            let vk_in = ViewingKey::generate(&sk_in);
            let rho_in = [10u8; 32];
            let rcm_in = compute_rcm(&vk_in, 1000, asset_id, &rho_in);
            let nullifier = derive_nullifier(&vk_in.incoming_view_key, &rho_in);
            let input_cm = compute_commitment(1000, asset_id, &rcm_in, &rho_in);

            let mut tree = PoseidonMerkleTree::new(32);
            tree.insert(&input_cm);
            let merkle_root = tree.root();
            let merkle_path = tree.proof_for_last();

            let input_witness = InputNoteWitness {
                value: 1000,
                rcm: rcm_in,
                recipient_ivk: vk_in.incoming_view_key,
                rho: rho_in,
                spending_key: sk_in,
            };

            let sk_out = [2u8; 32];
            let vk_out = ViewingKey::generate(&sk_out);
            let rho_out = [20u8; 32];
            let rcm_out = compute_rcm(&vk_out, 900, asset_id, &rho_out);
            let output_cm = compute_commitment(900, asset_id, &rcm_out, &rho_out);

            let output_witness = OutputNoteWitness {
                value: 900,
                rcm: rcm_out,
                recipient_ivk: vk_out.incoming_view_key,
                rho: rho_out,
            };

            let circuit = TransferCircuit::new(
                vec![nullifier],
                vec![output_cm],
                asset_id,
                merkle_root,
                vec![input_witness],
                vec![output_witness],
                vec![merkle_path],
            );

            let proof = prover.prove_transfer(&circuit);
            black_box(proof);
        });
    });
}

#[cfg(feature = "real-prover")]
criterion_group!(benches, bench_proof_deposit, bench_proof_transfer);
#[cfg(feature = "real-prover")]
criterion_main!(benches);

#[cfg(not(feature = "real-prover"))]
fn main() {
    eprintln!("This benchmark requires the `real-prover` feature.");
    eprintln!("Run with: cargo bench -p call-shielded --bench proof_generate --features real-prover");
    std::process::exit(1);
}
