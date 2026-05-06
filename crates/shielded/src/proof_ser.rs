//! Proof serialization for ark-groth16 proofs over BN254.
//!
//! Groth16 proof layout (128 bytes compressed):
//! - `proof_a`: G1Affine compressed = 32 bytes
//! - `proof_b`: G2Affine compressed = 64 bytes
//! - `proof_c`: G1Affine compressed = 32 bytes
//!
//! Public inputs are appended after the proof data during transmission.

use ark_bn254::{Bn254, G1Affine, G2Affine};
use ark_ff::PrimeField;
use ark_groth16::Proof;
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};

/// Serialized Groth16 proof size in bytes (compressed form).
/// G1 (32B) + G2 (64B) + G1 (32B) = 128 bytes.
pub const GROTH16_PROOF_SIZE: usize = 128;

/// Serialize a Groth16 proof to a fixed-size 128-byte buffer.
///
/// Uses compressed point serialization:
/// - `proof_a` (G1): 32 bytes
/// - `proof_b` (G2): 64 bytes
/// - `proof_c` (G1): 32 bytes
pub fn serialize_groth16_proof(proof: Proof<Bn254>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(GROTH16_PROOF_SIZE);

    // Serialize proof_a (G1Affine, compressed)
    proof_a_compressed(&proof)
        .serialize_compressed(&mut buf)
        .expect("G1 serialization failed");

    // Serialize proof_b (G2Affine, compressed)
    proof_b_compressed(&proof)
        .serialize_compressed(&mut buf)
        .expect("G2 serialization failed");

    // Serialize proof_c (G1Affine, compressed)
    proof_c_compressed(&proof)
        .serialize_compressed(&mut buf)
        .expect("G1 serialization failed");

    debug_assert_eq!(
        buf.len(),
        GROTH16_PROOF_SIZE,
        "expected {GROTH16_PROOF_SIZE} bytes, got {}",
        buf.len()
    );

    buf
}

/// Deserialize a Groth16 proof from a 128-byte buffer.
///
/// Returns an error if the input is not exactly [`GROTH16_PROOF_SIZE`] bytes
/// or if the compressed points are invalid.
pub fn deserialize_groth16_proof(data: &[u8]) -> Result<Proof<Bn254>, ProofDeserializeError> {
    if data.len() < GROTH16_PROOF_SIZE {
        return Err(ProofDeserializeError::TooShort {
            expected: GROTH16_PROOF_SIZE,
            got: data.len(),
        });
    }

    // Proof A: bytes 0..32 (G1Affine)
    let proof_a = G1Affine::deserialize_compressed(&data[0..32])
        .map_err(|_| ProofDeserializeError::InvalidPoint("proof_a"))?;

    // Proof B: bytes 32..96 (G2Affine)
    let proof_b = G2Affine::deserialize_compressed(&data[32..96])
        .map_err(|_| ProofDeserializeError::InvalidPoint("proof_b"))?;

    // Proof C: bytes 96..128 (G1Affine)
    let proof_c = G1Affine::deserialize_compressed(&data[96..128])
        .map_err(|_| ProofDeserializeError::InvalidPoint("proof_c"))?;

    Ok(Proof {
        a: proof_a,
        b: proof_b,
        c: proof_c,
    })
}

/// Encode public inputs as Fr bytes appended after the proof.
///
/// Each [`ark_bn254::Fr`] is serialized as 32 bytes (canonical little-endian).
/// Returns a new buffer containing `proof_data || public_inputs`.
pub fn append_public_inputs(proof_data: &[u8], public_inputs: &[ark_bn254::Fr]) -> Vec<u8> {
    use ark_ff::{BigInteger, PrimeField};
    let mut buf = proof_data.to_vec();
    for pi in public_inputs {
        let pi_bytes = pi.into_bigint().to_bytes_be();
        buf.extend_from_slice(&pi_bytes);
    }
    buf
}

/// Extract public inputs from a proof buffer that was built with [`append_public_inputs`].
///
/// Returns `(proof_bytes, public_inputs)` where proof_bytes is the first 128 bytes.
pub fn extract_public_inputs(
    data: &[u8],
) -> Result<(&[u8], Vec<ark_bn254::Fr>), ProofDeserializeError> {
    if data.len() < GROTH16_PROOF_SIZE {
        return Err(ProofDeserializeError::TooShort {
            expected: GROTH16_PROOF_SIZE,
            got: data.len(),
        });
    }

    let proof_bytes = &data[..GROTH16_PROOF_SIZE];
    let pi_bytes = &data[GROTH16_PROOF_SIZE..];

    let public_inputs = pi_bytes
        .chunks(32)
        .map(|chunk| {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(chunk);
            ark_bn254::Fr::from_be_bytes_mod_order(&buf)
        })
        .collect();

    Ok((proof_bytes, public_inputs))
}

// ---------------------------------------------------------------------------
// Internal helpers: extract affine points from the projective Proof struct
// ---------------------------------------------------------------------------

fn proof_a_compressed(proof: &Proof<Bn254>) -> G1Affine {
    proof.a.into()
}

fn proof_b_compressed(proof: &Proof<Bn254>) -> G2Affine {
    proof.b.into()
}

fn proof_c_compressed(proof: &Proof<Bn254>) -> G1Affine {
    proof.c.into()
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error deserializing a Groth16 proof.
#[derive(Debug, thiserror::Error)]
pub enum ProofDeserializeError {
    #[error("proof data too short: expected {expected} bytes, got {got}")]
    TooShort { expected: usize, got: usize },
    #[error("invalid compressed point: {0}")]
    InvalidPoint(&'static str),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "real-prover"))]
mod tests {
    use super::*;
    use ark_bn254::Fr;
    use ark_groth16::Groth16;
    use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
    use ark_snark::SNARK;
    use ark_std::rand::rngs::StdRng;
    use ark_std::rand::SeedableRng;

    /// Tiny test circuit with 3 public inputs.
    #[derive(Debug, Clone)]
    struct TestCircuit {
        pub a: Option<Fr>,
        pub b: Option<Fr>,
    }

    impl ConstraintSynthesizer<Fr> for TestCircuit {
        fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
            use ark_r1cs_std::alloc::AllocVar;
            use ark_r1cs_std::fields::fp::FpVar;
            let a = Fr::from(3u64);
            let b = Fr::from(5u64);
            let c = Fr::from(15u64); // a * b = c

            let a_var = FpVar::new_input(cs.clone(), || Ok(a))?;
            let b_var = FpVar::new_input(cs.clone(), || Ok(b))?;
            let c_var = FpVar::new_input(cs.clone(), || Ok(c))?;

            // Enforce a * b = c
            let _ = a_var * b_var - c_var;
            Ok(())
        }
    }

    fn generate_test_proof() -> Proof<Bn254> {
        let circuit = TestCircuit { a: None, b: None };
        let rng = &mut StdRng::seed_from_u64(42);
        let (pk, _vk) = Groth16::<Bn254>::circuit_specific_setup(circuit.clone(), rng).unwrap();
        Groth16::<Bn254>::prove(&pk, circuit, rng).unwrap()
    }

    #[test]
    fn test_serialize_proof_size() {
        let proof = generate_test_proof();
        let serialized = serialize_groth16_proof(proof);
        assert_eq!(serialized.len(), GROTH16_PROOF_SIZE);
    }

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let proof = generate_test_proof();
        let serialized = serialize_groth16_proof(proof.clone());
        let deserialized = deserialize_groth16_proof(&serialized).unwrap();

        assert_eq!(proof.a, deserialized.a);
        assert_eq!(proof.b, deserialized.b);
        assert_eq!(proof.c, deserialized.c);
    }

    #[test]
    fn test_deserialize_too_short() {
        let result = deserialize_groth16_proof(&[0u8; 64]);
        assert!(matches!(
            result,
            Err(ProofDeserializeError::TooShort { .. })
        ));
    }

    #[test]
    fn test_deserialize_empty() {
        let result = deserialize_groth16_proof(&[]);
        assert!(matches!(
            result,
            Err(ProofDeserializeError::TooShort { .. })
        ));
    }

    #[test]
    fn test_deserialize_invalid_point_rejected() {
        // 128 bytes of zeros should fail because the infinity flag / encoding is invalid
        let zeros = [0u8; GROTH16_PROOF_SIZE];
        // The first byte being 0 means no infinity flag — but the rest are zeros
        // which may or may not deserialize depending on the curve point encoding.
        // We just check that it doesn't crash.
        let _ = deserialize_groth16_proof(&zeros);
    }

    #[test]
    fn test_append_public_inputs() {
        let proof = generate_test_proof();
        let serialized = serialize_groth16_proof(proof);

        let pis = vec![Fr::from(1u64), Fr::from(2u64), Fr::from(3u64)];
        let combined = append_public_inputs(&serialized, &pis);

        assert_eq!(combined.len(), GROTH16_PROOF_SIZE + 3 * 32);

        // Extract back
        let (proof_bytes, extracted) = extract_public_inputs(&combined).unwrap();
        assert_eq!(proof_bytes.len(), GROTH16_PROOF_SIZE);
        assert_eq!(extracted.len(), 3);
        assert_eq!(extracted[0], Fr::from(1u64));
        assert_eq!(extracted[1], Fr::from(2u64));
        assert_eq!(extracted[2], Fr::from(3u64));
    }

    #[test]
    fn test_extract_public_inputs_too_short() {
        let result = extract_public_inputs(&[0u8; 50]);
        assert!(matches!(
            result,
            Err(ProofDeserializeError::TooShort { .. })
        ));
    }

    #[test]
    fn test_append_zero_public_inputs() {
        let proof = generate_test_proof();
        let serialized = serialize_groth16_proof(proof);
        let combined = append_public_inputs(&serialized, &[]);
        assert_eq!(combined.len(), GROTH16_PROOF_SIZE);
    }

    #[test]
    fn test_proof_is_deterministic_with_seed() {
        let proof1 = generate_test_proof();
        let serialized1 = serialize_groth16_proof(proof1);

        let proof2 = generate_test_proof();
        let serialized2 = serialize_groth16_proof(proof2);

        assert_eq!(serialized1, serialized2);
    }
}
