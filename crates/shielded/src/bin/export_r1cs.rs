//! Export arkworks R1CS circuits to snarkjs-compatible `.r1cs` files.
//!
//! snarkjs Phase 2 derivation requires `.r1cs` files, but arkworks generates
//! constraints in-memory via `ConstraintSynthesizer`. This tool bridges the gap
//! by running the arkworks circuit setup and writing the constraint matrices
//! in snarkjs binary format.
//!
//! Usage:
//!   cargo run --release --features real-prover --bin export_r1cs
//!
//! Output:
//!   r1cs/deposit.r1cs
//!   r1cs/transfer.r1cs
//!   r1cs/withdraw.r1cs
//!
//! These files are then used by `phase2_derive.sh` for key generation.

use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use ark_relations::r1cs::{ConstraintMatrices, ConstraintSynthesizer, ConstraintSystem, SynthesisMode};
use ark_std::io::Write;
use std::fs::File;
use std::path::Path;

use call_shielded::circuit_deposit::DepositCircuit;
use call_shielded::circuit_transfer::TransferCircuit;
use call_shielded::circuit_withdraw::WithdrawCircuit;

/// Write R1CS matrices to snarkjs binary format.
///
/// snarkjs `.r1cs` format (v2):
///   - Header: magic + version + wires, pub_out, pub_inputs, prv_inputs, labels, constraints
///   - Wire ID map (labels)
///   - Constraint matrices: A, B, C as sparse matrices
fn write_r1cs_binary(
    matrices: &ConstraintMatrices<Fr>,
    n_wires: usize,
    output_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let n_pub_out = matrices.num_instance_variables - 1; // exclude constant term
    let n_pub_inputs = matrices.num_instance_variables - 1;
    let n_prv_inputs = n_wires - matrices.num_instance_variables;
    let n_labels = n_wires as u64;
    let m_constraints = matrices.num_constraints;

    println!("  Wires:       {}", n_wires);
    println!("  Public outs: {}", n_pub_out);
    println!("  Pub inputs:  {}", n_pub_inputs);
    println!("  Priv inputs: {}", n_prv_inputs);
    println!("  Constraints: {}", m_constraints);

    let mut file = File::create(output_path)?;

    // Magic: "r1cs"
    file.write_all(b"r1cs")?;
    // Version: 2
    file.write_all(&2u32.to_le_bytes())?;
    // Number of sections: 3 (header, wire labels, constraints)
    file.write_all(&3u32.to_le_bytes())?;

    // Section 1: Header
    let section_id = 1u32;
    let section_size = 8 * 6; // 6 u64 fields
    file.write_all(&section_id.to_le_bytes())?;
    file.write_all(&(section_size as u64).to_le_bytes())?;
    file.write_all(&(n_wires as u64).to_le_bytes())?;
    file.write_all(&(n_pub_out as u64).to_le_bytes())?;
    file.write_all(&(n_pub_inputs as u64).to_le_bytes())?;
    file.write_all(&(n_prv_inputs as u64).to_le_bytes())?;
    file.write_all(&n_labels.to_le_bytes())?;
    file.write_all(&(m_constraints as u64).to_le_bytes())?;

    // Section 2: Wire ID map (labels)
    let section_id = 2u32;
    let section_size = n_wires * 8;
    file.write_all(&section_id.to_le_bytes())?;
    file.write_all(&(section_size as u64).to_le_bytes())?;
    for i in 0..n_wires {
        file.write_all(&(i as u64).to_le_bytes())?;
    }

    // Section 3: Constraint matrices (sparse format)
    // Each constraint has 3 matrices (A, B, C), each is a sparse vector of (wire_index, coefficient)
    let mut constraint_data: Vec<u8> = Vec::new();

    for constraint_idx in 0..m_constraints {
        let a_entries = matrices.a.get(constraint_idx).map(|v| v.as_slice()).unwrap_or(&[]);
        let b_entries = matrices.b.get(constraint_idx).map(|v| v.as_slice()).unwrap_or(&[]);
        let c_entries = matrices.c.get(constraint_idx).map(|v| v.as_slice()).unwrap_or(&[]);

        // Write A entries — matrix format is Vec<Vec<(F, usize)>>: (coeff, wire_idx)
        constraint_data.write_all(&(a_entries.len() as u64).to_le_bytes())?;
        for &(ref coeff, wire_idx) in a_entries {
            constraint_data.write_all(&(wire_idx as u64).to_le_bytes())?;
            let coeff_bytes = coeff.into_bigint().to_bytes_le();
            constraint_data.write_all(&(coeff_bytes.len() as u32).to_le_bytes())?;
            constraint_data.write_all(&coeff_bytes)?;
        }

        // Write B entries
        constraint_data.write_all(&(b_entries.len() as u64).to_le_bytes())?;
        for &(ref coeff, wire_idx) in b_entries {
            constraint_data.write_all(&(wire_idx as u64).to_le_bytes())?;
            let coeff_bytes = coeff.into_bigint().to_bytes_le();
            constraint_data.write_all(&(coeff_bytes.len() as u32).to_le_bytes())?;
            constraint_data.write_all(&coeff_bytes)?;
        }

        // Write C entries
        constraint_data.write_all(&(c_entries.len() as u64).to_le_bytes())?;
        for &(ref coeff, wire_idx) in c_entries {
            constraint_data.write_all(&(wire_idx as u64).to_le_bytes())?;
            let coeff_bytes = coeff.into_bigint().to_bytes_le();
            constraint_data.write_all(&(coeff_bytes.len() as u32).to_le_bytes())?;
            constraint_data.write_all(&coeff_bytes)?;
        }
    }

    let section_size = constraint_data.len() as u64;
    let section_id = 3u32;
    file.write_all(&section_id.to_le_bytes())?;
    file.write_all(&section_size.to_le_bytes())?;
    file.write_all(&constraint_data)?;

    println!("  Written to: {}", output_path.display());
    Ok(())
}

/// Helper to synthesize constraints from a circuit and extract matrices.
fn synthesize<C: ConstraintSynthesizer<Fr>>(
    circuit: C,
) -> Result<(ConstraintMatrices<Fr>, usize), Box<dyn std::error::Error>> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    cs.set_mode(SynthesisMode::Setup);

    circuit.generate_constraints(cs.clone())?;
    cs.finalize();

    let matrices = cs.to_matrices().expect("CS should have matrices");
    let n_wires = cs.num_instance_variables() + cs.num_witness_variables();
    Ok((matrices, n_wires))
}

/// Build a deposit circuit with witness data for constraint synthesis.
fn build_deposit_circuit() -> DepositCircuit {
    use call_shielded::circuit_deposit::DepositWitness;
    use call_shielded::poseidon::{bytes_to_fr, fr_to_bytes, poseidon_hash};
    use call_shielded::ViewingKey;

    let sk = { let mut k = [0u8; 32]; k[0] = 1; k };
    let vk = ViewingKey::generate(&sk);
    let rho = { let mut h = [0u8; 32]; h[0] = 1; h };
    let value: u128 = 1000;
    let asset_id: u64 = 1;

    // Compute rcm = poseidon([rcm_tag, ivk, value, asset, rho])
    let mut rcm_tag = [0u8; 32];
    rcm_tag[..3].copy_from_slice(b"rcm");
    let rcm_tag_fr = bytes_to_fr(&rcm_tag);
    let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
    let mut value_bytes = [0u8; 32];
    value_bytes[..16].copy_from_slice(&value.to_le_bytes());
    let value_fr = bytes_to_fr(&value_bytes);
    let mut asset_bytes = [0u8; 32];
    asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
    let asset_fr = bytes_to_fr(&asset_bytes);
    let rho_fr = bytes_to_fr(&rho);
    let rcm_fr = poseidon_hash(&[rcm_tag_fr, ivk_fr, value_fr, asset_fr, rho_fr]);
    let rcm = fr_to_bytes(&rcm_fr);

    let witness = DepositWitness { value, rcm, recipient_ivk: vk.incoming_view_key, rho };

    // Compute commitment
    let cm_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
    let commitment = fr_to_bytes(&cm_fr);

    DepositCircuit::new(commitment, asset_id, witness)
}

/// Build a withdraw circuit with witness data.
fn build_withdraw_circuit() -> WithdrawCircuit {
    use call_shielded::circuit_withdraw::WithdrawWitness;
    use call_shielded::poseidon::{bytes_to_fr, fr_to_bytes, poseidon_hash, domain};
    use call_shielded::merkle_poseidon::PoseidonMerkleTree;
    use call_shielded::ViewingKey;

    let sk = { let mut k = [0u8; 32]; k[0] = 1; k };
    let vk = ViewingKey::generate(&sk);
    let rho = { let mut h = [0u8; 32]; h[0] = 1; h };
    let value: u128 = 500;
    let asset_id: u64 = 1;

    let mut rcm_tag = [0u8; 32];
    rcm_tag[..3].copy_from_slice(b"rcm");
    let rcm_tag_fr = bytes_to_fr(&rcm_tag);
    let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
    let mut value_bytes = [0u8; 32];
    value_bytes[..16].copy_from_slice(&value.to_le_bytes());
    let value_fr = bytes_to_fr(&value_bytes);
    let mut asset_bytes = [0u8; 32];
    asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
    let asset_fr = bytes_to_fr(&asset_bytes);
    let rho_fr = bytes_to_fr(&rho);
    let rcm_fr = poseidon_hash(&[rcm_tag_fr, ivk_fr, value_fr, asset_fr, rho_fr]);
    let rcm = fr_to_bytes(&rcm_fr);

    // Nullifier
    let mut domain_bytes = [0u8; 32];
    domain_bytes[..domain::FVK_FROM_IVK.len()].copy_from_slice(domain::FVK_FROM_IVK.as_bytes());
    let fvk_tag = bytes_to_fr(&domain_bytes);
    let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
    let nf_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);
    let nullifier = fr_to_bytes(&nf_fr);

    // Commitment
    let cm_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
    let commitment = fr_to_bytes(&cm_fr);

    let mut tree = PoseidonMerkleTree::new(32);
    tree.insert(&commitment);
    let merkle_root = tree.root();
    let merkle_path = tree.proof_for_last();

    let witness = WithdrawWitness {
        note_value: value, rcm, recipient_ivk: vk.incoming_view_key, rho, merkle_path,
    };
    let target_address = [1u8; 20];

    WithdrawCircuit::new(nullifier, asset_id, value, target_address, merkle_root, witness)
}

/// Build a transfer circuit with witness data.
fn build_transfer_circuit() -> TransferCircuit {
    use call_shielded::circuit_transfer::{InputNoteWitness, OutputNoteWitness};
    use call_shielded::poseidon::{bytes_to_fr, fr_to_bytes, poseidon_hash, domain};
    use call_shielded::merkle_poseidon::PoseidonMerkleTree;
    use call_shielded::ViewingKey;

    let asset_id: u64 = 1;

    let sk = { let mut k = [0u8; 32]; k[0] = 1; k };
    let vk = ViewingKey::generate(&sk);
    let rho_in = { let mut h = [0u8; 32]; h[0] = 10; h };

    // Input note
    let mut value_bytes = [0u8; 32];
    value_bytes[..16].copy_from_slice(&1000u128.to_le_bytes());
    let value_fr = bytes_to_fr(&value_bytes);
    let mut asset_bytes = [0u8; 32];
    asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
    let asset_fr = bytes_to_fr(&asset_bytes);
    let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
    let rho_fr = bytes_to_fr(&rho_in);
    let mut rcm_tag = [0u8; 32];
    rcm_tag[..3].copy_from_slice(b"rcm");
    let rcm_tag_fr = bytes_to_fr(&rcm_tag);
    let rcm_fr = poseidon_hash(&[rcm_tag_fr, ivk_fr, value_fr, asset_fr, rho_fr]);
    let rcm_in = fr_to_bytes(&rcm_fr);

    // Nullifier
    let mut domain_bytes = [0u8; 32];
    domain_bytes[..domain::FVK_FROM_IVK.len()].copy_from_slice(domain::FVK_FROM_IVK.as_bytes());
    let fvk_tag = bytes_to_fr(&domain_bytes);
    let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
    let nf_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);
    let nullifier = fr_to_bytes(&nf_fr);

    let input_cm_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
    let input_cm = fr_to_bytes(&input_cm_fr);

    let mut tree = PoseidonMerkleTree::new(32);
    tree.insert(&input_cm);
    let merkle_root = tree.root();
    let merkle_path = tree.proof_for_last();

    let input_witness = InputNoteWitness {
        value: 1000, rcm: rcm_in, recipient_ivk: vk.incoming_view_key,
        rho: rho_in, spending_key: sk,
    };

    // Output note
    let out_sk = { let mut k = [0u8; 32]; k[0] = 2; k };
    let out_vk = ViewingKey::generate(&out_sk);
    let rho_out = { let mut h = [0u8; 32]; h[0] = 20; h };
    let mut out_value_bytes = [0u8; 32];
    out_value_bytes[..16].copy_from_slice(&900u128.to_le_bytes());
    let out_value_fr = bytes_to_fr(&out_value_bytes);
    let out_ivk_fr = bytes_to_fr(&out_vk.incoming_view_key);
    let rho_out_fr = bytes_to_fr(&rho_out);
    let out_rcm_fr = poseidon_hash(&[rcm_tag_fr, out_ivk_fr, out_value_fr, asset_fr, rho_out_fr]);
    let rcm_out = fr_to_bytes(&out_rcm_fr);

    let output_cm_fr = poseidon_hash(&[out_value_fr, asset_fr, out_rcm_fr, rho_out_fr]);
    let output_cm = fr_to_bytes(&output_cm_fr);

    let output_witness = OutputNoteWitness {
        value: 900, rcm: rcm_out, recipient_ivk: out_vk.incoming_view_key, rho: rho_out,
    };

    TransferCircuit::new(
        vec![nullifier], vec![output_cm], asset_id, merkle_root,
        vec![input_witness], vec![output_witness], vec![merkle_path],
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output_dir = Path::new("r1cs");
    std::fs::create_dir_all(output_dir)?;

    println!("=== Exporting R1CS circuits for snarkjs Phase 2 ===");
    println!("");

    // --- Deposit ---
    println!("=== Exporting deposit circuit ===");
    let circuit = build_deposit_circuit();
    let (matrices, n_wires) = synthesize(circuit)?;
    write_r1cs_binary(&matrices, n_wires, &output_dir.join("deposit.r1cs"))?;
    println!("");

    // --- Withdraw ---
    println!("=== Exporting withdraw circuit ===");
    let circuit = build_withdraw_circuit();
    let (matrices, n_wires) = synthesize(circuit)?;
    write_r1cs_binary(&matrices, n_wires, &output_dir.join("withdraw.r1cs"))?;
    println!("");

    // --- Transfer ---
    println!("=== Exporting transfer circuit ===");
    let circuit = build_transfer_circuit();
    let (matrices, n_wires) = synthesize(circuit)?;
    write_r1cs_binary(&matrices, n_wires, &output_dir.join("transfer.r1cs"))?;
    println!("");

    println!("=== All circuits exported ===");
    println!("Files in r1cs/:");
    for entry in std::fs::read_dir(output_dir)? {
        let entry = entry?;
        println!("  {}", entry.file_name().to_string_lossy());
    }

    Ok(())
}
