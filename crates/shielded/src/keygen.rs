//! Key generation and persistence for Groth16 circuits over BN254.
//!
//! Provides circuit-specific trusted setup, disk persistence, and loading
//! of proving/verifying keys. All operations gated behind the `real-prover` feature.

use ark_bn254::Bn254;
use ark_groth16::{ProvingKey, VerifyingKey};
use ark_relations::r1cs::ConstraintSynthesizer;
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_snark::SNARK;

/// Generate circuit-specific proving and verifying keys.
///
/// Uses a seeded RNG for reproducible dev/test keys.
/// For production, pass a CSPRNG or use a ceremony-generated CRS.
pub fn generate_keys_for_circuit<C: ConstraintSynthesizer<ark_bn254::Fr> + Clone>(
    circuit: C,
) -> (ProvingKey<Bn254>, VerifyingKey<Bn254>) {
    use ark_groth16::Groth16;
    use ark_std::rand::rngs::StdRng;
    use ark_std::rand::SeedableRng;

    let rng = &mut StdRng::seed_from_u64(42);
    Groth16::<Bn254>::circuit_specific_setup(circuit, rng)
        .expect("circuit-specific setup failed")
}

/// Save proving and verifying keys to disk.
///
/// Writes two files:
/// - `{path_prefix}.pk` — serialized proving key
/// - `{path_prefix}.vk` — serialized verifying key
///
/// Keys are serialized in compressed canonical form.
pub fn save_keys(
    pk: &ProvingKey<Bn254>,
    vk: &VerifyingKey<Bn254>,
    path_prefix: &str,
) -> Result<(), KeygenError> {
    let pk_path = format!("{}.pk", path_prefix);
    let vk_path = format!("{}.vk", path_prefix);

    // Serialize and save proving key
    let mut pk_bytes = Vec::new();
    pk.serialize_compressed(&mut pk_bytes)
        .map_err(|e| KeygenError::Io(format!("serialize PK: {}", e)))?;
    std::fs::write(&pk_path, &pk_bytes)
        .map_err(|e| KeygenError::Io(format!("write {}: {}", pk_path, e)))?;

    // Serialize and save verifying key
    let mut vk_bytes = Vec::new();
    vk.serialize_compressed(&mut vk_bytes)
        .map_err(|e| KeygenError::Io(format!("serialize VK: {}", e)))?;
    std::fs::write(&vk_path, &vk_bytes)
        .map_err(|e| KeygenError::Io(format!("write {}: {}", vk_path, e)))?;

    Ok(())
}

/// Load proving and verifying keys from disk.
///
/// Reads files:
/// - `{path_prefix}.pk` — serialized proving key
/// - `{path_prefix}.vk` — serialized verifying key
pub fn load_keys(path_prefix: &str) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>), KeygenError> {
    let pk_path = format!("{}.pk", path_prefix);
    let vk_path = format!("{}.vk", path_prefix);

    // Load proving key
    let pk_bytes = std::fs::read(&pk_path)
        .map_err(|e| KeygenError::Io(format!("read {}: {}", pk_path, e)))?;
    let pk = ProvingKey::<Bn254>::deserialize_compressed(&pk_bytes[..])
        .map_err(|e| KeygenError::Deserialize(format!("PK: {}", e)))?;

    // Load verifying key
    let vk_bytes = std::fs::read(&vk_path)
        .map_err(|e| KeygenError::Io(format!("read {}: {}", vk_path, e)))?;
    let vk = VerifyingKey::<Bn254>::deserialize_compressed(&vk_bytes[..])
        .map_err(|e| KeygenError::Deserialize(format!("VK: {}", e)))?;

    Ok((pk, vk))
}

/// Metadata about a generated circuit key pair.
#[derive(Debug, Clone)]
pub struct KeyInfo {
    /// Number of R1CS constraints in the circuit.
    pub constraint_count: usize,
    /// Number of public input variables.
    pub public_input_count: usize,
    /// Serialized proving key size in bytes.
    pub pk_size_bytes: usize,
    /// Serialized verifying key size in bytes.
    pub vk_size_bytes: usize,
}

impl KeyInfo {
    /// Compute key info from a circuit and its generated keys.
    pub fn from_circuit_and_keys<C: ConstraintSynthesizer<ark_bn254::Fr> + Clone>(
        circuit: C,
        pk: &ProvingKey<Bn254>,
        vk: &VerifyingKey<Bn254>,
    ) -> Self {
        use ark_relations::r1cs::ConstraintSystem;

        // Count constraints and public inputs by synthesizing the circuit
        let cs = ConstraintSystem::<ark_bn254::Fr>::new_ref();
        let _ = circuit.generate_constraints(cs.clone());
        let constraint_count = cs.num_constraints();
        let public_input_count = cs.num_instance_variables();

        // Measure serialized key sizes
        let mut pk_buf = Vec::new();
        pk.serialize_compressed(&mut pk_buf).ok();
        let mut vk_buf = Vec::new();
        vk.serialize_compressed(&mut vk_buf).ok();

        Self {
            constraint_count,
            public_input_count,
            pk_size_bytes: pk_buf.len(),
            vk_size_bytes: vk_buf.len(),
        }
    }
}

/// Key generation error.
#[derive(Debug, thiserror::Error)]
pub enum KeygenError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("deserialization failed: {0}")]
    Deserialize(String),
    #[error("setup failed: {0}")]
    Setup(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "real-prover"))]
mod tests {
    use super::*;
    use ark_bn254::Fr;
    use ark_relations::r1cs::{ConstraintSystemRef, SynthesisError};

    /// Tiny test circuit for keygen tests.
    #[derive(Debug, Clone)]
    struct TinyCircuit;

    impl ConstraintSynthesizer<Fr> for TinyCircuit {
        fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
            use ark_r1cs_std::alloc::AllocVar;
            use ark_r1cs_std::fields::fp::FpVar;
            use ark_r1cs_std::prelude::*;
            use ark_ff::Zero;

            let a = Fr::from(3u64);
            let b = Fr::from(5u64);
            let c = Fr::from(15u64); // a * b = c

            let a_var = FpVar::new_input(cs.clone(), || Ok(a))?;
            let b_var = FpVar::new_input(cs.clone(), || Ok(b))?;
            let c_var = FpVar::new_input(cs.clone(), || Ok(c))?;

            // Enforce a * b = c (creates an actual R1CS constraint)
            let _ = (&a_var * &b_var - &c_var).enforce_equal(&FpVar::new_constant(cs.clone(), Fr::zero())?)?;
            Ok(())
        }
    }

    /// A circuit with more constraints and inputs to test size differences.
    #[derive(Debug, Clone)]
    struct BiggerCircuit;

    impl ConstraintSynthesizer<Fr> for BiggerCircuit {
        fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
            use ark_r1cs_std::alloc::AllocVar;
            use ark_r1cs_std::fields::fp::FpVar;
            use ark_r1cs_std::prelude::*;
            use ark_ff::Zero;

            // 5 public inputs
            for i in 1u64..=5u64 {
                let _ = FpVar::new_input(cs.clone(), || Ok(Fr::from(i)))?;
            }

            // Allocate witness variables for constraints
            let x = FpVar::new_witness(cs.clone(), || Ok(Fr::from(3u64)))?;
            let y = FpVar::new_witness(cs.clone(), || Ok(Fr::from(5u64)))?;
            let z = FpVar::new_witness(cs.clone(), || Ok(Fr::from(15u64)))?;

            // Enforce x * y = z (creates R1CS constraint)
            let zero = FpVar::new_constant(cs.clone(), Fr::zero())?;
            (&x * &y - &z).enforce_equal(&zero)?;
            Ok(())
        }
    }

    #[test]
    fn test_generate_keys_succeeds() {
        let circuit = TinyCircuit;
        let (pk, vk) = generate_keys_for_circuit(circuit);
        // PK has non-empty query vectors after successful setup
        assert!(!pk.a_query.is_empty());
        assert!(!vk.gamma_abc_g1.is_empty());
    }

    #[test]
    fn test_keys_are_deterministic() {
        let circuit = TinyCircuit;
        let (pk1, vk1) = generate_keys_for_circuit(circuit.clone());
        let (pk2, vk2) = generate_keys_for_circuit(circuit);

        // Serialize and compare
        let mut buf1 = Vec::new();
        pk1.serialize_compressed(&mut buf1).unwrap();
        let mut buf2 = Vec::new();
        pk2.serialize_compressed(&mut buf2).unwrap();
        assert_eq!(buf1, buf2, "proving keys should be deterministic");

        let mut buf1 = Vec::new();
        vk1.serialize_compressed(&mut buf1).unwrap();
        let mut buf2 = Vec::new();
        vk2.serialize_compressed(&mut buf2).unwrap();
        assert_eq!(buf1, buf2, "verifying keys should be deterministic");
    }

    #[test]
    fn test_save_and_load_keys_roundtrip() {
        let circuit = TinyCircuit;
        let (pk_orig, vk_orig) = generate_keys_for_circuit(circuit);

        let temp_dir = std::env::temp_dir();
        let path_prefix = temp_dir.join("test_keygen_keys").to_string_lossy().to_string();

        // Save keys
        save_keys(&pk_orig, &vk_orig, &path_prefix).unwrap();

        // Load keys back
        let (pk_loaded, vk_loaded) = load_keys(&path_prefix).unwrap();

        // Compare serialized forms
        let mut buf1 = Vec::new();
        pk_orig.serialize_compressed(&mut buf1).unwrap();
        let mut buf2 = Vec::new();
        pk_loaded.serialize_compressed(&mut buf2).unwrap();
        assert_eq!(buf1, buf2, "loaded PK should match original");

        let mut buf1 = Vec::new();
        vk_orig.serialize_compressed(&mut buf1).unwrap();
        let mut buf2 = Vec::new();
        vk_loaded.serialize_compressed(&mut buf2).unwrap();
        assert_eq!(buf1, buf2, "loaded VK should match original");

        // Cleanup
        let _ = std::fs::remove_file(format!("{}.pk", path_prefix));
        let _ = std::fs::remove_file(format!("{}.vk", path_prefix));
    }

    #[test]
    fn test_load_keys_missing_file_errors() {
        let result = load_keys("/nonexistent/path/that/does/not/exist/keys");
        assert!(result.is_err());
        assert!(matches!(result, Err(KeygenError::Io(_))));
    }

    #[test]
    fn test_key_info_from_circuit() {
        let circuit = TinyCircuit;
        let (pk, vk) = generate_keys_for_circuit(circuit.clone());
        let info = KeyInfo::from_circuit_and_keys(circuit, &pk, &vk);

        assert!(info.constraint_count > 0, "should have constraints");
        assert!(info.public_input_count > 0, "should have public inputs");
        assert!(info.pk_size_bytes > 0, "PK should be non-empty");
        assert!(info.vk_size_bytes > 0, "VK should be non-empty");
        assert!(info.pk_size_bytes > info.vk_size_bytes,
            "PK should be larger than VK");
    }

    #[test]
    fn test_key_info_different_circuits() {
        // Generate keys for circuits with different sizes
        let (pk_small, vk_small) = generate_keys_for_circuit(TinyCircuit);
        let small_info = KeyInfo::from_circuit_and_keys(TinyCircuit, &pk_small, &vk_small);

        let (pk_big, vk_big) = generate_keys_for_circuit(BiggerCircuit);
        let big_info = KeyInfo::from_circuit_and_keys(BiggerCircuit, &pk_big, &vk_big);

        // Bigger circuit has more public inputs (5 vs 3)
        assert!(big_info.public_input_count > small_info.public_input_count,
            "bigger circuit should have more public inputs: {} vs {}",
            big_info.public_input_count, small_info.public_input_count);
        assert!(big_info.pk_size_bytes > small_info.pk_size_bytes,
            "bigger circuit PK should be larger: {} vs {}",
            big_info.pk_size_bytes, small_info.pk_size_bytes);
        assert!(big_info.vk_size_bytes > small_info.vk_size_bytes,
            "bigger circuit VK should be larger: {} vs {}",
            big_info.vk_size_bytes, small_info.vk_size_bytes);

        // All should have PK larger than VK
        assert!(small_info.pk_size_bytes > small_info.vk_size_bytes);
        assert!(big_info.pk_size_bytes > big_info.vk_size_bytes);

        // Both should have positive values
        assert!(small_info.constraint_count > 0);
        assert!(big_info.constraint_count > 0);
    }
}
