//! Shielded precompile entry point (0x202).
//!
//! Thin wrapper that routes EVM calls to [`ShieldedStorage`] backed by
//! EVM storage. Business logic lives in [`ShieldedStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use call_precompile::storage::StorageProvider;
use call_precompile::{
    check_compliance, dispatch, slot_balance, storage::storage_slot, u128_to_u256, u256_to_u128,
    u256_to_u64, u64_to_u256, StorageRef, ASSET_ADDRESS,
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

/// Number of blocks a note remains spendable after insertion (~1 week at 6s/block).
pub const NOTE_WINDOW_BLOCKS: u64 = 100_800;

/// Number of BitSet buckets for nullifier compression (4096 * 256 bits = 1,048,576 bits).
const NULLIFIER_BITSET_BUCKETS: u64 = 4096;

/// Maximum number of commitments before the Merkle tree refuses new insertions.
/// At depth 32 this is ~4B, but we cap far lower to keep EVM storage growth bounded.
/// When reached, users must spend old notes (withdraw/transfer) before new deposits.
const MAX_COMMITMENTS: u64 = 1_000_000;

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

    /// Check if a nullifier has been spent **and** is within the expiry window.
    /// Returns false for expired nullifiers (allowing prune).
    fn check_nullifier_spent(&mut self, nullifier: [u8; 32], current_block: u64) -> bool {
        let flag = self
            .sload_shielded(slot_shielded_nullifier(nullifier))
            .to_be_bytes::<32>()[31]
            == 1;
        if !flag {
            return false;
        }
        let spent_at = u256_to_u64(self.sload_shielded(slot_shielded_nullifier_height(nullifier)));
        // Legacy entries have no height recorded — treat as valid spent
        if spent_at == 0 {
            return true;
        }
        let window = self.load_window_blocks();
        current_block.saturating_sub(spent_at) < window
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
        for i in 0..count {
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
        caller: Address,
        current_block: u64,
    ) -> Result<(), ShieldedError> {
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
        current_block: u64,
    ) -> Result<(), ShieldedError> {
        let stored_root = self
            .sload_shielded(slot_shielded_merkle_root())
            .to_be_bytes::<32>();
        if merkle_root != stored_root {
            return Err(ShieldedError::MerkleRootMismatch);
        }

        if proof_data.is_empty() {
            return Err(ShieldedError::InvalidZkProof);
        }
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

        if self.check_nullifier_spent(nullifier, current_block) {
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
        key_version: u32,
        current_block: u64,
    ) -> Result<(), ShieldedError> {
        if nullifiers.is_empty() && commitments.is_empty() {
            return Err(ShieldedError::EmptyBatch);
        }

        if proof_data.is_empty() {
            return Err(ShieldedError::InvalidZkProof);
        }

        let merkle_root = self
            .sload_shielded(slot_shielded_merkle_root())
            .to_be_bytes::<32>();

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

        for nf in &nullifiers {
            if self.check_nullifier_spent(*nf, current_block) {
                return Err(ShieldedError::NullifierAlreadySpent);
            }
        }

        for nf in &nullifiers {
            self.mark_nullifier_spent(*nf, current_block);
        }

        for cm in &commitments {
            let count = u256_to_u64(self.sload_shielded(slot_shielded_commitment_count()));
            if count >= MAX_COMMITMENTS {
                return Err(ShieldedError::MerkleTreeFull);
            }
            self.insert_commitment(*cm, current_block);
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
        self.sload_shielded(slot_shielded_commitment(index))
            .to_be_bytes::<32>()
    }

    pub fn is_nullifier_spent(&mut self, nullifier: [u8; 32], current_block: u64) -> bool {
        self.check_nullifier_spent(nullifier, current_block)
    }

    pub fn get_commitment_height(&mut self, index: u64) -> u64 {
        u256_to_u64(self.sload_shielded(slot_shielded_commitment_height(index)))
    }
}

// ── ShieldedPrecompile ────────────────────────────────────────────────

sol! {
    interface IProtocolShielded {
        function deposit(uint64 assetId, uint128 amount, bytes32 commitment) external;
        function withdraw(uint64 assetId, address target, uint128 amount, bytes32 nullifier, bytes32 merkleRoot, bytes proofData, uint32 keyVersion) external;
        function transfer(uint64 assetId, bytes proof, bytes32[] nullifiers, bytes32[] commitments, uint32 keyVersion) external;
        function getMerkleRoot() external view returns (bytes32);
        function getCommitmentCount() external view returns (uint64);
        function getCommitment(uint64 index) external view returns (bytes32);
        function getCommitmentHeight(uint64 index) external view returns (uint64);
        function isNullifierSpent(bytes32 nullifier) external view returns (bool);
        function pruneNullifiers() external returns (uint64);
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
        dispatch::mutate_void::<IProtocolShielded::depositCall, _>(
            calldata,
            50000,
            storage,
            |call, storage| {
                check_compliance(msg_sender, storage)?;
                let current_block = storage.block_number();
                let mut store = ShieldedStorage::new(sr);
                store
                    .deposit(
                        call.assetId,
                        call.amount,
                        call.commitment.into(),
                        msg_sender,
                        current_block,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn withdraw(
        &self,
        calldata: &[u8],
        _msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        let call = dispatch::decode_call::<IProtocolShielded::withdrawCall>(calldata)?;

        // Dynamic gas: base 30_000 + 10 per byte of proof data
        let proof_len = call.proofData.len() as u64;
        let dynamic_gas = 30_000u64
            .saturating_add(proof_len.saturating_mul(10));
        storage.deduct_gas(dynamic_gas)?;

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
                current_block,
            )
            .map_err(|e| PrecompileError::Other(e.to_string().into()));

        match result {
            Ok(()) => {
                storage.checkpoint_commit(cp);
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
        _msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        let call = dispatch::decode_call::<IProtocolShielded::transferCall>(calldata)?;

        // Dynamic gas: base 30_000 + 10 per byte of proof data
        let proof_len = call.proof.len() as u64;
        let dynamic_gas = 30_000u64
            .saturating_add(proof_len.saturating_mul(10));
        storage.deduct_gas(dynamic_gas)?;

        let current_block = storage.block_number();
        let cp = storage.checkpoint();
        let mut store = ShieldedStorage::new(sr);
        let nullifiers: Vec<[u8; 32]> =
            call.nullifiers.iter().map(|n| (*n).into()).collect();
        let commitments: Vec<[u8; 32]> =
            call.commitments.iter().map(|c| (*c).into()).collect();
        let result = store
            .transfer(call.assetId, call.proof.to_vec(), nullifiers, commitments, call.keyVersion, current_block)
            .map_err(|e| PrecompileError::Other(e.to_string().into()));

        match result {
            Ok(()) => {
                storage.checkpoint_commit(cp);
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
            10000,
            storage,
            |_call, storage| {
                let current_block = storage.block_number();
                let mut store = ShieldedStorage::new(sr);
                let pruned = store.prune_expired_nullifiers(current_block);
                Ok(pruned)
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
            keyVersion: 0,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "transfer with empty proof should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid ZK proof"), "expected InvalidZkProof, got: {err}");
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
}
