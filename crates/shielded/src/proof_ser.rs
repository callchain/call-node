//! Proof serialization for Halo2 IPA proofs.
//!
//! Halo2 proofs are raw byte vectors produced by `Blake2bWrite::finalize()`.
//! No special deserialization is required — the verifier reads the proof
//! bytes directly via `Blake2bRead`. This module provides size constants
//! and error types for protocol-level validation.

/// Error deserializing or validating a proof.
#[derive(Debug, thiserror::Error)]
pub enum ProofDeserializeError {
    #[error("proof data too short: expected {expected} bytes, got {got}")]
    TooShort { expected: usize, got: usize },
    #[error("proof data exceeds maximum size: {0}")]
    TooLarge(usize),
}

/// Approximate minimum Halo2 IPA proof size (deposit circuit, k=10).
/// Actual size varies by circuit and transcript; this is a lower bound
/// for sanity checks (e.g. rejecting empty or truncated proofs).
pub const HALO2_PROOF_MIN_SIZE: usize = 2_000;

/// Approximate maximum Halo2 IPA proof size (transfer circuit, k=12).
/// Used as an upper bound for mempool / protocol validation.
pub const HALO2_PROOF_MAX_SIZE: usize = 20_000;
