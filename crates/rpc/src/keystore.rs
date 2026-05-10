//! Local keystore for RPC signing operations.
//!
//! Supports both in-memory (ephemeral) and disk-persistent modes.
//! When a `keystore_dir` is set, imported keys are encrypted with
//! Web3 Secret Storage format (scrypt + AES-128-CTR + keccak256 MAC)
//! and written to `{dir}/{address_hex}.json`.
//!
//! # Security notes
//! - Keys in memory are raw bytes (not zeroized on drop in current impl).
//! - Persistent files use scrypt KDF with user-supplied passphrase.
//! - Production nodes should use HSM or external signer instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use alloy_primitives::{Address, B256};
use call_crypto::{
    encrypt_key, keccak256, load_key, pubkey_to_address, secp256k1_sign, KeystoreError,
};
use call_primitives::PublicKey;

/// Local keystore with optional disk persistence.
pub struct LocalKeystore {
    keys: RwLock<HashMap<Address, [u8; 32]>>,
    /// Directory where encrypted keystore files are stored.
    keystore_dir: Option<PathBuf>,
}

impl LocalKeystore {
    /// Create a new in-memory-only keystore (ephemeral).
    pub fn new() -> Self {
        Self {
            keys: RwLock::new(HashMap::new()),
            keystore_dir: None,
        }
    }

    /// Create a keystore backed by a directory on disk.
    /// Imported keys are encrypted and persisted automatically.
    pub fn new_with_dir<P: AsRef<Path>>(dir: P) -> Self {
        let path = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&path).ok();
        Self {
            keys: RwLock::new(HashMap::new()),
            keystore_dir: Some(path),
        }
    }

    /// Import a raw 32-byte private key.
    ///
    /// If `passphrase` is provided and `keystore_dir` is set, the key is
    /// encrypted and written to disk. Otherwise it is stored in memory only.
    pub fn import_raw_key(
        &self,
        raw_key: &[u8; 32],
        passphrase: Option<&str>,
    ) -> Result<Address, KeystoreError> {
        let address = derive_address(raw_key);

        // Persist to disk if configured
        if let (Some(dir), Some(pw)) = (&self.keystore_dir, passphrase) {
            let path = keystore_path(dir, &address);
            let encrypted = encrypt_key(raw_key, pw)?;
            let json = serde_json::to_string_pretty(&encrypted)
                .map_err(|e| KeystoreError::Io(format!("serialize: {e}")))?;
            std::fs::write(&path, json)
                .map_err(|e| KeystoreError::Io(format!("write keystore: {e}")))?;
        }

        if let Ok(mut keys) = self.keys.write() {
            keys.insert(address, *raw_key);
        }
        Ok(address)
    }

    /// Load an encrypted keystore file into memory.
    pub fn load_from_file(&self, path: &Path, passphrase: &str) -> Result<Address, KeystoreError> {
        let raw_key = load_key(path, passphrase)?;
        let address = derive_address(&*raw_key);
        if let Ok(mut keys) = self.keys.write() {
            keys.insert(address, *raw_key);
        }
        Ok(address)
    }

    /// Load all keystore files from the configured directory.
    /// Returns (success_count, failures).
    pub fn load_all_from_dir(&self, passphrase: &str) -> (usize, Vec<String>) {
        let mut ok = 0;
        let mut failures = Vec::new();
        let Some(dir) = &self.keystore_dir else {
            return (0, vec!["no keystore_dir configured".into()]);
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => return (0, vec![format!("read_dir failed: {e}")]),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            match self.load_from_file(&path, passphrase) {
                Ok(_) => ok += 1,
                Err(e) => failures.push(format!("{}: {e}", path.display())),
            }
        }
        (ok, failures)
    }

    /// Remove an account from the keystore (memory + disk if persisted).
    pub fn remove_account(&self, address: &Address) -> bool {
        let removed = if let Ok(mut keys) = self.keys.write() {
            keys.remove(address).is_some()
        } else {
            false
        };
        if removed {
            if let Some(dir) = &self.keystore_dir {
                let path = keystore_path(dir, address);
                let _ = std::fs::remove_file(path);
            }
        }
        removed
    }

    /// List all addresses currently loaded in memory.
    pub fn list_accounts(&self) -> Vec<Address> {
        if let Ok(keys) = self.keys.read() {
            keys.keys().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Check if an address is loaded in memory.
    pub fn has_account(&self, address: &Address) -> bool {
        if let Ok(keys) = self.keys.read() {
            keys.contains_key(address)
        } else {
            false
        }
    }

    /// Get the raw private key for an address.
    /// Returns None if the account is not in the keystore.
    pub fn get_key(&self, address: &Address) -> Option<[u8; 32]> {
        if let Ok(keys) = self.keys.read() {
            keys.get(address).copied()
        } else {
            None
        }
    }

    /// Sign a 32-byte hash with the account's private key.
    /// Returns a 65-byte signature (r || s || v).
    pub fn sign_hash(&self, address: &Address, hash: &B256) -> Option<[u8; 65]> {
        let key = {
            let keys = self.keys.read().ok()?;
            *keys.get(address)?
        };
        Some(secp256k1_sign(&key, hash.as_ref()))
    }

    /// Sign a message using Ethereum personal_sign format.
    ///
    /// Message format: `\x19Ethereum Signed Message:\n{len}\n{message}`
    /// Returns 65-byte signature (r || s || v).
    pub fn sign_message(&self, address: &Address, message: &[u8]) -> Option<[u8; 65]> {
        let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
        let mut full = prefix.into_bytes();
        full.extend_from_slice(message);
        let hash = keccak256(&full);
        self.sign_hash(address, &hash)
    }
}

impl Default for LocalKeystore {
    fn default() -> Self {
        Self::new()
    }
}

/// Derive Ethereum address from a raw secp256k1 private key.
fn derive_address(raw_key: &[u8; 32]) -> Address {
    use k256::ecdsa::SigningKey;
    let signing_key = SigningKey::from_slice(raw_key).expect("valid secp256k1 key");
    let verifying_key = signing_key.verifying_key();
    let encoded = verifying_key.to_encoded_point(false);
    let mut pubkey = [0u8; 64];
    pubkey.copy_from_slice(&encoded.as_bytes()[1..]);
    let pk = PublicKey::from(pubkey);
    let addr = pubkey_to_address(&pk);
    Address::from_slice(addr.as_slice())
}

/// Build keystore file path: `{dir}/{address_lower}.json`
fn keystore_path(dir: &Path, address: &Address) -> PathBuf {
    let name = format!("{}.json", address.to_string().to_lowercase());
    dir.join(name)
}
