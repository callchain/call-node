use call_crypto::{build_merkle_root, keccak256, verify_merkle_proof};
use call_primitives::{Address, Hash, TxHash};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Shielded audit info (encrypted, viewing key required to decrypt)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShieldedAuditInfo {
    pub nullifiers: Vec<Hash>,
    pub commitments: Vec<Hash>,
    pub encrypted_amounts: Vec<EncryptedValue>,
    pub compliance_mode: String,
}

/// Encrypted value wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedValue {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
}

/// Append-only audit entry (per spec §24.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub block_height: u64,
    pub tx_index: u32,
    pub tx_type: String,
    pub action: String,
    pub agent_id: Option<String>,
    pub fee_payer: Option<Address>,
    pub before_state: serde_json::Value,
    pub after_state: serde_json::Value,
    pub tx_hash: TxHash,
    pub shielded_details: Option<ShieldedAuditInfo>,
}

impl AuditEntry {
    /// Hash this entry for Merkle tree inclusion
    pub fn hash(&self) -> Hash {
        let data = serde_json::to_vec(self).unwrap_or_default();
        keccak256(&data)
    }
}

/// Append-only audit log with Merkle tree
pub struct AuditLog {
    pub entries: Vec<AuditEntry>,
    entry_hashes: Vec<Hash>,
    merkle_root: Hash,
    log_file: Option<File>,
}

impl AuditLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            entry_hashes: Vec::new(),
            merkle_root: Hash::ZERO,
            log_file: None,
        }
    }

    /// Open audit log file (append mode)
    pub fn open_file(path: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("failed to open audit log: {e}"))?;

        Ok(Self {
            entries: Vec::new(),
            entry_hashes: Vec::new(),
            merkle_root: Hash::ZERO,
            log_file: Some(file),
        })
    }

    /// Append an entry (append-only — no deletions or modifications)
    pub fn append(&mut self, entry: AuditEntry) -> Result<(), String> {
        let entry_hash = entry.hash();

        // Write to file if open
        if let Some(ref mut file) = self.log_file {
            let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
            writeln!(file, "{line}").map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
        }

        self.entries.push(entry);
        self.entry_hashes.push(entry_hash);

        // Recompute Merkle root
        self.merkle_root = build_merkle_root(&self.entry_hashes).unwrap_or(Hash::ZERO);

        Ok(())
    }

    /// Get Merkle root of current audit log
    pub fn merkle_root(&self) -> Hash {
        self.merkle_root
    }

    /// Get entry count
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Generate Merkle proof for entry at index
    pub fn merkle_proof_for(&self, index: usize) -> Option<Vec<(Hash, bool)>> {
        if index >= self.entry_hashes.len() {
            return None;
        }

        // Build full Merkle tree and extract path
        let mut current_level: Vec<Hash> = self.entry_hashes.clone();
        let mut all_paths: Vec<Vec<(Hash, bool)>> = vec![Vec::new(); current_level.len()];

        while current_level.len() > 1 {
            let next_level = Vec::with_capacity(current_level.len().div_ceil(2));
            let mut next_paths: Vec<Vec<(Hash, bool)>> = vec![Vec::new(); current_level.len().div_ceil(2)];

            for (i, chunk) in current_level.chunks(2).enumerate() {
                let (_parent, _sibling_left) = match chunk {
                    [a, b] => {
                        let mut data = Vec::with_capacity(64);
                        data.extend_from_slice(a.as_slice());
                        data.extend_from_slice(b.as_slice());
                        (keccak256(&data), false)
                    }
                    [a] => {
                        let mut data = Vec::with_capacity(64);
                        data.extend_from_slice(a.as_slice());
                        data.extend_from_slice(a.as_slice());
                        (keccak256(&data), false)
                    }
                    _ => unreachable!(),
                };

                // Update paths: left child gets sibling on right, right child gets sibling on left
                let left_idx = i * 2;
                let right_idx = i * 2 + 1;

                if left_idx < all_paths.len() {
                    let sibling = chunk.get(1).copied().unwrap_or(chunk[0]);
                    let mut new_path = all_paths[left_idx].clone();
                    new_path.push((sibling, true)); // sibling on right
                    if left_idx < next_paths.len() {
                        next_paths[left_idx / 2] = new_path;
                    }
                }

                if right_idx < all_paths.len() {
                    let sibling = chunk[0];
                    let mut new_path = all_paths[right_idx].clone();
                    new_path.push((sibling, false)); // sibling on left
                    if right_idx / 2 < next_paths.len() {
                        next_paths[right_idx / 2] = new_path;
                    }
                }
            }

            current_level = next_level;
            all_paths = next_paths;
        }

        all_paths.first().cloned()
    }

    /// Verify an entry against the Merkle root
    pub fn verify_entry(&self, entry: &AuditEntry, proof: &[(Hash, bool)]) -> bool {
        verify_merkle_proof(entry.hash(), proof, self.merkle_root)
    }

    /// Read all entries from file
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("failed to open audit log: {e}"))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut hashes = Vec::new();

        for line in reader.lines() {
            let line = line.map_err(|e| e.to_string())?;
            if line.is_empty() {
                continue;
            }
            let entry: AuditEntry =
                serde_json::from_str(&line).map_err(|e| format!("invalid audit entry: {e}"))?;
            hashes.push(entry.hash());
            entries.push(entry);
        }

        let merkle_root = build_merkle_root(&hashes).unwrap_or(Hash::ZERO);

        Ok(Self {
            entries,
            entry_hashes: hashes,
            merkle_root,
            log_file: None,
        })
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}
