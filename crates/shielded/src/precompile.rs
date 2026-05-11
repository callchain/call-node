//! Shielded precompile entry point (0x202).
//!
//! Thin wrapper that routes EVM calls to [`ShieldedStorage`] backed by
//! EVM storage. Business logic lives in [`ShieldedStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use alloy_primitives::{address, Address, U256};
use alloy_sol_types::{sol, SolCall};
use call_precompile::storage::StorageProvider;
use call_precompile::{
    dispatch, slot_balance, storage::storage_slot, u128_to_u256, u256_to_u128, u256_to_u64,
    u64_to_u256, StorageRef, ASSET_ADDRESS,
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

    fn check_nullifier_spent(&mut self, nullifier: [u8; 32]) -> bool {
        self.sload_shielded(slot_shielded_nullifier(nullifier))
            .to_be_bytes::<32>()[31]
            == 1
    }

    fn insert_commitment(&mut self, commitment: [u8; 32]) -> [u8; 32] {
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
        current
    }

    pub fn deposit(
        &mut self,
        asset_id: u64,
        amount: u128,
        commitment: [u8; 32],
        caller: Address,
    ) -> Result<(), ShieldedError> {
        let sender_bal = self
            .load_bal(asset_id, caller)
            .checked_sub(amount)
            .ok_or(ShieldedError::InsufficientBalance)?;
        self.save_bal(asset_id, caller, sender_bal);

        self.insert_commitment(commitment);

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
    ) -> Result<(), ShieldedError> {
        let stored_root = self
            .sload_shielded(slot_shielded_merkle_root())
            .to_be_bytes::<32>();
        if merkle_root != stored_root {
            return Err(ShieldedError::MerkleRootMismatch);
        }

        if !proof_data.is_empty() {
            let proof = crate::ZkProof {
                proof_data,
                nullifiers: vec![crate::Nullifier::new(Hash::from_slice(&nullifier))],
                commitments: vec![],
                asset_id,
                key_version: 0,
            };

            match crate::verify_shielded_proof(&proof, "withdraw", Some(&merkle_root), Some(amount))
            {
                Ok(true) => {}
                Ok(false) => return Err(ShieldedError::InvalidZkProof),
                Err(e) => {
                    if e.contains("real-prover") {
                        if !crate::verify_zk_proof(&proof) {
                            return Err(ShieldedError::InvalidZkProof);
                        }
                    } else {
                        return Err(ShieldedError::ZkProofError(e));
                    }
                }
            }
        }

        if self.check_nullifier_spent(nullifier) {
            return Err(ShieldedError::NullifierAlreadySpent);
        }

        self.sstore_shielded(slot_shielded_nullifier(nullifier), U256::from(1u8));

        let target_bal = self
            .load_bal(asset_id, target)
            .checked_add(amount)
            .ok_or(ShieldedError::BalanceOverflow)?;
        self.save_bal(asset_id, target, target_bal);

        Ok(())
    }

    pub fn transfer(
        &mut self,
        asset_id: u64,
        proof_data: Vec<u8>,
        nullifiers: Vec<[u8; 32]>,
        commitments: Vec<[u8; 32]>,
    ) -> Result<(), ShieldedError> {
        if nullifiers.is_empty() && commitments.is_empty() {
            return Err(ShieldedError::EmptyBatch);
        }

        if !proof_data.is_empty() {
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
                key_version: 0,
            };

            match crate::verify_shielded_proof(&proof, "transfer", Some(&merkle_root), None) {
                Ok(true) => {}
                Ok(false) => return Err(ShieldedError::InvalidZkProof),
                Err(e) => {
                    if e.contains("real-prover") {
                        if !crate::verify_zk_proof(&proof) {
                            return Err(ShieldedError::InvalidZkProof);
                        }
                    } else {
                        return Err(ShieldedError::ZkProofError(e));
                    }
                }
            }
        }

        for nf in &nullifiers {
            if self.check_nullifier_spent(*nf) {
                return Err(ShieldedError::NullifierAlreadySpent);
            }
        }

        for nf in &nullifiers {
            self.sstore_shielded(slot_shielded_nullifier(*nf), U256::from(1u8));
        }

        for cm in &commitments {
            self.insert_commitment(*cm);

            let count = u256_to_u64(self.sload_shielded(slot_shielded_commitment_count()));
            self.sstore_shielded(slot_shielded_commitment(count), U256::from_be_slice(cm));
            self.sstore_shielded(slot_shielded_commitment_count(), u64_to_u256(count + 1));
        }

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

    pub fn is_nullifier_spent(&mut self, nullifier: [u8; 32]) -> bool {
        self.check_nullifier_spent(nullifier)
    }
}

// ── ShieldedPrecompile ────────────────────────────────────────────────

sol! {
    interface IProtocolShielded {
        function deposit(uint64 assetId, uint128 amount, bytes32 commitment) external;
        function withdraw(uint64 assetId, address target, uint128 amount, bytes32 nullifier, bytes32 merkleRoot, bytes proofData) external;
        function transfer(uint64 assetId, bytes proof, bytes32[] nullifiers, bytes32[] commitments) external;
        function getMerkleRoot() external view returns (bytes32);
        function getCommitmentCount() external view returns (uint64);
        function getCommitment(uint64 index) external view returns (bytes32);
        function isNullifierSpent(bytes32 nullifier) external view returns (bool);
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
            |call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                store
                    .deposit(
                        call.assetId,
                        call.amount,
                        call.commitment.into(),
                        msg_sender,
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
        dispatch::mutate_void::<IProtocolShielded::withdrawCall, _>(
            calldata,
            50000,
            storage,
            |call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                store
                    .withdraw(
                        call.assetId,
                        call.target,
                        call.amount,
                        call.nullifier.into(),
                        call.merkleRoot.into(),
                        call.proofData.to_vec(),
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn transfer(
        &self,
        calldata: &[u8],
        _msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolShielded::transferCall, _>(
            calldata,
            50000,
            storage,
            |call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                let nullifiers: Vec<[u8; 32]> =
                    call.nullifiers.iter().map(|n| (*n).into()).collect();
                let commitments: Vec<[u8; 32]> =
                    call.commitments.iter().map(|c| (*c).into()).collect();
                store
                    .transfer(call.assetId, call.proof.to_vec(), nullifiers, commitments)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
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
            |call, _storage| {
                let mut store = ShieldedStorage::new(sr);
                Ok(store.is_nullifier_spent(call.nullifier.into()))
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
        let selector: [u8; 4] = calldata[..4].try_into().unwrap();
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolShielded::depositCall::SELECTOR => self.deposit(calldata, msg_sender, storage, sr),
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
            IProtocolShielded::isNullifierSpentCall::SELECTOR => {
                self.is_nullifier_spent(calldata, storage, sr)
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

    /// Withdraw precompile test without real ZK verification (default / non-real-prover).
    /// Skips ZK proof verification and tests core precompile logic directly.
    #[cfg(not(feature = "real-prover"))]
    #[test]
    fn test_shielded_precompile_withdraw() {
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
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "withdraw failed: {:?}", result.err());

        let input = IProtocolShielded::isNullifierSpentCall {
            nullifier: [0xBBu8; 32].into(),
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 1);

        let target_slot = slot_balance(0, target);
        let bal = provider
            .get(ASSET_ADDRESS, target_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(bal, 500);
    }

    /// Withdraw precompile test with real Groth16 proof verification.
    /// Only runs when `real-prover` feature is enabled (e.g. workspace build).
    #[cfg(feature = "real-prover")]
    #[test]
    fn test_shielded_precompile_withdraw() {
        use crate::prover::{setup_withdraw_circuit, RealProver};

        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x55);
        let target = Address::repeat_byte(0x66);

        let circuit = setup_withdraw_circuit();

        // Set the merkle root in storage so the precompile merkle check passes
        provider.set(
            SHIELDED_ADDRESS,
            slot_shielded_merkle_root(),
            U256::from_be_slice(&circuit.merkle_root),
        );

        let prover = RealProver::global();
        let proof_data = prover.prove_withdraw(&circuit).expect("prove failed");

        let mut precompile = ShieldedPrecompile;

        let input = IProtocolShielded::withdrawCall {
            assetId: circuit.asset_id,
            target,
            amount: circuit.value,
            nullifier: circuit.nullifier.into(),
            merkleRoot: circuit.merkle_root.into(),
            proofData: proof_data.into(),
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

    #[test]
    fn test_shielded_precompile_transfer() {
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
            nullifier: [0xCCu8; 32].into(),
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

    #[test]
    fn test_shielded_precompile_transfer_updates_merkle_root() {
        let mut provider = HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        let mut precompile = ShieldedPrecompile;

        let nullifiers = vec![[0xCCu8; 32].into()];
        let commitments = vec![test_commitment(3).into()];

        let input = IProtocolShielded::transferCall {
            assetId: 1,
            proof: vec![].into(),
            nullifiers,
            commitments,
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
        tree.insert(&test_commitment(3));
        assert_eq!(root, tree.root());
    }
}
