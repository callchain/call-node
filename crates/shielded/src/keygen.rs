//! Key generation placeholder during Halo2 migration.
//!
//! Groth16 key generation has been removed. Halo2 uses `Params::new(k)` +
//! `keygen_vk` / `keygen_pk` instead. This module will be deleted in Phase 6.

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

/// Metadata about a generated circuit key pair.
#[derive(Debug, Clone)]
pub struct KeyInfo {
    /// Number of constraints in the circuit.
    pub constraint_count: usize,
    /// Number of public input variables.
    pub public_input_count: usize,
    /// Serialized proving key size in bytes.
    pub pk_size_bytes: usize,
    /// Serialized verifying key size in bytes.
    pub vk_size_bytes: usize,
}
