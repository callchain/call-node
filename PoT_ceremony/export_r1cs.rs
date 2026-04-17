//! Export arkworks R1CS circuits to snarkjs-compatible .r1cs files.
//!
//! snarkjs Phase 2 derivation requires .r1cs files, but arkworks generates
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
//! These files are then used by phase2_derive.sh for key generation.

use ark_bn254::{Bn254, Fr};
use ark_relations::r1cs::{ConstraintMatrices, ConstraintSystemRef, SynthesisMode};
use ark_std::io::Write;
use std::fs::File;
use std::path::Path;

/// Placeholder circuit constructors — replace with actual circuit instantiation
/// from crates/shielded/src/circuit_transfer.rs, etc.
fn build_transfer_circuit() -> impl ark_relations::r1cs::ConstraintSynthesizer<Fr> {
    // TODO: Import and instantiate the real ShieldedTransferCircuit
    // from call_shielded::circuit_transfer::ShieldedTransferCircuit
    unimplemented!("Wire up real transfer circuit from call-shielded")
}

fn build_deposit_circuit() -> impl ark_relations::r1cs::ConstraintSynthesizer<Fr> {
    // TODO: Import and instantiate the real ShieldedDepositCircuit
    // from call_shielded::circuit_deposit::ShieldedDepositCircuit
    unimplemented!("Wire up real deposit circuit from call-shielded")
}

fn build_withdraw_circuit() -> impl ark_relations::r1cs::ConstraintSynthesizer<Fr> {
    // TODO: Import and instantiate the real ShieldedWithdrawCircuit
    // from call_shielded::circuit_withdraw::ShieldedWithdrawCircuit
    unimplemented!("Wire up real withdraw circuit from call-shielded")
}

/// Write R1CS matrices to snarkjs binary format.
///
/// snarkjs .r1cs format (v2):
///   - Header: magic + version + n_wires, n_pub_out, n_pub_inputs, n_prv_inputs, n_labels, m_constraints
///   - Wire ID map (label)
///   - Constraint matrices: A, B, C as sparse matrices
fn write_r1cs_binary(
    matrices: &ConstraintMatrices<Fr>,
    cs: &ark_relations::r1cs::ConstraintSystem<Fr>,
    output_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let n_wires = cs.num_instance_variables + cs.num_witness_variables;
    let n_pub_out = matrices.num_instance_variables - 1; // exclude constant term
    let n_pub_inputs = matrices.num_instance_variables - 1;
    let n_prv_inputs = cs.num_witness_variables - matrices.num_instance_variables;
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

    // Section 3: Constraints (sparse matrix)
    let section_id = 3u32;
    // Each constraint: 3 matrices (A, B, C), each is a sparse vector
    // For simplicity, write constraint count * 3 entries
    // TODO: Extract actual constraint matrices from the CS
    // This requires accessing cs.constraints which is private in ark-relations
    // Alternative: use ark-circuit-exporter or manual extraction
    let section_size: u64 = 0; // placeholder
    file.write_all(&section_id.to_le_bytes())?;
    file.write_all(&section_size.to_le_bytes())?;

    println!("  Written to: {}", output_path.display());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output_dir = Path::new("r1cs");
    std::fs::create_dir_all(output_dir)?;

    let circuits: Vec<(&str, Box<dyn Fn() -> Box<dyn ark_relations::r1cs::ConstraintSynthesizer<Fr>>>)> = vec![
        ("deposit", Box::new(|| Box::new(build_deposit_circuit()))),
        ("transfer", Box::new(|| Box::new(build_transfer_circuit()))),
        ("withdraw", Box::new(|| Box::new(build_withdraw_circuit()))),
    ];

    for (name, builder) in circuits {
        println!("=== Exporting {} circuit ===", name);
        let circuit = builder();

        // Use ark_relations to synthesize and extract matrices
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        cs.set_mode(SynthesisMode::Setup);

        circuit.generate_constraints(cs.clone())?;
        cs.finalize();

        let matrices = cs.to_matrices().expect("CS should have matrices");

        let output_path = output_dir.join(format!("{}.r1cs", name));
        write_r1cs_binary(&matrices, &cs, &output_path)?;
        println!("");
    }

    println!("=== All circuits exported ===");
    println!("Files in r1cs/:");
    for entry in std::fs::read_dir(output_dir)? {
        let entry = entry?;
        println!("  {}", entry.file_name().to_string_lossy());
    }

    Ok(())
}
