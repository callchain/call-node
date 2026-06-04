//! Shielded precompile entry point (0x202).
//!
//! Thin wrapper that routes EVM calls to [`ShieldedStorage`] backed by
//! EVM storage. Business logic lives in [`ShieldedStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use call_precompile::storage::StorageProvider;
use call_precompile::{
    address_to_u256, check_compliance, dispatch, slot_balance, storage::storage_slot,
    u128_to_u256, u256_to_u128, u256_to_u64, u64_to_u256, StorageRef, ASSET_ADDRESS,
};
use call_primitives::Hash;
use call_protocol::storage_backend::StorageBackend;
use revm_precompile::{PrecompileError, PrecompileResult};

pub const SHIELDED_ADDRESS: Address = address!("0000000000000000000000000000000000000202");

// ── Storage slot helpers ──────────────────────────────────────────────

fn slot_shielded_merkle_root() -> U256 {
    storage_slot(&[b"merkle_root"])
}

fn slot_shielded_nullifier(nullifier: [u8; 32]) -> U256 {
    storage_slot(&[b"nullifier", &nullifier[..]])
}

fn slot_shielded_commitment_count() -> U256 {
    storage_slot(&[b"cm_count"])
}

fn slot_shielded_commitment(index: u64) -> U256 {
    storage_slot(&[b"commitment", &index.to_be_bytes()[..]])
}

fn slot_tree_node(level: u8, index: u64) -> U256 {
    storage_slot(&[b"tree", &[level][..], &index.to_be_bytes()[..]])
}

fn slot_shielded_nullifier_height(nullifier: [u8; 32]) -> U256 {
    storage_slot(&[b"nf_height", &nullifier[..]])
}

fn slot_shielded_commitment_height(index: u64) -> U256 {
    storage_slot(&[b"cm_height", &index.to_be_bytes()[..]])
}

fn slot_shielded_nullifier_bitset(bucket: u64) -> U256 {
    storage_slot(&[b"nf_bits", &bucket.to_be_bytes()[..]])
}

fn slot_shielded_window_start() -> U256 {
    storage_slot(&[b"window_start"])
}

fn slot_shielded_window_blocks() -> U256 {
    storage_slot(&[b"window_blocks"])
}

fn slot_shielded_spent_nullifier(index: u64) -> U256 {
    storage_slot(&[b"spent_nf", &index.to_be_bytes()[..]])
}

fn slot_shielded_spent_nullifier_count() -> U256 {
    storage_slot(&[b"spent_nf_count"])
}

fn slot_shielded_paused() -> U256 {
    storage_slot(&[b"paused"])
}

fn slot_shielded_commitment_exists(commitment: [u8; 32]) -> U256 {
    storage_slot(&[b"cm_exists", &commitment[..]])
}

fn slot_shielded_addr_ops(addr: Address) -> U256 {
    storage_slot(&[b"addr_ops", addr.as_slice()])
}

fn slot_shielded_addr_ops_block(addr: Address) -> U256 {
    storage_slot(&[b"addr_blk", addr.as_slice()])
}

fn slot_shielded_root_history_count() -> U256 {
    storage_slot(&[b"root_hist_count"])
}

fn slot_shielded_root_history(index: u64) -> U256 {
    storage_slot(&[b"root_hist", &index.to_be_bytes()[..]])
}

fn slot_shielded_governance() -> U256 {
    storage_slot(&[b"governance"])
}

fn slot_shielded_last_prune_block() -> U256 {
    storage_slot(&[b"last_prune_blk"])
}

/// Number of blocks a note remains spendable after insertion (~1 week at 6s/block).
pub const NOTE_WINDOW_BLOCKS: u64 = 100_800;

/// Number of BitSet buckets for nullifier compression (4096 * 256 bits = 1,048,576 bits).
const NULLIFIER_BITSET_BUCKETS: u64 = 4096;

/// Maximum number of commitments before the Merkle tree refuses new insertions.
/// At depth 32 this is ~4B, but we cap far lower to keep EVM storage growth bounded.
/// When reached, users must spend old notes (withdraw/transfer) before new deposits.
const MAX_COMMITMENTS: u64 = 1_000_000;

/// Maximum shielded operations per address per block.
const MAX_OPS_PER_ADDRESS_PER_BLOCK: u64 = 20;

/// Maximum number of historical Merkle roots to retain for proof validity.
const MAX_ROOT_HISTORY: u64 = 256;

/// Minimum amount allowed for deposit / withdraw to prevent dust spam.
const MIN_SHIELDED_AMOUNT: u128 = 1;

/// Maximum nullifiers allowed in a single transfer call.
const MAX_TRANSFER_NULLIFIERS: usize = 16;

/// Maximum commitments allowed in a single transfer call.
const MAX_TRANSFER_COMMITMENTS: usize = 16;

/// Minimum block interval between external prune calls.
const MIN_PRUNE_INTERVAL_BLOCKS: u64 = 100;

/// Maximum nullifiers to prune in a single call to prevent gas exhaustion.
const MAX_PRUNE_PER_CALL: u64 = 1000;

// ── Error type ────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ShieldedError {
    InsufficientBalance,
    BalanceOverflow,
    MerkleRootMismatch,
    InvalidZkProof,
    ZkProofError(String),
    NullifierAlreadySpent,
    EmptyBatch,
    NoteExpired,
    MerkleTreeFull,
    Paused,
    CommitmentAlreadyExists,
    RateLimitExceeded,
    InvalidAmount,
    Unauthorized,
    PruneTooFrequent,
}

impl core::fmt::Display for ShieldedError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ShieldedError::InsufficientBalance => write!(f, "insufficient balance"),
            ShieldedError::BalanceOverflow => write!(f, "balance overflow"),
            ShieldedError::MerkleRootMismatch => write!(f, "merkle root mismatch"),
            ShieldedError::InvalidZkProof => write!(f, "invalid ZK proof"),
            ShieldedError::ZkProofError(e) => write!(f, "ZK verification error: {e}"),
            ShieldedError::NullifierAlreadySpent => write!(f, "nullifier already spent"),
            ShieldedError::EmptyBatch => write!(f, "empty batch"),
            ShieldedError::NoteExpired => write!(f, "note expired"),
            ShieldedError::MerkleTreeFull => write!(f, "merkle tree commitment cap reached"),
            ShieldedError::Paused => write!(f, "shielded pool is paused"),
            ShieldedError::CommitmentAlreadyExists => write!(f, "commitment already exists"),
            ShieldedError::RateLimitExceeded => write!(f, "per-address rate limit exceeded"),
            ShieldedError::InvalidAmount => write!(f, "amount must be greater than zero"),
            ShieldedError::Unauthorized => write!(f, "unauthorized caller"),
            ShieldedError::PruneTooFrequent => write!(f, "prune called too frequently"),
        }
    }
}

// ── Empty hash cache ──────────────────────────────────────────────────

thread_local! {
    static EMPTY_HASH_CACHE: std::cell::RefCell<Vec<[u8; 32]>> =
        std::cell::RefCell::new(vec![[0u8; 32]]);
}

fn empty_hash(level: usize) -> [u8; 32] {
    EMPTY_HASH_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        while cache.len() <= level {
            let last = cache[cache.len() - 1];
            cache.push(crate::poseidon::poseidon_hash_pair(&last, &last));
        }
        cache[level]
    })
}

// ── ShieldedStorage ───────────────────────────────────────────────────

pub struct ShieldedStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> ShieldedStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    fn load_bal(&mut self, asset_id: u64, addr: Address) -> u128 {
        u256_to_u128(
            self.backend
                .load(ASSET_ADDRESS, slot_balance(asset_id, addr)),
        )
    }

    fn save_bal(&mut self, asset_id: u64, addr: Address, amount: u128) {
        self.backend.store(
            ASSET_ADDRESS,
            slot_balance(asset_id, addr),
            u128_to_u256(amount),
        );
    }

    fn sload_shielded(&mut self, slot: U256) -> U256 {
        self.backend.load(SHIELDED_ADDRESS, slot)
    }

    fn sstore_shielded(&mut self, slot: U256, value: U256) {
        self.backend.store(SHIELDED_ADDRESS, slot, value);
    }

    /// Check if a nullifier has been spent. Returns true permanently
    /// once the spent flag is set. Never returns false for expiry.
    fn check_nullifier_spent(&mut self, nullifier: [u8; 32]) -> bool {
        self.sload_shielded(slot_shielded_nullifier(nullifier))
            .to_be_bytes::<32>()[31]
            == 1
    }

    /// Check if a spent nullifier is old enough to be pruned.
    fn is_nullifier_expired(&mut self, nullifier: [u8; 32], current_block: u64) -> bool {
        let spent_at = u256_to_u64(self.sload_shielded(slot_shielded_nullifier_height(nullifier)));
        if spent_at == 0 {
            return false;
        }
        let window = self.load_window_blocks();
        current_block.saturating_sub(spent_at) >= window
    }

    /// Mark a nullifier as spent, recording the block height and updating the BitSet.
    fn mark_nullifier_spent(&mut self, nullifier: [u8; 32], current_block: u64) {
        self.sstore_shielded(slot_shielded_nullifier(nullifier), U256::from(1u8));
        self.sstore_shielded(
            slot_shielded_nullifier_height(nullifier),
            u64_to_u256(current_block),
        );
        // Update BitSet for fast pruning / compression
        let bucket =
            u64::from_le_bytes(nullifier[0..8].try_into().unwrap()) % NULLIFIER_BITSET_BUCKETS;
        let bit = (nullifier[8] as u64) % 256;
        let mut word = self
            .sload_shielded(slot_shielded_nullifier_bitset(bucket))
            .to_be_bytes::<32>();
        let byte_idx = (bit / 8) as usize;
        let bit_idx = (bit % 8) as u8;
        word[byte_idx] |= 1u8 << bit_idx;
        self.sstore_shielded(
            slot_shielded_nullifier_bitset(bucket),
            U256::from_be_slice(&word),
        );
        // Append to spent-nullifier log so prune can iterate
        let count = u256_to_u64(self.sload_shielded(slot_shielded_spent_nullifier_count()));
        self.sstore_shielded(
            slot_shielded_spent_nullifier(count),
            U256::from_be_slice(&nullifier),
        );
        self.sstore_shielded(
            slot_shielded_spent_nullifier_count(),
            u64_to_u256(count + 1),
        );
    }

    /// Prune nullifiers whose spent height is older than the note window.
    /// Clears the spent flag, height, and BitSet bit for expired entries.
    /// Returns the number of nullifiers pruned.
    fn prune_expired_nullifiers(&mut self, current_block: u64) -> u64 {
        let window = self.load_window_blocks();
        let count = u256_to_u64(self.sload_shielded(slot_shielded_spent_nullifier_count()));
        let mut pruned = 0u64;
        let mut new_count = count;
        let limit = count.min(MAX_PRUNE_PER_CALL);
        for i in 0..limit {
            let nf = self
                .sload_shielded(slot_shielded_spent_nullifier(i))
                .to_be_bytes::<32>();
            // Stop at first unexpired nullifier — log is append-only in order
            let spent_at = u256_to_u64(self.sload_shielded(slot_shielded_nullifier_height(nf)));
            if spent_at == 0 || current_block.saturating_sub(spent_at) < window {
                // Not expired yet — since log is ordered by insertion, all subsequent
                // entries are also unexpired (or at least not older). Break.
                break;
            }
            // Clear storage
            self.sstore_shielded(slot_shielded_nullifier(nf), U256::ZERO);
            self.sstore_shielded(slot_shielded_nullifier_height(nf), U256::ZERO);
            // Clear BitSet bit
            let bucket = u64::from_le_bytes(nf[0..8].try_into().unwrap()) % NULLIFIER_BITSET_BUCKETS;
            let bit = (nf[8] as u64) % 256;
            let mut word = self
                .sload_shielded(slot_shielded_nullifier_bitset(bucket))
                .to_be_bytes::<32>();
            let byte_idx = (bit / 8) as usize;
            let bit_idx = (bit % 8) as u8;
            word[byte_idx] &= !(1u8 << bit_idx);
            self.sstore_shielded(
                slot_shielded_nullifier_bitset(bucket),
                U256::from_be_slice(&word),
            );
            pruned += 1;
            new_count -= 1;
        }
        if pruned > 0 {
            // Compact log by shifting remaining entries to the front
            for i in 0..new_count {
                let nf = self
                    .sload_shielded(slot_shielded_spent_nullifier(i + pruned))
                    .to_be_bytes::<32>();
                self.sstore_shielded(slot_shielded_spent_nullifier(i), U256::from_be_slice(&nf));
            }
            self.sstore_shielded(
                slot_shielded_spent_nullifier_count(),
                u64_to_u256(new_count),
            );
        }
        pruned
    }

    /// Load the configured note window in blocks (default: NOTE_WINDOW_BLOCKS).
    fn load_window_blocks(&mut self) -> u64 {
        let stored = u256_to_u64(self.sload_shielded(slot_shielded_window_blocks()));
        if stored == 0 {
            NOTE_WINDOW_BLOCKS
        } else {
            stored
        }
    }

    /// Return true if the shielded pool is paused.
    fn is_paused(&mut self) -> bool {
        self.sload_shielded(slot_shielded_paused()).to_be_bytes::<32>()[31] != 0
    }

    /// Revert if the pool is paused.
    fn require_not_paused(&mut self) -> Result<(), ShieldedError> {
        if self.is_paused() {
            Err(ShieldedError::Paused)
        } else {
            Ok(())
        }
    }

    /// Set the paused flag (0 = unpaused, 1 = paused).
    pub fn set_paused(&mut self, paused: bool) {
        self.sstore_shielded(
            slot_shielded_paused(),
            U256::from(if paused { 1u8 } else { 0 }),
        );
    }

    /// Check whether a commitment already exists (duplicate guard).
    fn commitment_exists(&mut self, commitment: [u8; 32]) -> bool {
        self.sload_shielded(slot_shielded_commitment_exists(commitment))
            .to_be_bytes::<32>()[31]
            != 0
    }

    /// Mark a commitment as existing in the dedup set.
    fn mark_commitment_exists(&mut self, commitment: [u8; 32]) {
        self.sstore_shielded(
            slot_shielded_commitment_exists(commitment),
            U256::from(1u8),
        );
    }

    /// Check and increment per-address operation rate limit.
    fn check_rate_limit(&mut self, addr: Address, current_block: u64) -> Result<(), ShieldedError> {
        let recorded_block = u256_to_u64(self.sload_shielded(slot_shielded_addr_ops_block(addr)));
        let count = if recorded_block == current_block {
            u256_to_u64(self.sload_shielded(slot_shielded_addr_ops(addr)))
        } else {
            0
        };
        if count >= MAX_OPS_PER_ADDRESS_PER_BLOCK {
            return Err(ShieldedError::RateLimitExceeded);
        }
        self.sstore_shielded(
            slot_shielded_addr_ops_block(addr),
            u64_to_u256(current_block),
        );
        self.sstore_shielded(
            slot_shielded_addr_ops(addr),
            u64_to_u256(count + 1),
        );
        Ok(())
    }

    /// Record the current Merkle root into the history buffer before it changes.
    fn record_root_history(&mut self) {
        let root = self.sload_shielded(slot_shielded_merkle_root());
        if root.is_zero() {
            return;
        }
        let count = u256_to_u64(self.sload_shielded(slot_shielded_root_history_count()));
        let idx = count % MAX_ROOT_HISTORY;
        self.sstore_shielded(slot_shielded_root_history(idx), root);
        self.sstore_shielded(
            slot_shielded_root_history_count(),
            u64_to_u256(count.saturating_add(1)),
        );
    }

    /// Return true if the given Merkle root matches the current root or is in the history.
    fn is_valid_root(&mut self, merkle_root: [u8; 32]) -> bool {
        let current = self
            .sload_shielded(slot_shielded_merkle_root())
            .to_be_bytes::<32>();
        if merkle_root == current {
            return true;
        }
        let count = u256_to_u64(self.sload_shielded(slot_shielded_root_history_count()));
        let limit = count.min(MAX_ROOT_HISTORY);
        for i in 0..limit {
            let idx = if count <= MAX_ROOT_HISTORY {
                i
            } else {
                (count - MAX_ROOT_HISTORY + i) % MAX_ROOT_HISTORY
            };
            let hist = self
                .sload_shielded(slot_shielded_root_history(idx))
                .to_be_bytes::<32>();
            if hist == merkle_root {
                return true;
            }
        }
        false
    }

    /// Load the governance address from storage.
    fn governance(&mut self) -> Address {
        let raw = self.sload_shielded(slot_shielded_governance());
        let bytes: [u8; 32] = raw.to_be_bytes();
        Address::from_slice(&bytes[12..32])
    }

    /// Set the governance address.
    pub fn set_governance(&mut self, addr: Address) {
        let mut bytes = [0u8; 32];
        bytes[12..32].copy_from_slice(addr.as_slice());
        self.sstore_shielded(slot_shielded_governance(), U256::from_be_slice(&bytes));
    }

    /// Revert if the caller is not the governance address.
    /// Requires governance to be set (non-zero) — initialization window is
    /// only for setGovernance, not for operational governance functions.
    fn require_governance(&mut self, caller: Address) -> Result<(), ShieldedError> {
        let gov = self.governance();
        if gov == Address::ZERO {
            return Err(ShieldedError::Unauthorized);
        }
        if caller != gov {
            return Err(ShieldedError::Unauthorized);
        }
        Ok(())
    }

    /// Check if enough blocks have passed since the last prune.
    /// First call (last == 0) is always allowed.
    fn check_prune_interval(&mut self, current_block: u64) -> Result<(), ShieldedError> {
        let last = u256_to_u64(self.sload_shielded(slot_shielded_last_prune_block()));
        if last != 0 && current_block.saturating_sub(last) < MIN_PRUNE_INTERVAL_BLOCKS {
            return Err(ShieldedError::PruneTooFrequent);
        }
        self.sstore_shielded(
            slot_shielded_last_prune_block(),
            u64_to_u256(current_block),
        );
        Ok(())
    }

    fn insert_commitment(&mut self, commitment: [u8; 32], current_block: u64) -> [u8; 32] {
        let count = u256_to_u64(self.sload_shielded(slot_shielded_commitment_count()));
        let mut idx = count as usize;
        let mut current = commitment;
        const DEPTH: usize = 32;

        for level in 0..DEPTH {
            self.sstore_shielded(
                slot_tree_node(level as u8, idx as u64),
                U256::from_be_slice(&current),
            );

            let is_left = idx & 1 == 0;
            let parent = if is_left {
                let sibling = empty_hash(level);
                crate::poseidon::poseidon_hash_pair(&current, &sibling)
            } else {
                let left = self
                    .sload_shielded(slot_tree_node(level as u8, (idx - 1) as u64))
                    .to_be_bytes::<32>();
                crate::poseidon::poseidon_hash_pair(&left, &current)
            };

            current = parent;
            idx >>= 1;
        }

        self.record_root_history();
        self.sstore_shielded(slot_shielded_merkle_root(), U256::from_be_slice(&current));
        // Record insertion height for expiry checks
        self.sstore_shielded(
            slot_shielded_commitment_height(count),
            u64_to_u256(current_block),
        );
        current
    }

    pub fn deposit(
        &mut self,
        asset_id: u64,
        amount: u128,
        commitment: [u8; 32],
        proof_data: Vec<u8>,
        key_version: u32,
        caller: Address,
        current_block: u64,
    ) -> Result<(), ShieldedError> {
        self.require_not_paused()?;
        self.check_rate_limit(caller, current_block)?;

        if amount == 0 || amount < MIN_SHIELDED_AMOUNT {
            return Err(ShieldedError::InvalidAmount);
        }

        if commitment == [0u8; 32] {
            return Err(ShieldedError::InvalidZkProof);
        }

        if proof_data.is_empty() || proof_data.len() > 20_000 {
            return Err(ShieldedError::InvalidZkProof);
        }

        let _ = key_version;

        #[cfg(feature = "halo2-prover")]
        {
            let proof = crate::ZkProof {
                proof_data: proof_data.clone(),
                nullifiers: vec![],
                commitments: vec![crate::NoteCommitment::new(Hash::from_slice(&commitment))],
                asset_id,
                key_version,
            };

            match crate::verify_shielded_proof(&proof, "deposit", None, Some(amount), None) {
                Ok(true) => {}
                Ok(false) => return Err(ShieldedError::InvalidZkProof),
                Err(e) => return Err(ShieldedError::ZkProofError(e)),
            }
        }

        if self.commitment_exists(commitment) {
            return Err(ShieldedError::CommitmentAlreadyExists);
        }

        let sender_bal = self
            .load_bal(asset_id, caller)
            .checked_sub(amount)
            .ok_or(ShieldedError::InsufficientBalance)?;
        self.save_bal(asset_id, caller, sender_bal);

        let count = u256_to_u64(self.sload_shielded(slot_shielded_commitment_count()));
        if count >= MAX_COMMITMENTS {
            return Err(ShieldedError::MerkleTreeFull);
        }

        self.insert_commitment(commitment, current_block);
        self.mark_commitment_exists(commitment);

        let count = u256_to_u64(self.sload_shielded(slot_shielded_commitment_count()));
        self.sstore_shielded(
            slot_shielded_commitment(count),
            U256::from_be_slice(&commitment),
        );
        self.sstore_shielded(slot_shielded_commitment_count(), u64_to_u256(count + 1));

        Ok(())
    }

    pub fn withdraw(
        &mut self,
        asset_id: u64,
        target: Address,
        amount: u128,
        nullifier: [u8; 32],
        merkle_root: [u8; 32],
        proof_data: Vec<u8>,
        key_version: u32,
        caller: Address,
        current_block: u64,
    ) -> Result<(), ShieldedError> {
        self.require_not_paused()?;
        self.check_rate_limit(caller, current_block)?;

        if amount == 0 || amount < MIN_SHIELDED_AMOUNT {
            return Err(ShieldedError::InvalidAmount);
        }

        if target == Address::ZERO {
            return Err(ShieldedError::InvalidAmount);
        }

        if !self.is_valid_root(merkle_root) {
            return Err(ShieldedError::MerkleRootMismatch);
        }

        if proof_data.is_empty() || proof_data.len() > 20_000 {
            return Err(ShieldedError::InvalidZkProof);
        }

        let _ = key_version;

        #[cfg(feature = "halo2-prover")]
        {
            let proof = crate::ZkProof {
                proof_data,
                nullifiers: vec![crate::Nullifier::new(Hash::from_slice(&nullifier))],
                commitments: vec![],
                asset_id,
                key_version,
            };

            let target_bytes: [u8; 20] = target.into();
            match crate::verify_shielded_proof(
                &proof,
                "withdraw",
                Some(&merkle_root),
                Some(amount),
                Some(target_bytes),
            ) {
                Ok(true) => {}
                Ok(false) => return Err(ShieldedError::InvalidZkProof),
                Err(e) => return Err(ShieldedError::ZkProofError(e)),
            }
        }

        if self.check_nullifier_spent(nullifier) {
            return Err(ShieldedError::NullifierAlreadySpent);
        }

        self.mark_nullifier_spent(nullifier, current_block);

        let target_bal = self
            .load_bal(asset_id, target)
            .checked_add(amount)
            .ok_or(ShieldedError::BalanceOverflow)?;
        self.save_bal(asset_id, target, target_bal);

        // Opportunistic prune of expired nullifiers
        self.prune_expired_nullifiers(current_block);

        Ok(())
    }

    pub fn transfer(
        &mut self,
        asset_id: u64,
        proof_data: Vec<u8>,
        nullifiers: Vec<[u8; 32]>,
        commitments: Vec<[u8; 32]>,
        merkle_root: [u8; 32],
        key_version: u32,
        caller: Address,
        current_block: u64,
    ) -> Result<(), ShieldedError> {
        self.require_not_paused()?;
        self.check_rate_limit(caller, current_block)?;

        if nullifiers.len() > MAX_TRANSFER_NULLIFIERS {
            return Err(ShieldedError::InvalidZkProof);
        }
        if commitments.len() > MAX_TRANSFER_COMMITMENTS {
            return Err(ShieldedError::InvalidZkProof);
        }

        if nullifiers.is_empty() || commitments.is_empty() {
            return Err(ShieldedError::EmptyBatch);
        }

        if nullifiers.len() != commitments.len() {
            return Err(ShieldedError::InvalidZkProof);
        }

        if !self.is_valid_root(merkle_root) {
            return Err(ShieldedError::MerkleRootMismatch);
        }

        if proof_data.is_empty() || proof_data.len() > 20_000 {
            return Err(ShieldedError::InvalidZkProof);
        }

        let _ = key_version;

        #[cfg(feature = "halo2-prover")]
        {
            let proof = crate::ZkProof {
                proof_data,
                nullifiers: nullifiers
                    .iter()
                    .map(|nf| crate::Nullifier::new(Hash::from_slice(nf)))
                    .collect(),
                commitments: commitments
                    .iter()
                    .map(|cm| crate::NoteCommitment::new(Hash::from_slice(cm)))
                    .collect(),
                asset_id,
                key_version,
            };

            match crate::verify_shielded_proof(&proof, "transfer", Some(&merkle_root), None, None) {
                Ok(true) => {}
                Ok(false) => return Err(ShieldedError::InvalidZkProof),
                Err(e) => return Err(ShieldedError::ZkProofError(e)),
            }
        }

        // Reject duplicate nullifiers within the same batch
        {
            let mut seen = std::collections::HashSet::new();
            for nf in &nullifiers {
                if !seen.insert(*nf) {
                    return Err(ShieldedError::NullifierAlreadySpent);
                }
            }
        }

        for nf in &nullifiers {
            if self.check_nullifier_spent(*nf) {
                return Err(ShieldedError::NullifierAlreadySpent);
            }
        }

        for nf in &nullifiers {
            self.mark_nullifier_spent(*nf, current_block);
        }

        for cm in &commitments {
            if self.commitment_exists(*cm) {
                return Err(ShieldedError::CommitmentAlreadyExists);
            }
            let count = u256_to_u64(self.sload_shielded(slot_shielded_commitment_count()));
            if count >= MAX_COMMITMENTS {
                return Err(ShieldedError::MerkleTreeFull);
            }
            self.insert_commitment(*cm, current_block);
            self.mark_commitment_exists(*cm);
            self.sstore_shielded(slot_shielded_commitment(count), U256::from_be_slice(cm));
            self.sstore_shielded(slot_shielded_commitment_count(), u64_to_u256(count + 1));
        }

        // Opportunistic prune of expired nullifiers
        self.prune_expired_nullifiers(current_block);

        Ok(())
    }

    pub fn get_merkle_root(&mut self) -> [u8; 32] {
        self.sload_shielded(slot_shielded_merkle_root())
            .to_be_bytes::<32>()
    }

    pub fn get_commitment_count(&mut self) -> u64 {
        u256_to_u64(self.sload_shielded(slot_shielded_commitment_count()))
    }

    pub fn get_commitment(&mut self, index: u64) -> [u8; 32] {
        let count = self.get_commitment_count();
        if index >= count {
            return [0u8; 32];
        }
        self.sload_shielded(slot_shielded_commitment(index))
            .to_be_bytes::<32>()
    }

    pub fn is_nullifier_spent(&mut self, nullifier: [u8; 32], _current_block: u64) -> bool {
        self.check_nullifier_spent(nullifier)
    }

    pub fn get_commitment_height(&mut self, index: u64) -> u64 {
        let count = self.get_commitment_count();
        if index >= count {
            return 0;
        }
        u256_to_u64(self.sload_shielded(slot_shielded_commitment_height(index)))
    }

    pub fn get_root_history_count(&mut self) -> u64 {
        u256_to_u64(self.sload_shielded(slot_shielded_root_history_count()))
    }

    pub fn get_root_history(&mut self, index: u64) -> [u8; 32] {
        let count = self.get_root_history_count();
        if index >= count {
            return [0u8; 32];
        }
        self.sload_shielded(slot_shielded_root_history(index))
            .to_be_bytes::<32>()
    }
}

// ── ShieldedPrecompile ────────────────────────────────────────────────

sol! {
    interface IProtocolShielded {
        event Deposit(address indexed sender, uint64 assetId, uint128 amount, bytes32 commitment);
        event Withdraw(address indexed target, uint64 assetId, uint128 amount, bytes32 nullifier);
        event Transfer(address indexed sender, uint64 assetId, bytes32[] nullifiers, bytes32[] commitments);

        function deposit(uint64 assetId, uint128 amount, bytes32 commitment, bytes proofData, uint32 keyVersion) external;
        function withdraw(uint64 assetId, address target, uint128 amount, bytes32 nullifier, bytes32 merkleRoot, bytes proofData, uint32 keyVersion) external;
        function transfer(uint64 assetId, bytes proof, bytes32[] nullifiers, bytes32[] commitments, bytes32 merkleRoot, uint32 keyVersion) external;
        function getMerkleRoot() external view returns (bytes32);
        function getCommitmentCount() external view returns (uint64);
        function getCommitment(uint64 index) external view returns (bytes32);
        function getCommitmentHeight(uint64 index) external view returns (uint64);
        function isNullifierSpent(bytes32 nullifier) external view returns (bool);
        function pruneNullifiers() external returns (uint64);
        function setPaused(bool paused) external;
        function isPaused() external view returns (bool);
        function getRootHistoryCount() external view returns (uint64);
        function getRootHistory(uint64 index) external view returns (bytes32);
        function setGovernance(address governance) external;
        function getGovernance() external view returns (address);
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ShieldedPrecompile;

impl ShieldedPrecompile {
    fn deposit(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        let call = dispatch::decode_call::<IProtocolShielded::depositCall>(calldata)?;

        // Dynamic gas: base 300_000 + 10 per byte of proof data
        let proof_len = call.proofData.len() as u64;
        let dynamic_gas = 300_000u64
            .saturating_add(proof_len.saturating_mul(10));
        storage.charge_gas(dynamic_gas)?;

        check_compliance(msg_sender, storage)?;
        let current_block = storage.block_number();
        let cp = storage.checkpoint();
        let mut store = ShieldedStorage::new(sr);
        let result = store
            .deposit(
                call.assetId,
                call.amount,
                call.commitment.into(),
                call.proofData.to_vec(),
                call.keyVersion,
                msg_sender,
                current_block,
            )
            .map_err(|e| PrecompileError::Other(e.to_string().into()));

        match result {
            Ok(()) => {
                storage.checkpoint_commit(cp);
                // Emit Deposit(sender, assetId, amount, commitment)
                let topic0 = alloy_primitives::keccak256(b"Deposit(address,uint64,uint128,bytes32)");
                let mut event_data = Vec::with_capacity(96);
                event_data.extend_from_slice(&call_precompile::u64_to_u256(call.assetId).to_be_bytes::<32>());
                event_data.extend_from_slice(&call_precompile::u128_to_u256(call.amount).to_be_bytes::<32>());
                event_data.extend_from_slice(call.commitment.as_slice());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, address_to_u256(msg_sender).to_be_bytes::<32>().into()],
                    alloy_primitives::Bytes::from(event_data),
                ) {
                    let _ = storage.emit_event(SHIELDED_ADDRESS, log);
                }
                let output = revm_precompile::PrecompileOutput::new(0, alloy_primitives::Bytes::new());
                Ok(call_precompile::storage::fill_precompile_output(output, storage))
            }
            Err(e) => {
                storage.checkpoint_revert(cp);
                Err(e)
            }
        }
    }

    fn withdraw(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        let call = dispatch::decode_call::<IProtocolShielded::withdrawCall>(calldata)?;

        // Dynamic gas: base 200_000 + 10 per byte of proof data
        let proof_len = call.proofData.len() as u64;
        let dynamic_gas = 200_000u64
            .saturating_add(proof_len.saturating_mul(10));
        storage.charge_gas(dynamic_gas)?;

        check_compliance(call.target, storage)?;
        let current_block = storage.block_number();

        let cp = storage.checkpoint();
        let mut store = ShieldedStorage::new(sr);
        let result = store
            .withdraw(
                call.assetId,
                call.target,
                call.amount,
                call.nullifier.into(),
                call.merkleRoot.into(),
                call.proofData.to_vec(),
                call.keyVersion,
                msg_sender,
                current_block,
            )
            .map_err(|e| PrecompileError::Other(e.to_string().into()));

        match result {
            Ok(()) => {
                storage.checkpoint_commit(cp);
                // Emit Withdraw(target, assetId, amount, nullifier)
                let topic0 = alloy_primitives::keccak256(b"Withdraw(address,uint64,uint128,bytes32)");
                let mut event_data = Vec::with_capacity(96);
                event_data.extend_from_slice(&u64_to_u256(call.assetId).to_be_bytes::<32>());
                event_data.extend_from_slice(&u128_to_u256(call.amount).to_be_bytes::<32>());
                event_data.extend_from_slice(call.nullifier.as_slice());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, address_to_u256(call.target).to_be_bytes::<32>().into()],
                    alloy_primitives::Bytes::from(event_data),
                ) {
                    let _ = storage.emit_event(SHIELDED_ADDRESS, log);
                }
                let output = revm_precompile::PrecompileOutput::new(0, alloy_primitives::Bytes::new());
                Ok(call_precompile::storage::fill_precompile_output(output, storage))
            }
            Err(e) => {
                storage.checkpoint_revert(cp);
                Err(e)
            }
        }
    }

    fn transfer(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        let call = dispatch::decode_call::<IProtocolShielded::transferCall>(calldata)?;

        // Dynamic gas: base 250_000 + 20_000 per commitment + 10_000 per nullifier + 10 per byte of proof data
        let proof_len = call.proof.len() as u64;
        let commitment_count = call.commitments.len() as u64;
        let nullifier_count = call.nullifiers.len() as u64;
        let dynamic_gas = 250_000u64
            .saturating_add(commitment_count.saturating_mul(20_000))
            .saturating_add(nullifier_count.saturating_mul(10_000))
            .saturating_add(proof_len.saturating_mul(10));
        storage.charge_gas(dynamic_gas)?;

        check_compliance(msg_sender, storage)?;
        let current_block = storage.block_number();
        let cp = storage.checkpoint();
        let mut store = ShieldedStorage::new(sr);
        let nullifiers: Vec<[u8; 32]> =
            call.nullifiers.iter().map(|n| (*n).into()).collect();
        let commitments: Vec<[u8; 32]> =
            call.commitments.iter().map(|c| (*c).into()).collect();
        let merkle_root: [u8; 32] = call.merkleRoot.into();
        let result = store
            .transfer(call.assetId, call.proof.to_vec(), nullifiers, commitments, merkle_root, call.keyVersion, msg_sender, current_block)
            .map_err(|e| PrecompileError::Other(e.to_string().into()));

        match result {
            Ok(()) => {
                storage.checkpoint_commit(cp);
                // Emit Transfer(sender, assetId, nullifiers, commitments)
                let topic0 = alloy_primitives::keccak256(
                    b"Transfer(address,uint64,bytes32[],bytes32[])"
                );
                // Standard Solidity ABI encoding for: uint64, bytes32[], bytes32[]
                // Static part (3 * 32 bytes): assetId | offset_nullifiers | offset_commitments
                // Dynamic part: length + elements for each array
                let nf_len = call.nullifiers.len();
                let cm_len = call.commitments.len();
                let nf_data_size = 32 + nf_len * 32;
                let offset_nf: u64 = 96; // 3 * 32 bytes of static part
                let offset_cm: u64 = offset_nf + nf_data_size as u64;
                let mut event_data = Vec::with_capacity(96 + nf_data_size + 32 + cm_len * 32);
                // assetId (uint64, left-padded)
                event_data.extend_from_slice(&u64_to_u256(call.assetId).to_be_bytes::<32>());
                // offset to nullifiers array
                event_data.extend_from_slice(&u64_to_u256(offset_nf).to_be_bytes::<32>());
                // offset to commitments array
                event_data.extend_from_slice(&u64_to_u256(offset_cm).to_be_bytes::<32>());
                // nullifiers array: length + elements
                event_data.extend_from_slice(&u64_to_u256(nf_len as u64).to_be_bytes::<32>());
                for nf in &call.nullifiers {
                    event_data.extend_from_slice(nf.as_slice());
                }
                // commitments array: length + elements
                event_data.extend_from_slice(&u64_to_u256(cm_len as u64).to_be_bytes::<32>());
                for cm in &call.commitments {
                    event_data.extend_from_slice(cm.as_slice());
                }
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![
                        topic0,
                        address_to_u256(msg_sender).to_be_bytes::<32>().into(),
                    ],
                    alloy_primitives::Bytes::from(event_data),
                ) {
                    let _ = storage.emit_event(SHIELDED_ADDRESS, log);
                }
                let output = revm_precompile::PrecompileOutput::new(0, alloy_primitives::Bytes::new());
                Ok(call_precompile::storage::fill_precompile_output(output, storage))
            }
            Err(e) => {
                storage.checkpoint_revert(cp);
                Err(e)
            }
        }
    }

    fn get_merkle_root(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolShielded::getMerkleRootCall, _, _>(
            calldata,
            1000,
            storage,
            |_call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                Ok(store.get_merkle_root())
            },
        )
    }

    fn get_commitment_count(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolShielded::getCommitmentCountCall, _, _>(
            calldata,
            1000,
            storage,
            |_call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                Ok(store.get_commitment_count())
            },
        )
    }

    fn get_commitment(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolShielded::getCommitmentCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                Ok(store.get_commitment(call.index))
            },
        )
    }

    fn get_commitment_height(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolShielded::getCommitmentHeightCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                Ok(store.get_commitment_height(call.index))
            },
        )
    }

    fn is_nullifier_spent(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolShielded::isNullifierSpentCall, _, _>(
            calldata,
            2000,
            storage,
            |call, storage| {
                let current_block = storage.block_number();
                let mut store = ShieldedStorage::new(sr);
                Ok(store.is_nullifier_spent(call.nullifier.into(), current_block))
            },
        )
    }

    fn prune_nullifiers(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate::<IProtocolShielded::pruneNullifiersCall, _, _>(
            calldata,
            3000,
            storage,
            |_call, storage| {
                let current_block = storage.block_number();
                let mut store = ShieldedStorage::new(sr);
                store
                    .check_prune_interval(current_block)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                let pruned = store.prune_expired_nullifiers(current_block);
                let prune_gas = 3000u64 + pruned * 500;
                storage.charge_gas(prune_gas)?;
                Ok(pruned)
            },
        )
    }

    fn set_paused(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate::<IProtocolShielded::setPausedCall, _, _>(
            calldata,
            5000,
            storage,
            |call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                store
                    .require_governance(msg_sender)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                store.set_paused(call.paused);
                Ok(())
            },
        )
    }

    fn set_governance(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate::<IProtocolShielded::setGovernanceCall, _, _>(
            calldata,
            5000,
            storage,
            |call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                let gov = store.governance();
                // Initialization: anyone can set governance when it's zero.
                // After initialization, only existing governance can change it.
                if gov != Address::ZERO && gov != msg_sender {
                    return Err(PrecompileError::Other(
                        "not authorized".to_string().into(),
                    ));
                }
                store.set_governance(call.governance);
                Ok(())
            },
        )
    }

    fn get_governance(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolShielded::getGovernanceCall, _, _>(
            calldata,
            1000,
            storage,
            |_call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                Ok(store.governance())
            },
        )
    }

    fn is_paused(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolShielded::isPausedCall, _, _>(
            calldata,
            1000,
            storage,
            |_call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                Ok(store.is_paused())
            },
        )
    }

    fn get_root_history_count(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolShielded::getRootHistoryCountCall, _, _>(
            calldata,
            1000,
            storage,
            |_call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                Ok(store.get_root_history_count())
            },
        )
    }

    fn get_root_history(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolShielded::getRootHistoryCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                Ok(store.get_root_history(call.index))
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for ShieldedPrecompile {
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("too short".into()));
        }
        let selector: [u8; 4] = calldata[..4]
            .try_into()
            .expect("invariant: 4-byte selector");
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolShielded::depositCall::SELECTOR => {
                self.deposit(calldata, msg_sender, storage, sr)
            }
            IProtocolShielded::withdrawCall::SELECTOR => {
                self.withdraw(calldata, msg_sender, storage, sr)
            }
            IProtocolShielded::transferCall::SELECTOR => {
                self.transfer(calldata, msg_sender, storage, sr)
            }
            IProtocolShielded::getMerkleRootCall::SELECTOR => {
                self.get_merkle_root(calldata, storage, sr)
            }
            IProtocolShielded::getCommitmentCountCall::SELECTOR => {
                self.get_commitment_count(calldata, storage, sr)
            }
            IProtocolShielded::getCommitmentCall::SELECTOR => {
                self.get_commitment(calldata, storage, sr)
            }
            IProtocolShielded::getCommitmentHeightCall::SELECTOR => {
                self.get_commitment_height(calldata, storage, sr)
            }
            IProtocolShielded::isNullifierSpentCall::SELECTOR => {
                self.is_nullifier_spent(calldata, storage, sr)
            }
            IProtocolShielded::pruneNullifiersCall::SELECTOR => {
                self.prune_nullifiers(calldata, storage, sr)
            }
            IProtocolShielded::setPausedCall::SELECTOR => {
                self.set_paused(calldata, msg_sender, storage, sr)
            }
            IProtocolShielded::isPausedCall::SELECTOR => {
                self.is_paused(calldata, storage, sr)
            }
            IProtocolShielded::getRootHistoryCountCall::SELECTOR => {
                self.get_root_history_count(calldata, storage, sr)
            }
            IProtocolShielded::getRootHistoryCall::SELECTOR => {
                self.get_root_history(calldata, storage, sr)
            }
            IProtocolShielded::setGovernanceCall::SELECTOR => {
                self.set_governance(calldata, msg_sender, storage, sr)
            }
            IProtocolShielded::getGovernanceCall::SELECTOR => {
                self.get_governance(calldata, storage, sr)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::StatefulPrecompile;
    use call_precompile::{slot_balance, u128_to_u256, u256_to_u128};
    use call_primitives::Address;

    #[test]
    fn test_shielded_address() {
        assert_eq!(
            SHIELDED_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000202")
        );
    }

    /// Deposit with empty proof must be rejected regardless of feature flags.
    #[test]
    fn test_shielded_precompile_deposit_rejects_empty_proof() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000));

        let mut precompile = ShieldedPrecompile;

        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 1_000,
            commitment: test_commitment(1).into(),
            proofData: vec![].into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "deposit with empty proof should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid ZK proof"), "expected InvalidZkProof, got: {err}");
    }

    /// Non-halo2-prover: deposit with dummy proof succeeds.
    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_deposit_and_get() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000));

        let mut precompile = ShieldedPrecompile;

        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 1_000,
            commitment: test_commitment(1).into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "deposit failed: {:?}", result.err());

        let input = IProtocolShielded::getCommitmentCountCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let count = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&result.bytes[24..32]);
            buf
        });
        assert_eq!(count, 1);

        let input = IProtocolShielded::getCommitmentCall { index: 0 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(&result.bytes[..], &test_commitment(1));

        let input = IProtocolShielded::isNullifierSpentCall {
            nullifier: test_commitment(1).into(),
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 0);
    }

    /// Empty proof must be rejected regardless of feature flags.
    #[test]
    fn test_shielded_precompile_withdraw_rejects_empty_proof() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x55);
        let target = Address::repeat_byte(0x66);

        let mut precompile = ShieldedPrecompile;

        let input = IProtocolShielded::withdrawCall {
            assetId: 0,
            target,
            amount: 500,
            nullifier: [0xBBu8; 32].into(),
            merkleRoot: [0u8; 32].into(),
            proofData: vec![].into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "withdraw with empty proof should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid ZK proof"), "expected InvalidZkProof, got: {err}");
    }

    /// Withdraw to zero address must be rejected.
    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_withdraw_rejects_zero_target() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let input = IProtocolShielded::withdrawCall {
            assetId: 1,
            target: Address::ZERO,
            amount: 500,
            nullifier: [0xBBu8; 32].into(),
            merkleRoot: [0u8; 32].into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "withdraw to zero address should fail");
    }

    /// Withdraw precompile test with real Halo2 proof verification.
    /// Only runs when `halo2-prover` feature is enabled.
    #[cfg(feature = "halo2-prover")]
    #[test]
    fn test_shielded_precompile_withdraw() {
        use crate::prover::{setup_withdraw_circuit, Halo2Prover};

        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x55);
        let circuit = setup_withdraw_circuit();
        let target: Address = circuit.target_address.into();

        // Set the merkle root in storage so the precompile merkle check passes
        provider.set(
            SHIELDED_ADDRESS,
            slot_shielded_merkle_root(),
            U256::from_be_slice(&circuit.merkle_root),
        );

        let prover = Halo2Prover::setup();
        let proof_data = prover.prove_withdraw(&circuit).expect("prove failed");

        let mut precompile = ShieldedPrecompile;

        let input = IProtocolShielded::withdrawCall {
            assetId: circuit.asset_id,
            target,
            amount: circuit.value,
            nullifier: circuit.nullifier.into(),
            merkleRoot: circuit.merkle_root.into(),
            proofData: proof_data.into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "withdraw failed: {:?}", result.err());

        let input = IProtocolShielded::isNullifierSpentCall {
            nullifier: circuit.nullifier.into(),
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 1);

        let target_slot = slot_balance(circuit.asset_id, target);
        let bal = provider
            .get(ASSET_ADDRESS, target_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(bal, circuit.value);
    }

    /// Empty proof must be rejected regardless of feature flags.
    #[test]
    fn test_shielded_precompile_transfer_rejects_empty_proof() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        let mut precompile = ShieldedPrecompile;

        let nullifiers = vec![[0xCCu8; 32].into()];
        let commitments = vec![test_commitment(3).into(), test_commitment(4).into()];

        let input = IProtocolShielded::transferCall {
            assetId: 1,
            proof: vec![].into(),
            nullifiers,
            commitments,
            merkleRoot: [0u8; 32].into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "transfer with empty proof should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid ZK proof"), "expected InvalidZkProof, got: {err}");
    }

    /// Transfer with empty nullifiers must be rejected.
    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_transfer_rejects_empty_nullifiers() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let nullifiers: Vec<alloy_sol_types::sol_data::FixedBytes<32>> = vec![];
        let commitments = vec![test_commitment(3).into(), test_commitment(4).into()];

        let input = IProtocolShielded::transferCall {
            assetId: 1,
            proof: vec![1u8; 200].into(),
            nullifiers,
            commitments,
            merkleRoot: [0u8; 32].into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "transfer with empty nullifiers should fail");
    }

    /// Transfer with empty commitments must be rejected.
    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_transfer_rejects_empty_commitments() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let nullifiers = vec![[0xCCu8; 32].into()];
        let commitments: Vec<alloy_sol_types::sol_data::FixedBytes<32>> = vec![];

        let input = IProtocolShielded::transferCall {
            assetId: 1,
            proof: vec![1u8; 200].into(),
            nullifiers,
            commitments,
            merkleRoot: [0u8; 32].into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "transfer with empty commitments should fail");
    }

    /// Transfer with duplicate nullifiers in the same batch must be rejected.
    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_transfer_rejects_duplicate_nullifiers() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let dup_nf = [0xCCu8; 32];
        let nullifiers = vec![dup_nf.into(), dup_nf.into()];
        let commitments = vec![test_commitment(3).into(), test_commitment(4).into()];

        let input = IProtocolShielded::transferCall {
            assetId: 1,
            proof: vec![1u8; 200].into(),
            nullifiers,
            commitments,
            merkleRoot: [0u8; 32].into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(
            result.is_err(),
            "transfer with duplicate nullifiers should fail"
        );
    }

    /// Transfer with invalid merkleRoot must be rejected.
    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_transfer_rejects_invalid_merkle_root() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let nullifiers = vec![[0xCCu8; 32].into()];
        let commitments = vec![test_commitment(3).into(), test_commitment(4).into()];

        let input = IProtocolShielded::transferCall {
            assetId: 1,
            proof: vec![1u8; 200].into(),
            nullifiers,
            commitments,
            merkleRoot: [0xFFu8; 32].into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "transfer with invalid merkleRoot should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("merkle root mismatch"),
            "expected MerkleRootMismatch, got: {err}"
        );
    }

    /// Transfer precompile test with real Halo2 proof verification.
    #[cfg(feature = "halo2-prover")]
    #[test]
    fn test_shielded_precompile_transfer() {
        use crate::prover::{setup_transfer_circuit, Halo2Prover};

        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        let circuit = setup_transfer_circuit();
        let prover = Halo2Prover::setup();
        let proof_data = prover.prove_transfer(&circuit).expect("prove failed");

        provider.set(
            SHIELDED_ADDRESS,
            slot_shielded_merkle_root(),
            U256::from_be_slice(&circuit.merkle_root),
        );

        let mut precompile = ShieldedPrecompile;

        let nullifiers = circuit.nullifiers.iter().map(|n| (*n).into()).collect();
        let commitments = circuit.commitments.iter().map(|c| (*c).into()).collect();

        let input = IProtocolShielded::transferCall {
            assetId: circuit.asset_id,
            proof: proof_data.into(),
            nullifiers,
            commitments,
            merkleRoot: circuit.merkle_root.into(),
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "transfer failed: {:?}", result.err());

        let input = IProtocolShielded::getCommitmentCountCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let count = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&result.bytes[24..32]);
            buf
        });
        assert_eq!(count, 2);

        let input = IProtocolShielded::isNullifierSpentCall {
            nullifier: circuit.nullifiers[0].into(),
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 1);
    }

    fn test_commitment(n: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0] = n;
        out
    }

    #[test]
    fn test_shielded_precompile_pause() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let governance = Address::repeat_byte(0xAA);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        // Set governance first
        let input = IProtocolShielded::setGovernanceCall {
            governance,
        }
        .abi_encode();
        // First call succeeds because governance is unset (defaults to zero address)
        let result = precompile.call(&input, governance, &mut provider);
        assert!(result.is_ok(), "setGovernance failed: {:?}", result.err());

        // Non-governance cannot pause
        let input = IProtocolShielded::setPausedCall { paused: true }.abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "non-governance should not be able to pause");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unauthorized"), "expected Unauthorized, got: {err}");

        // Governance can pause
        let input = IProtocolShielded::setPausedCall { paused: true }.abi_encode();
        let result = precompile.call(&input, governance, &mut provider);
        assert!(result.is_ok(), "setPaused failed: {:?}", result.err());

        // Verify paused
        let input = IProtocolShielded::isPausedCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        assert_eq!(result.bytes[31], 1, "expected paused");

        // Deposit should fail when paused
        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000));
        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 1_000,
            commitment: test_commitment(1).into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "deposit should fail when paused");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("paused"), "expected Paused error, got: {err}");
    }

    /// setPaused must fail when governance has not been set (zero address).
    #[test]
    fn test_shielded_precompile_set_paused_rejects_zero_governance() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let attacker = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        // No governance set — setPaused should fail
        let input = IProtocolShielded::setPausedCall { paused: true }.abi_encode();
        let result = precompile.call(&input, attacker, &mut provider);
        assert!(
            result.is_err(),
            "setPaused should fail when governance is zero"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unauthorized"), "expected Unauthorized, got: {err}");
    }

    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_duplicate_commitment_rejected() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000));

        let commitment = test_commitment(1);
        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 1_000,
            commitment: commitment.into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        // Second deposit with same commitment should fail
        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 2_000,
            commitment: commitment.into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "duplicate commitment should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("already exists"),
            "expected CommitmentAlreadyExists, got: {err}"
        );
    }

    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_rate_limit() {
        let mut provider = HashMapStorageProvider::new(50_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(1_000_000));

        // Deposit MAX_OPS_PER_ADDRESS_PER_BLOCK times
        for i in 0..MAX_OPS_PER_ADDRESS_PER_BLOCK {
            let mut commitment = [0u8; 32];
            commitment[0] = i as u8;
            let input = IProtocolShielded::depositCall {
                assetId: 1,
                amount: 1,
                commitment: commitment.into(),
                proofData: vec![1u8; 200].into(),
                keyVersion: 0,
            }
            .abi_encode();
            let result = precompile.call(&input, sender, &mut provider);
            assert!(result.is_ok(), "deposit {i} failed: {:?}", result.err());
        }

        // Next deposit should exceed rate limit
        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 1,
            commitment: test_commitment(255).into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "rate limit should be exceeded");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("rate limit"),
            "expected RateLimitExceeded, got: {err}"
        );
    }

    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_zero_amount_rejected() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000));

        // Zero-amount deposit should fail
        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 0,
            commitment: test_commitment(1).into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "zero amount deposit should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("amount must be greater than zero"),
            "expected InvalidAmount, got: {err}"
        );
    }

    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_proof_size_limit() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000));

        // Deposit with oversized proof (>20_000 bytes) should fail
        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 1_000,
            commitment: test_commitment(1).into(),
            proofData: vec![1u8; 20_001].into(),
            keyVersion: 0,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "oversized proof should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid ZK proof"),
            "expected InvalidZkProof, got: {err}"
        );
    }

    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_prune_rate_limit() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        provider.set_block_number(1);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        // First prune should succeed
        let input = IProtocolShielded::pruneNullifiersCall {}.abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "first prune failed: {:?}", result.err());

        // Second prune at same block should fail (too frequent)
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "second prune should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("too frequently"),
            "expected PruneTooFrequent, got: {err}"
        );

        // Advance beyond interval — should succeed
        provider.set_block_number(1 + MIN_PRUNE_INTERVAL_BLOCKS);
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "prune after interval failed: {:?}", result.err());
    }

    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_root_history() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);
        let mut precompile = ShieldedPrecompile;

        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000));

        // First deposit
        let commitment1 = test_commitment(1);
        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 1_000,
            commitment: commitment1.into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        // Get root after first deposit
        let input = IProtocolShielded::getMerkleRootCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        let root1: [u8; 32] = result.bytes[..32].try_into().unwrap();

        // Zero root is not recorded, so count is 0 after first deposit
        let input = IProtocolShielded::getRootHistoryCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        let count = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&result.bytes[24..32]);
            buf
        });
        assert_eq!(count, 0, "zero root not recorded");

        // Second deposit
        let commitment2 = test_commitment(2);
        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 2_000,
            commitment: commitment2.into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        // Verify root changed
        let input = IProtocolShielded::getMerkleRootCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        let root2: [u8; 32] = result.bytes[..32].try_into().unwrap();
        assert_ne!(root1, root2, "root should change after second deposit");

        // Now root1 should be in history
        let input = IProtocolShielded::getRootHistoryCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        let count = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&result.bytes[24..32]);
            buf
        });
        assert_eq!(count, 1, "root1 should be recorded");

        let input = IProtocolShielded::getRootHistoryCall { index: 0 }.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        let hist_root1: [u8; 32] = result.bytes[..32].try_into().unwrap();
        assert_eq!(hist_root1, root1, "root1 should be in history");
    }

    /// Non-halo2-prover: deposit with dummy proof updates merkle root.
    #[cfg(not(feature = "halo2-prover"))]
    #[test]
    fn test_shielded_precompile_deposit_updates_merkle_root() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000));

        let mut precompile = ShieldedPrecompile;
        let commitment = test_commitment(1);

        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 1_000,
            commitment: commitment.into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        let input = IProtocolShielded::getMerkleRootCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let root1: [u8; 32] = result.bytes[..32].try_into().unwrap();
        assert_ne!(
            root1, [0u8; 32],
            "merkle root should not be zero after deposit"
        );

        let mut tree = crate::PoseidonMerkleTree::new(32);
        let tree_root = tree.insert(&commitment);
        assert_eq!(
            root1, tree_root,
            "merkle root should match local computation"
        );

        let commitment2 = test_commitment(2);
        let input = IProtocolShielded::depositCall {
            assetId: 1,
            amount: 2_000,
            commitment: commitment2.into(),
            proofData: vec![1u8; 200].into(),
            keyVersion: 0,
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        let input = IProtocolShielded::getMerkleRootCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let root2: [u8; 32] = result.bytes[..32].try_into().unwrap();
        assert_ne!(
            root1, root2,
            "merkle root should change after second deposit"
        );

        let mut tree = crate::PoseidonMerkleTree::new(32);
        tree.insert(&commitment);
        tree.insert(&commitment2);
        let expected_root2 = tree.root();
        assert_eq!(root2, expected_root2);
    }

    /// Transfer merkle-root update test with real Halo2 proof verification.
    #[cfg(feature = "halo2-prover")]
    #[test]
    fn test_shielded_precompile_transfer_updates_merkle_root() {
        use crate::prover::{setup_transfer_circuit, Halo2Prover};

        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        let circuit = setup_transfer_circuit();
        let prover = Halo2Prover::setup();
        let proof_data = prover.prove_transfer(&circuit).expect("prove failed");

        provider.set(
            SHIELDED_ADDRESS,
            slot_shielded_merkle_root(),
            U256::from_be_slice(&circuit.merkle_root),
        );

        let mut precompile = ShieldedPrecompile;

        let nullifiers = circuit.nullifiers.iter().map(|n| (*n).into()).collect();
        let commitments = circuit.commitments.iter().map(|c| (*c).into()).collect();

        let input = IProtocolShielded::transferCall {
            assetId: circuit.asset_id,
            proof: proof_data.into(),
            nullifiers,
            commitments,
            merkleRoot: circuit.merkle_root.into(),
            keyVersion: 0,
        }
        .abi_encode();

        precompile.call(&input, sender, &mut provider).unwrap();

        let input = IProtocolShielded::getMerkleRootCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let root: [u8; 32] = result.bytes[..32].try_into().unwrap();
        assert_ne!(
            root, [0u8; 32],
            "merkle root should not be zero after transfer"
        );

        let mut tree = crate::PoseidonMerkleTree::new(32);
        for cm in &circuit.commitments {
            tree.insert(cm);
        }
        assert_eq!(root, tree.root());
    }

    /// Halo2-prover: deposit with real proof updates merkle root.
    #[cfg(feature = "halo2-prover")]
    #[test]
    fn test_shielded_precompile_deposit_with_real_proof() {
        use crate::prover::{setup_deposit_circuit, Halo2Prover};

        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        let sender_slot = slot_balance(1, sender);
        provider.set(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000));

        let circuit = setup_deposit_circuit();
        let prover = Halo2Prover::setup();
        let proof_data = prover.prove_deposit(&circuit).expect("prove failed");

        let mut precompile = ShieldedPrecompile;

        let input = IProtocolShielded::depositCall {
            assetId: circuit.asset_id,
            amount: circuit.witness.as_ref().unwrap().value,
            commitment: circuit.commitment.into(),
            proofData: proof_data.into(),
            keyVersion: 0,
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        let input = IProtocolShielded::getMerkleRootCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let root: [u8; 32] = result.bytes[..32].try_into().unwrap();
        assert_ne!(
            root, [0u8; 32],
            "merkle root should not be zero after deposit"
        );

        let mut tree = crate::PoseidonMerkleTree::new(32);
        tree.insert(&circuit.commitment);
        assert_eq!(root, tree.root());
    }

    /// Nullifier must remain marked as spent even after the expiry window.
    /// This prevents natural double-spend without any attacker action.
    #[test]
    fn test_nullifier_spent_permanent_after_expiry() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sr = StorageRef::new(&mut provider);
        let mut store = ShieldedStorage::new(sr);

        let nullifier = [0xABu8; 32];
        let spent_at = 1_000u64;
        store.mark_nullifier_spent(nullifier, spent_at);

        // Within window — still spent
        assert!(
            store.is_nullifier_spent(nullifier, spent_at + NOTE_WINDOW_BLOCKS - 1),
            "nullifier should be spent within window"
        );

        // Just past window — MUST still be spent (the critical fix)
        assert!(
            store.is_nullifier_spent(nullifier, spent_at + NOTE_WINDOW_BLOCKS + 1),
            "nullifier must remain spent after window expiry"
        );

        // Far future — permanently spent
        assert!(
            store.is_nullifier_spent(nullifier, spent_at + NOTE_WINDOW_BLOCKS * 10),
            "nullifier must be permanently spent"
        );
    }

    /// Prune should only clear expired nullifiers, not unexpired ones.
    /// is_nullifier_expired must correctly identify expired entries.
    #[test]
    fn test_prune_respects_expiry_and_is_expired_works() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sr = StorageRef::new(&mut provider);
        let mut store = ShieldedStorage::new(sr);

        let nf_expired = [0xCDu8; 32];
        let nf_fresh = [0xDEu8; 32];
        let spent_at = 100u64;
        store.mark_nullifier_spent(nf_expired, spent_at);
        store.mark_nullifier_spent(nf_fresh, spent_at + NOTE_WINDOW_BLOCKS + 50);

        // Both are spent
        assert!(store.check_nullifier_spent(nf_expired));
        assert!(store.check_nullifier_spent(nf_fresh));

        // Only the first is expired relative to prune_block
        let prune_block = spent_at + NOTE_WINDOW_BLOCKS + 10;
        assert!(
            store.is_nullifier_expired(nf_expired, prune_block),
            "old nullifier should be expired"
        );
        assert!(
            !store.is_nullifier_expired(nf_fresh, prune_block),
            "fresh nullifier should not be expired"
        );

        // Prune should only remove the expired one
        let pruned = store.prune_expired_nullifiers(prune_block);
        assert_eq!(pruned, 1, "only expired nullifier should be pruned");

        // Expired one is cleared
        assert!(!store.check_nullifier_spent(nf_expired));
        // Fresh one remains
        assert!(store.check_nullifier_spent(nf_fresh));
    }
}
