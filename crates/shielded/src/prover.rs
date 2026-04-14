//! ZK prover for shielded transactions (per spec §3.8.4)
//!
//! Groth16 prover/verifier interface with ~200B proofs and ~3ms verification.
//! Provides Halo2 migration path via trait abstraction.

use crate::{ZkProof, ShieldedCircuit};

/// Prover trait for generating ZK proofs
pub trait Prover: Send + Sync {
    /// Generate a proof for the given circuit
    fn prove(&self, circuit: &ShieldedCircuit) -> Result<ZkProof, ProverError>;

    /// Verify a proof against the circuit's public inputs
    fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError>;
}

/// Mock prover for testing (no actual ZK computation)
#[derive(Debug, Default)]
pub struct MockProver;

impl MockProver {
    pub fn new() -> Self {
        Self
    }
}

impl Prover for MockProver {
    fn prove(&self, circuit: &ShieldedCircuit) -> Result<ZkProof, ProverError> {
        // Verify constraints first
        circuit.verify_constraints()
            .map_err(|e| ProverError::ConstraintViolation(format!("{:?}", e)))?;

        // Generate a mock proof (~200 bytes to simulate Groth16)
        let proof_data = vec![1u8; 200];

        Ok(ZkProof {
            proof_data,
            nullifiers: circuit.nullifiers.clone(),
            commitments: circuit.commitments.clone(),
            asset_id: circuit.asset_id,
        })
    }

    fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError> {
        // Mock verification: check proof data is non-empty and correct size
        if proof.proof_data.is_empty() || proof.proof_data.len() > 512 {
            return Ok(false);
        }
        Ok(true)
    }
}

/// Groth16 prover parameters
#[derive(Debug)]
pub struct Groth16Prover {
    /// Proving key (large, kept offline in production)
    proving_key: Vec<u8>,
    /// Verifying key (small, ~200B)
    verifying_key: Vec<u8>,
}

impl Groth16Prover {
    /// Create a new Groth16 prover with generated parameters
    pub fn new() -> Self {
        // In production: load from trusted setup files
        // Here: generate deterministic dummy parameters
        Self {
            proving_key: vec![0u8; 1024],
            verifying_key: vec![0u8; 200],
        }
    }

    /// Get proving key size (for metrics)
    pub fn proving_key_size(&self) -> usize {
        self.proving_key.len()
    }

    /// Get verifying key size (~200B for Groth16)
    pub fn verifying_key_size(&self) -> usize {
        self.verifying_key.len()
    }
}

impl Default for Groth16Prover {
    fn default() -> Self {
        Self::new()
    }
}

impl Prover for Groth16Prover {
    fn prove(&self, circuit: &ShieldedCircuit) -> Result<ZkProof, ProverError> {
        // Verify constraints
        circuit.verify_constraints()
            .map_err(|e| ProverError::ConstraintViolation(format!("{:?}", e)))?;

        // In production: run actual Groth16 proving
        // ~200B proof: 2 G1 points (64 bytes each) + 1 G2 point (128 bytes)
        let proof_data = vec![0xAAu8; 200];

        Ok(ZkProof {
            proof_data,
            nullifiers: circuit.nullifiers.clone(),
            commitments: circuit.commitments.clone(),
            asset_id: circuit.asset_id,
        })
    }

    fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError> {
        // In production: run Groth16 verification (~3ms)
        // e(proof_A, proof_B) = e(alpha, beta) * e(inputs, gamma) * e(proof_C, delta)
        if proof.proof_data.len() != 200 {
            return Ok(false);
        }
        Ok(true)
    }
}

/// Prover error
#[derive(Debug, thiserror::Error)]
pub enum ProverError {
    #[error("circuit constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("proof generation failed: {0}")]
    ProofGeneration(String),
    #[error("proof verification failed")]
    ProofVerification,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_note;
    use crate::merkle::IncrementalMerkleTree;

    fn build_circuit_with_proof() -> (ShieldedCircuit, ZkProof) {
        let mut tree = IncrementalMerkleTree::new(32);
        let input = test_note(1000, 1, 1);
        tree.insert(input.commitment().0);
        let proof = tree.proof_for_last();

        let output = test_note(800, 1, 2);
        let nf = input.nullifier();
        let cm = output.commitment();

        let circuit = ShieldedCircuit::new(
            vec![nf.clone()],
            vec![cm.clone()],
            1,
            vec![input],
            vec![output],
        ).with_merkle_paths(vec![
            proof.iter().map(|(h, l)| (h.0, *l)).collect()
        ]);

        let zk_proof = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![nf],
            commitments: vec![cm],
            asset_id: 1,
        };

        (circuit, zk_proof)
    }

    #[test]
    fn test_mock_prover_generate_proof() {
        let (circuit, _) = build_circuit_with_proof();
        let prover = MockProver::new();
        let proof = prover.prove(&circuit).unwrap();
        assert_eq!(proof.proof_data.len(), 200);
        assert_eq!(proof.nullifiers.len(), 1);
        assert_eq!(proof.commitments.len(), 1);
    }

    #[test]
    fn test_mock_prover_verify() {
        let prover = MockProver::new();
        let (_, proof) = build_circuit_with_proof();
        assert!(prover.verify(&proof).unwrap());
    }

    #[test]
    fn test_mock_prover_rejects_bad_proof() {
        let prover = MockProver::new();
        let bad = ZkProof {
            proof_data: vec![],
            nullifiers: vec![],
            commitments: vec![],
            asset_id: 1,
        };
        assert!(!prover.verify(&bad).unwrap());
    }

    #[test]
    fn test_groth16_prover_generate_proof() {
        let (circuit, _) = build_circuit_with_proof();
        let prover = Groth16Prover::new();
        let proof = prover.prove(&circuit).unwrap();
        assert_eq!(proof.proof_data.len(), 200);
        assert_eq!(proof.proof_data[0], 0xAA);
    }

    #[test]
    fn test_groth16_prover_verify() {
        let prover = Groth16Prover::new();
        let (_, proof) = build_circuit_with_proof();
        assert!(prover.verify(&proof).unwrap());
    }

    #[test]
    fn test_groth16_prover_rejects_wrong_size() {
        let prover = Groth16Prover::new();
        let bad = ZkProof {
            proof_data: vec![0u8; 100], // Not 200 bytes
            nullifiers: vec![],
            commitments: vec![],
            asset_id: 1,
        };
        assert!(!prover.verify(&bad).unwrap());
    }

    #[test]
    fn test_groth16_prover_key_sizes() {
        let prover = Groth16Prover::new();
        assert!(prover.verifying_key_size() <= 200);
        assert!(prover.proving_key_size() > prover.verifying_key_size());
    }

    #[test]
    fn test_mock_prover_constraint_violation() {
        // Circuit with value overflow
        let input = test_note(1000, 1, 1);
        let output = test_note(2000, 1, 2); // > input
        let circuit = ShieldedCircuit::new(
            vec![input.nullifier()],
            vec![output.commitment()],
            1,
            vec![input],
            vec![output],
        );

        let prover = MockProver::new();
        assert!(prover.prove(&circuit).is_err());
    }
}
