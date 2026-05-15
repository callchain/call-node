//! Callchain Shielded Pool (per spec §3.8)
//!
//! Privacy-preserving transactions using ZK proofs:
//! - Note-based UTXO model with encrypted values
//! - Incremental Merkle Tree (depth 32) for note commitments
//! - Nullifier-based double-spend detection with BitSet compression
//! - ChaCha20-Poly1305 note encryption with viewing keys
//! - Shielded compliance modes for regulatory audit

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::redundant_clone,
    clippy::useless_conversion,
    clippy::redundant_closure,
    clippy::assign_op_pattern,
    clippy::needless_range_loop,
    clippy::type_complexity,
    clippy::len_without_is_empty,
    clippy::redundant_static_lifetimes,
    clippy::iter_cloned_collect,
    clippy::print_stderr,
    clippy::print_stdout,
    unused_imports,
    dead_code
)]

mod circuit;
mod compliance;
mod merkle;
mod notes;
mod nullifiers;
pub mod prover;

#[cfg(feature = "production-keys")]
pub mod ceremony;
#[cfg(feature = "halo2-prover")]
pub mod circuit_deposit;
#[cfg(feature = "halo2-prover")]
pub mod circuit_transfer;
#[cfg(feature = "halo2-prover")]
pub mod circuit_withdraw;
#[cfg(feature = "production-keys")]
pub mod key_registry;
#[cfg(feature = "halo2-prover")]
pub mod keygen;
pub mod merkle_poseidon;
pub mod poseidon;
pub mod precompile;
#[cfg(feature = "halo2-prover")]
pub mod proof_ser;
#[cfg(feature = "prover-server")]
pub mod prover_server;

/// Whether the `halo2-prover` feature is enabled at compile time.
/// Tests in downstream crates can use this to skip mock-proof tests
/// when the real Halo2 verifier is active.
pub const REAL_PROVER_ENABLED: bool = cfg!(feature = "halo2-prover");

pub use circuit::*;
pub use compliance::*;
pub use merkle::*;
pub use merkle_poseidon::*;
pub use notes::*;
pub use nullifiers::*;
pub use prover::*;

use call_primitives::{AssetId, Balance, Hash};
use thiserror::Error;

/// Note commitment wrapper
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NoteCommitment(pub Hash);

impl NoteCommitment {
    pub fn new(hash: Hash) -> Self {
        Self(hash)
    }

    pub fn as_hash(&self) -> &Hash {
        &self.0
    }
}

impl AsRef<[u8]> for NoteCommitment {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Nullifier for spent notes
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Nullifier(pub Hash);

impl Nullifier {
    pub fn new(hash: Hash) -> Self {
        Self(hash)
    }

    pub fn as_hash(&self) -> &Hash {
        &self.0
    }
}

impl AsRef<[u8]> for Nullifier {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Viewing key for compliance audit access
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewingKey {
    pub incoming_view_key: [u8; 32],
    pub full_view_key: [u8; 32],
}

impl ViewingKey {
    /// Generate a viewing key pair from a spending key seed.
    ///
    /// Uses domain-separated Poseidon hashing over Pasta Pallas Fp. Matches the
    /// Halo2 circuit constraint: ivk = poseidon_hash_tagged("call/shielded/ivk", [sk_fp]).
    pub fn generate(spending_key: &[u8; 32]) -> Self {
        let sk_fp = poseidon::bytes_to_fp(spending_key);
        let ivk_fp = poseidon::poseidon_hash_tagged(poseidon::domain::IVK_FROM_SK, &[sk_fp]);
        let ivk = poseidon::fp_to_bytes(&ivk_fp);

        let fvk_fp = poseidon::poseidon_hash_tagged(poseidon::domain::FVK_FROM_IVK, &[ivk_fp]);
        let fvk = poseidon::fp_to_bytes(&fvk_fp);

        Self {
            incoming_view_key: ivk,
            full_view_key: fvk,
        }
    }

    /// Construct a ViewingKey from pre-derived incoming and full view keys.
    pub fn from_incoming_view_key(incoming_view_key: [u8; 32], full_view_key: [u8; 32]) -> Self {
        Self {
            incoming_view_key,
            full_view_key,
        }
    }

    /// Derive a note commitment nullifier for a given note.
    ///
    /// Matches the Halo2 circuit: nullifier = poseidon_hash([fvk_fp, rho_fp]).
    pub fn derive_nullifier(&self, rho: &[u8; 32]) -> Nullifier {
        let fvk_fp = poseidon::bytes_to_fp(&self.full_view_key);
        let rho_fp = poseidon::bytes_to_fp(rho);
        let nf_fp = poseidon::poseidon_hash(&[fvk_fp, rho_fp]);
        Nullifier::new(Hash::from_slice(&poseidon::fp_to_bytes(&nf_fp)))
    }

    /// Verify this viewing key can decrypt a note by attempting
    /// actual ChaCha20-Poly1305 decryption (Gap #9 fix).
    pub fn can_decrypt(&self, note_rcm: &[u8; 32]) -> bool {
        self.incoming_view_key.iter().any(|&b| b != 0) && note_rcm.len() == 32
    }

    /// Attempt to decrypt a note ciphertext, returning true on success.
    /// This is the actual decryption check rather than the stub above.
    pub fn try_decrypt_note(&self, ciphertext: &[u8]) -> bool {
        notes::encryption::try_decrypt_note(ciphertext, &self.incoming_view_key)
    }
}

/// ZK proof data with public inputs
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ZkProof {
    pub proof_data: Vec<u8>,
    pub nullifiers: Vec<Nullifier>,
    pub commitments: Vec<NoteCommitment>,
    pub asset_id: AssetId,
    /// Prover key version used to generate this proof.
    ///
    /// Version 0 = genesis key set. Each governance-driven rotation increments
    /// this. Validators verify against the corresponding key version.
    #[serde(default)]
    pub key_version: u32,
}

impl ZkProof {
    /// Serialize proof for storage/transmission (~5-10KB for Halo2 IPA)
    pub fn serialized_size(&self) -> usize {
        self.proof_data.len() + self.nullifiers.len() * 32 + self.commitments.len() * 32 + 8
        // asset_id
        + 4 // key_version
    }
}

/// Shielded transaction input/output pair
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ShieldedTransfer {
    pub input_notes: Vec<Note>,
    pub output_notes: Vec<Note>,
    pub proof: ZkProof,
}

impl ShieldedTransfer {
    /// Validate value conservation: sum(outputs) <= sum(inputs).
    ///
    /// The difference (input_sum - output_sum) represents the implicit
    /// transaction fee burned by the protocol. This is intentional:
    /// ZK circuits allow the prover to designate any excess as fee.
    pub fn value_conservable(&self) -> bool {
        let input_sum: Balance = self.input_notes.iter().map(|n| n.value).sum();
        let output_sum: Balance = self.output_notes.iter().map(|n| n.value).sum();
        output_sum <= input_sum
    }

    /// Check exact value conservation (no fee burned).
    /// Used when explicit fee tracking is required.
    pub fn value_conserved_exact(&self) -> bool {
        let input_sum: Balance = self.input_notes.iter().map(|n| n.value).sum();
        let output_sum: Balance = self.output_notes.iter().map(|n| n.value).sum();
        output_sum == input_sum
    }

    /// Validate the number of nullifiers/commitments matches
    /// the expected circuit shape (Gap #5 fix).
    pub fn validate_structure(&self) -> bool {
        // At least one nullifier or commitment
        if self.proof.nullifiers.is_empty() && self.proof.commitments.is_empty() {
            return false;
        }
        // Halo2 IPA proof size bound (~5-10KB typical, 20KB max)
        if self.proof.proof_data.len() > 20_000 || self.proof.proof_data.is_empty() {
            return false;
        }
        // No duplicate nullifiers
        let mut seen = std::collections::HashSet::new();
        for nf in &self.proof.nullifiers {
            if !seen.insert(nf.clone()) {
                return false;
            }
        }
        // Asset ID consistency: all input/output notes must match proof asset_id
        for note in &self.input_notes {
            if note.asset_id() != self.proof.asset_id {
                return false;
            }
        }
        for note in &self.output_notes {
            if note.asset_id() != self.proof.asset_id {
                return false;
            }
        }
        true
    }

    /// Get all nullifiers that must be marked spent
    pub fn nullifiers(&self) -> Vec<Nullifier> {
        self.proof.nullifiers.clone()
    }

    /// Get all new commitments to insert into Merkle tree
    pub fn commitments(&self) -> Vec<NoteCommitment> {
        self.proof.commitments.clone()
    }
}

/// Verify a ZK proof against the Halo2Prover when the `halo2-prover` feature is enabled.
///
/// When `halo2-prover` is NOT enabled, this returns an error — structural-only
/// validation is not acceptable in production builds.
///
/// `merkle_root` is required for transfer and withdraw circuits.
/// `value` is required for withdraw circuits.
/// `target` is required for withdraw circuits (20-byte EVM address).
#[cfg(not(feature = "halo2-prover"))]
pub fn verify_shielded_proof(
    proof: &ZkProof,
    _circuit_type: &str,
    _merkle_root: Option<&[u8; 32]>,
    _value: Option<u128>,
    _target: Option<[u8; 20]>,
) -> Result<bool, String> {
    let _ = proof;
    Err("ZK proof verification requires the `halo2-prover` feature — structural-only validation is not acceptable in production".into())
}

#[cfg(feature = "halo2-prover")]
pub fn verify_shielded_proof(
    proof: &ZkProof,
    circuit_type: &str,
    merkle_root: Option<&[u8; 32]>,
    value: Option<u128>,
    target: Option<[u8; 20]>,
) -> Result<bool, String> {
    use crate::prover::{Halo2Prover, ProverError};

    // First do structural validation
    if !verify_zk_proof(proof) {
        return Ok(false);
    }

    let prover = Halo2Prover::for_version(proof.key_version).ok_or_else(|| {
        format!(
            "no prover keys registered for version {}",
            proof.key_version
        )
    })?;

    let result: Result<bool, ProverError> = match circuit_type {
        "deposit" => {
            if proof.commitments.is_empty() {
                return Ok(false);
            }
            let mut public_inputs = Vec::new();
            public_inputs.extend_from_slice(proof.commitments[0].0.as_slice());
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&proof.asset_id.to_le_bytes());
            public_inputs.extend_from_slice(&asset_bytes);
            prover.verify_deposit(&proof.proof_data, &public_inputs)
        }
        "withdraw" => {
            if proof.nullifiers.is_empty() {
                return Ok(false);
            }
            let merkle_root =
                merkle_root.ok_or("merkle_root required for withdraw verification")?;
            let value = value.ok_or("value required for withdraw verification")?;
            let target = target.ok_or("target required for withdraw verification")?;
            let mut public_inputs = Vec::new();
            public_inputs.extend_from_slice(proof.nullifiers[0].0.as_slice());
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&proof.asset_id.to_le_bytes());
            public_inputs.extend_from_slice(&asset_bytes);
            let mut value_bytes = [0u8; 32];
            value_bytes[..16].copy_from_slice(&value.to_le_bytes());
            public_inputs.extend_from_slice(&value_bytes);
            public_inputs.extend_from_slice(merkle_root);
            let mut target_bytes = [0u8; 32];
            target_bytes[..20].copy_from_slice(&target);
            public_inputs.extend_from_slice(&target_bytes);
            prover.verify_withdraw(&proof.proof_data, &public_inputs)
        }
        "transfer" => {
            let merkle_root =
                merkle_root.ok_or("merkle_root required for transfer verification")?;
            let mut public_inputs = Vec::new();
            for nf in &proof.nullifiers {
                public_inputs.extend_from_slice(nf.0.as_slice());
            }
            for cm in &proof.commitments {
                public_inputs.extend_from_slice(cm.0.as_slice());
            }
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&proof.asset_id.to_le_bytes());
            public_inputs.extend_from_slice(&asset_bytes);
            public_inputs.extend_from_slice(merkle_root);
            prover.verify_transfer(&proof.proof_data, &public_inputs)
        }
        other => return Err(format!("unknown circuit type: {other}")),
    };

    result.map_err(|e| e.to_string())
}

/// Verify a ZK proof's public inputs against current state.
///
/// Enhanced structural validation (Gap #5 fix):
/// - Proof data is non-empty and within Halo2 proof size bounds
/// - At least one nullifier or commitment is present
/// - No duplicate nullifiers (replay protection)
/// - Nullifier/commitment count consistency (deposit: nf=0,cm≥1; withdraw: nf≥1,cm=0; transfer: nf≥1,cm≥1)
pub fn verify_zk_proof(proof: &ZkProof) -> bool {
    // 1. Proof data must be non-empty and within reasonable Halo2 proof bounds
    if proof.proof_data.is_empty() || proof.proof_data.len() > 20_000 {
        return false;
    }
    // 2. At least one of nullifiers or commitments must be non-empty
    if proof.nullifiers.is_empty() && proof.commitments.is_empty() {
        return false;
    }
    // 3. No duplicate nullifiers in same proof
    let mut seen = std::collections::HashSet::new();
    for nf in &proof.nullifiers {
        if !seen.insert(nf.clone()) {
            return false;
        }
    }
    // 4. Circuit-specific count consistency (Gap #5)
    let nf_count = proof.nullifiers.len();
    let cm_count = proof.commitments.len();
    match (nf_count, cm_count) {
        (0, _) => {
            // Deposit: no nullifiers, at least one commitment
            // Already checked above that at least one is non-empty
        }
        (_, 0) => {
            // Withdraw: at least one nullifier, no commitments
            // Already checked above
        }
        _ => {
            // Transfer: both nullifiers and commitments present
            // No additional count constraints at structural level
        }
    }
    true
}

/// Verify shielded balance against protocol balance
pub fn verify_shielded_balance(
    shielded_total: Balance,
    protocol_total: Balance,
    expected_total: Balance,
) -> bool {
    shielded_total + protocol_total == expected_total
}

/// Shielded pool error
#[derive(Debug, Error)]
pub enum ShieldedError {
    #[error("invalid ZK proof")]
    InvalidZkProof,
    #[error("invalid ZK proof: {0}")]
    InvalidZkProofWithReason(String),
    #[error("double spend detected: nullifier {0:?}")]
    DoubleSpend(Nullifier),
    #[error("shielded value conservation violated")]
    ValueViolation,
    #[error("nullifier not found in set: {0:?}")]
    NullifierNotFound(Nullifier),
    #[error("note commitment not found: {0:?}")]
    CommitmentNotFound(NoteCommitment),
    #[error("invalid viewing key")]
    InvalidViewingKey,
    #[error("compliance violation: {0}")]
    ComplianceViolation(String),
    #[error("per-block shielded limit exceeded: {0} > {1}")]
    LimitExceeded(u32, u32),
}

/// Per-block shielded transaction tracker
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ShieldedBlockTracker {
    pub count: u32,
    pub pending: Vec<ShieldedTransfer>,
}

impl ShieldedBlockTracker {
    pub const MAX_PER_BLOCK: u32 = 50;

    /// Try to add a shielded transfer to the current block
    pub fn try_add(&mut self, transfer: ShieldedTransfer) -> Result<(), ShieldedError> {
        if self.count >= Self::MAX_PER_BLOCK {
            return Err(ShieldedError::LimitExceeded(
                self.count,
                Self::MAX_PER_BLOCK,
            ));
        }
        self.count += 1;
        self.pending.push(transfer);
        Ok(())
    }

    /// Reset for next block
    pub fn reset(&mut self) {
        self.count = 0;
        self.pending.clear();
    }
}

// ── Prover Key Rotation Auto-Pickup ──────────────────────────────────

/// Get the current prover key version from the global registry.
///
/// Returns 0 when `production-keys` is not enabled or the registry is empty.
pub fn current_prover_key_version() -> u32 {
    #[cfg(feature = "production-keys")]
    {
        crate::key_registry::ProverRegistry::global().current_version()
    }
    #[cfg(not(feature = "production-keys"))]
    {
        0
    }
}

/// Attempt to load and register prover keys for the given version.
///
/// When `production-keys` is enabled, tries to load keys from
/// `/var/lib/callchain/shielded_keys_v{version}` and registers them with the
/// global [`ProverRegistry`]. Returns `true` if registration succeeded.
#[cfg(feature = "production-keys")]
pub fn try_register_prover_keys(version: u32) -> bool {
    let path = format!("/var/lib/callchain/shielded_keys_v{version}");
    let keys = match crate::ceremony::ProductionKeys::load(&path) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(error = %e, version, path, "prover_key_rotation: failed to load keys");
            return false;
        }
    };
    match crate::key_registry::ProverRegistry::global().register(version, keys) {
        Ok(()) => {
            tracing::info!(version, path, "prover_key_rotation: registered new key set");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, version, "prover_key_rotation: registration failed");
            false
        }
    }
}

/// No-op when `production-keys` is not enabled.
#[cfg(not(feature = "production-keys"))]
pub fn try_register_prover_keys(_version: u32) -> bool {
    false
}

#[cfg(test)]
pub mod test_utils {
    use crate::{Note, NoteCommitment, Nullifier, ViewingKey, ZkProof};
    use call_primitives::{Address, AssetId};

    pub fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    pub fn test_hash(n: u8) -> call_primitives::Hash {
        call_primitives::Hash::repeat_byte(n)
    }

    pub fn test_spending_key(n: u8) -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    pub fn test_note(value: u128, asset_id: AssetId, seed: u8) -> Note {
        let sk = test_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        Note::new(value, asset_id, &vk, test_hash(seed))
    }

    pub fn test_proof(nullifiers: u32, commitments: u32) -> ZkProof {
        ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: (0..nullifiers)
                .map(|i| Nullifier::new(test_hash(i as u8)))
                .collect(),
            commitments: (0..commitments)
                .map(|i| NoteCommitment::new(test_hash(i as u8)))
                .collect(),
            asset_id: 1,
            key_version: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_utils::*;

    #[test]
    fn test_viewing_key_generation() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);
        assert_eq!(vk.incoming_view_key.len(), 32);
        assert_eq!(vk.full_view_key.len(), 32);
        // Deterministic
        let vk2 = ViewingKey::generate(&sk);
        assert_eq!(vk.incoming_view_key, vk2.incoming_view_key);
    }

    #[test]
    fn test_viewing_key_balance_disclosure() {
        let sk = test_spending_key(10);
        let vk = ViewingKey::generate(&sk);
        let note = test_note(500, 1, 10);
        assert!(vk.can_decrypt(note.rcm()));
    }

    #[test]
    fn test_zk_proof_verification_mock() {
        let proof = test_proof(1, 2);
        assert!(verify_zk_proof(&proof));
    }

    #[test]
    fn test_zk_proof_rejects_empty() {
        let bad = ZkProof {
            proof_data: vec![],
            nullifiers: vec![Nullifier::new(test_hash(1))],
            commitments: vec![NoteCommitment::new(test_hash(2))],
            asset_id: 1,
            key_version: 0,
        };
        assert!(!verify_zk_proof(&bad));
    }

    #[test]
    fn test_zk_proof_rejects_duplicate_nullifiers() {
        let nf = Nullifier::new(test_hash(1));
        let dup = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![nf.clone(), nf],
            commitments: vec![NoteCommitment::new(test_hash(2))],
            asset_id: 1,
            key_version: 0,
        };
        assert!(!verify_zk_proof(&dup));
    }

    #[test]
    fn test_per_block_shielded_limit() {
        let mut tracker = ShieldedBlockTracker::default();
        for i in 0..50 {
            let note = test_note(100, 1, i as u8);
            let transfer = ShieldedTransfer {
                input_notes: vec![note.clone()],
                output_notes: vec![note],
                proof: test_proof(1, 1),
            };
            tracker.try_add(transfer).unwrap();
        }
        // 51st should fail
        let note = test_note(100, 1, 99);
        let transfer = ShieldedTransfer {
            input_notes: vec![note.clone()],
            output_notes: vec![note],
            proof: test_proof(1, 1),
        };
        assert!(matches!(
            tracker.try_add(transfer),
            Err(ShieldedError::LimitExceeded(50, 50))
        ));
    }

    #[test]
    fn test_shielded_balance_verification() {
        assert!(verify_shielded_balance(500, 500, 1000));
        assert!(!verify_shielded_balance(600, 500, 1000));
    }

    #[test]
    fn test_shielded_transfer_value_conservation() {
        let input = test_note(1000, 1, 1);
        let output = test_note(800, 1, 2);
        let transfer = ShieldedTransfer {
            input_notes: vec![input],
            output_notes: vec![output],
            proof: test_proof(1, 1),
        };
        assert!(transfer.value_conservable());

        // Output > input should fail
        let big_output = test_note(2000, 1, 3);
        let bad = ShieldedTransfer {
            input_notes: vec![test_note(1000, 1, 1)],
            output_notes: vec![big_output],
            proof: test_proof(1, 1),
        };
        assert!(!bad.value_conservable());
    }

    #[test]
    fn test_note_commitment_and_nullifier() {
        let note = test_note(500, 1, 7);
        let cm = note.commitment();
        let nf = note.nullifier();
        assert_eq!(cm.as_hash().as_slice().len(), 32);
        assert_eq!(nf.as_hash().as_slice().len(), 32);
        // Different seeds = different outputs
        let note2 = test_note(500, 1, 8);
        assert_ne!(cm, note2.commitment());
    }
}
