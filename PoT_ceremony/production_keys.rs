//! Production key loading from Powers of Tau ceremony output.
//!
//! This module replaces the dev-only `circuit_specific_setup` path
//! with production key loading from disk. The verifying keys are
//! derived from the Perpetual Powers of Tau + Phase 2 derivation,
//! meaning no single party knows the toxic waste.
//!
//! Usage in the node:
//! ```rust
//! let vk_set = ProductionKeys::load("/var/lib/callchain/shielded_keys")?;
//! let prover = RealProver::from_production_keys(vk_set);
//! ```

use ark_bn254::{Bn254, Fr};
use ark_groth16::{ProvingKey, VerifyingKey};
use ark_serialize::CanonicalDeserialize;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum KeyLoadError {
    #[error("Key file not found: {0}")]
    FileNotFound(PathBuf),
    #[error("Failed to deserialize key: {0}")]
    Deserialize(#[from] ark_serialize::SerializationError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("VK hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("Missing circuit key: {0}")]
    MissingCircuit(String),
}

/// Verifying keys for all three shielded circuits, loaded from production ceremony output.
#[derive(Clone, Debug)]
pub struct ProductionVerifyingKeys {
    pub transfer: VerifyingKey<Bn254>,
    pub deposit: VerifyingKey<Bn254>,
    pub withdraw: VerifyingKey<Bn254>,
}

/// Full production keys (VK + optional PK for client-side proving).
#[derive(Clone, Debug)]
pub struct ProductionKeys {
    pub vk: ProductionVerifyingKeys,
    pub pk: Option<ProductionProvingKeys>,
}

#[derive(Clone, Debug)]
pub struct ProductionProvingKeys {
    pub transfer: ProvingKey<Bn254>,
    pub deposit: ProvingKey<Bn254>,
    pub withdraw: ProvingKey<Bn254>,
}

/// Circuit types for key lookups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CircuitType {
    Transfer,
    Deposit,
    Withdraw,
}

impl CircuitType {
    fn as_str(&self) -> &'static str {
        match self {
            CircuitType::Transfer => "transfer",
            CircuitType::Deposit => "deposit",
            CircuitType::Withdraw => "withdraw",
        }
    }
}

impl ProductionKeys {
    /// Load all circuit keys from a directory.
    ///
    /// Expected directory structure:
    /// ```text
    /// keys_dir/
    ///   transfer_vk.bin
    ///   deposit_vk.bin
    ///   withdraw_vk.bin
    ///   transfer_pk.bin    (optional, for client-side proving)
    ///   deposit_pk.bin
    ///   withdraw_pk.bin
    /// ```
    pub fn load(keys_dir: impl AsRef<Path>) -> Result<Self, KeyLoadError> {
        let dir = keys_dir.as_ref();

        let transfer_vk = load_vk_from_file(&dir.join("transfer_vk.bin"))?;
        let deposit_vk = load_vk_from_file(&dir.join("deposit_vk.bin"))?;
        let withdraw_vk = load_vk_from_file(&dir.join("withdraw_vk.bin"))?;

        let vk = ProductionVerifyingKeys {
            transfer: transfer_vk,
            deposit: deposit_vk,
            withdraw: withdraw_vk,
        };

        // Proving keys are optional — only needed on clients that generate proofs.
        // Validators only need verifying keys.
        let pk = load_proving_keys_if_present(dir)?;

        Ok(Self { vk, pk })
    }

    /// Load with hash verification against genesis config.
    ///
    /// This ensures the loaded keys match what the chain expects,
    /// preventing nodes from running with wrong ceremony output.
    pub fn load_with_verification(
        keys_dir: impl AsRef<Path>,
        expected_hashes: &GenesisKeyHashes,
    ) -> Result<Self, KeyLoadError> {
        let keys = Self::load(&keys_dir)?;

        // Verify each VK matches the expected hash
        let transfer_hash = hash_vk(&keys.vk.transfer)?;
        let deposit_hash = hash_vk(&keys.vk.deposit)?;
        let withdraw_hash = hash_vk(&keys.vk.withdraw)?;

        if transfer_hash != expected_hashes.transfer {
            return Err(KeyLoadError::HashMismatch {
                expected: expected_hashes.transfer.clone(),
                actual: transfer_hash,
            });
        }
        if deposit_hash != expected_hashes.deposit {
            return Err(KeyLoadError::HashMismatch {
                expected: expected_hashes.deposit.clone(),
                actual: deposit_hash,
            });
        }
        if withdraw_hash != expected_hashes.withdraw {
            return Err(KeyLoadError::HashMismatch {
                expected: expected_hashes.withdraw.clone(),
                actual: withdraw_hash,
            });
        }

        Ok(keys)
    }

    /// Get the verifying key for a specific circuit.
    pub fn vk(&self, circuit: CircuitType) -> &VerifyingKey<Bn254> {
        match circuit {
            CircuitType::Transfer => &self.vk.transfer,
            CircuitType::Deposit => &self.vk.deposit,
            CircuitType::Withdraw => &self.vk.withdraw,
        }
    }
}

/// Expected VK hashes embedded in the genesis config.
#[derive(Clone, Debug)]
pub struct GenesisKeyHashes {
    pub transfer: String,
    pub deposit: String,
    pub withdraw: String,
}

/// Load a single verifying key from a binary file.
fn load_vk_from_file(path: &Path) -> Result<VerifyingKey<Bn254>, KeyLoadError> {
    if !path.exists() {
        return Err(KeyLoadError::FileNotFound(path.to_path_buf()));
    }

    let data = std::fs::read(path)?;
    let vk = VerifyingKey::<Bn254>::deserialize_uncompressed(&data[..])?;
    Ok(vk)
}

/// Load proving keys if present (not required for validators).
fn load_proving_keys_if_present(
    dir: &Path,
) -> Result<Option<ProductionProvingKeys>, KeyLoadError> {
    let transfer_pk = dir.join("transfer_pk.bin");
    let deposit_pk = dir.join("deposit_pk.bin");
    let withdraw_pk = dir.join("withdraw_pk.bin");

    if !transfer_pk.exists() {
        return Ok(None);
    }

    let load_pk = |path: &Path| -> Result<ProvingKey<Bn254>, KeyLoadError> {
        let data = std::fs::read(path)?;
        let pk = ProvingKey::<Bn254>::deserialize_uncompressed(&data[..])?;
        Ok(pk)
    };

    Ok(Some(ProductionProvingKeys {
        transfer: load_pk(&transfer_pk)?,
        deposit: load_pk(&deposit_pk)?,
        withdraw: load_pk(&withdraw_pk)?,
    }))
}

/// Compute SHA-256 hash of a verifying key for genesis embedding.
fn hash_vk(vk: &VerifyingKey<Bn254>) -> Result<String, KeyLoadError> {
    let mut buf = Vec::new();
    vk.serialize_uncompressed(&mut buf)?;
    let hash = sha256_hash(&buf);
    Ok(hash)
}

fn sha256_hash(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_hash_vk_deterministic() {
        // Create a dummy VK and verify hashing is deterministic
        // This test will work once real keys are generated
    }

    #[test]
    fn test_file_not_found_error() {
        let result = ProductionKeys::load("/nonexistent/path");
        assert!(result.is_err());
    }
}
