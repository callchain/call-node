//! Benchmark: ZK proof generation for shielded transactions
//!
//! Measures deposit and transfer proof generation time using
//! the Halo2 IPA prover. Requires `halo2-prover` feature.

#[cfg(feature = "halo2-prover")]
use criterion::{black_box, criterion_group, criterion_main, Criterion};

#[cfg(feature = "halo2-prover")]
fn bench_proof_deposit(c: &mut Criterion) {
    use call_shielded::prover::Halo2Prover;
    use call_shielded::{
        circuit_deposit::{DepositCircuit, DepositWitness},
        poseidon::{bytes_to_fp, fp_to_bytes, poseidon_hash, poseidon_hash_tagged},
        ViewingKey,
    };

    let prover = Halo2Prover::setup();

    // Helper: compute commitment for a deposit note
    fn compute_commitment(value: u128, asset_id: u64, rcm: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let mut value_bytes = [0u8; 32];
        value_bytes[..16].copy_from_slice(&value.to_le_bytes());
        let value_fp = bytes_to_fp(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fp = bytes_to_fp(&asset_bytes);
        let rcm_fp = bytes_to_fp(rcm);
        let rho_fp = bytes_to_fp(rho);
        let cm_fp = poseidon_hash(&[value_fp, asset_fp, rcm_fp, rho_fp]);
        fp_to_bytes(&cm_fp)
    }

    c.bench_function("shielded/proof_deposit", |b| {
        b.iter(|| {
            let sk = [1u8; 32];
            let vk = ViewingKey::generate(&sk);
            let rho = [2u8; 32];
            let value = 1000u128;
            let asset_id = 1u64;

            // Compute RCM deterministically
            let ivk_fp = bytes_to_fp(&vk.incoming_view_key);
            let mut value_bytes = [0u8; 32];
            value_bytes[..16].copy_from_slice(&value.to_le_bytes());
            let value_fp = bytes_to_fp(&value_bytes);
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
            let asset_fp = bytes_to_fp(&asset_bytes);
            let rho_fp = bytes_to_fp(&rho);
            let rcm_fp = poseidon_hash_tagged("rcm", &[ivk_fp, value_fp, asset_fp, rho_fp]);
            let rcm = fp_to_bytes(&rcm_fp);

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

#[cfg(feature = "halo2-prover")]
fn bench_proof_transfer(c: &mut Criterion) {
    use call_shielded::prover::Halo2Prover;
    use call_shielded::{
        circuit_transfer::{InputNoteWitness, OutputNoteWitness, TransferCircuit},
        merkle_poseidon::PoseidonMerkleTree,
        poseidon::{bytes_to_fp, fp_to_bytes, poseidon_hash, poseidon_hash_tagged},
        ViewingKey,
    };

    let prover = Halo2Prover::setup();

    fn value_to_fp_bytes(value: u128) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&value.to_le_bytes());
        bytes
    }

    fn compute_rcm(vk: &ViewingKey, value: u128, asset_id: u64, rho: &[u8; 32]) -> [u8; 32] {
        let ivk_fp = bytes_to_fp(&vk.incoming_view_key);
        let value_fp = bytes_to_fp(&value_to_fp_bytes(value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fp = bytes_to_fp(&asset_bytes);
        let rho_fp = bytes_to_fp(rho);
        let rcm_fp = poseidon_hash_tagged("rcm", &[ivk_fp, value_fp, asset_fp, rho_fp]);
        fp_to_bytes(&rcm_fp)
    }

    fn compute_commitment(value: u128, asset_id: u64, rcm: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let value_fp = bytes_to_fp(&value_to_fp_bytes(value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fp = bytes_to_fp(&asset_bytes);
        let rcm_fp = bytes_to_fp(rcm);
        let rho_fp = bytes_to_fp(rho);
        let cm_fp = poseidon_hash(&[value_fp, asset_fp, rcm_fp, rho_fp]);
        fp_to_bytes(&cm_fp)
    }

    fn derive_nullifier(ivk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let fvk_tag = bytes_to_fp(&{
            let mut b = [0u8; 32];
            b[..16].copy_from_slice("fvk_from_ivk".as_bytes());
            b
        });
        let ivk_fp = bytes_to_fp(ivk);
        let rho_fp = bytes_to_fp(rho);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fp]);
        let nf_fp = poseidon_hash(&[fvk_from_ivk, rho_fp]);
        fp_to_bytes(&nf_fp)
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

            let mut nullifiers_arr = [[0u8; 32]; 2];
            nullifiers_arr[0] = nullifier;
            let mut commitments_arr = [[0u8; 32]; 2];
            commitments_arr[0] = output_cm;
            let mut input_notes_arr = [InputNoteWitness {
                value: 0,
                rcm: [0u8; 32],
                recipient_ivk: [0u8; 32],
                rho: [0u8; 32],
                spending_key: [0u8; 32],
            }; 2];
            input_notes_arr[0] = input_witness;
            let mut output_notes_arr = [OutputNoteWitness {
                value: 0,
                rcm: [0u8; 32],
                recipient_ivk: [0u8; 32],
                rho: [0u8; 32],
            }; 2];
            output_notes_arr[0] = output_witness;
            let mut merkle_paths_arr = [([0u8; 32], false); 32];
            for (i, p) in merkle_path.iter().enumerate() {
                merkle_paths_arr[i] = *p;
            }
            let mut merkle_paths = [[([0u8; 32], false); 32]; 2];
            merkle_paths[0] = merkle_paths_arr;

            let circuit = TransferCircuit::new(
                nullifiers_arr,
                commitments_arr,
                asset_id,
                merkle_root,
                input_notes_arr,
                output_notes_arr,
                merkle_paths,
            );

            let proof = prover.prove_transfer(&circuit);
            black_box(proof);
        });
    });
}

#[cfg(feature = "halo2-prover")]
criterion_group!(benches, bench_proof_deposit, bench_proof_transfer);
#[cfg(feature = "halo2-prover")]
criterion_main!(benches);

#[cfg(not(feature = "halo2-prover"))]
fn main() {
    eprintln!("This benchmark requires the `halo2-prover` feature.");
    eprintln!(
        "Run with: cargo bench -p call-shielded --bench proof_generate --features halo2-prover"
    );
    std::process::exit(1);
}
