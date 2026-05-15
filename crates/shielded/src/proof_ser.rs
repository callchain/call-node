//! Proof serialization placeholder during Halo2 migration.
//!
//! Groth16 proof serialization has been removed. Halo2 proofs are serialized
//! via `halo2_proofs::plonk::Prover::create_proof`. This module will be
//! rewritten in Phase 5.

/// Error deserializing a proof.
#[derive(Debug, thiserror::Error)]
pub enum ProofDeserializeError {
    #[error("proof data too short: expected {expected} bytes, got {got}")]
    TooShort { expected: usize, got: usize },
    #[error("invalid compressed point: {0}")]
    InvalidPoint(&'static str),
}

/// Old Groth16 proof size (128 bytes). Retained for compatibility checks.
pub const GROTH16_PROOF_SIZE: usize = 128;
