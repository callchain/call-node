//! Shielded precompile at 0x202
//!
//! Functions: deposit, withdraw, transfer, getMerkleRoot, getCommitmentCount,
//!            getCommitment, isNullifierSpent
//!
//! Write functions update EVM storage directly, including incremental Merkle
//! tree updates. Deposit and transfer automatically recompute the Poseidon
//! Merkle root (depth=32) from EVM storage so the root is always current.
//! Withdraw and transfer optionally verify ZK proofs when a merkleRoot + proof
//! are provided in the calldata.

use alloy_primitives::{address, Address, U256};
use revm_precompile::PrecompileError;

use crate::{
    decode_address, decode_bytes32, decode_bytes32_array, decode_u128, decode_u64,
    decode_u256_usize, encode_u64, encode_u8, ok_empty, slot_balance, u128_to_u256,
    u256_to_u128, u256_to_u64, u64_to_u256, StatefulPrecompile,
};
use crate::storage::{storage_slot, StorageCtx};

pub const SHIELDED_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000202");

// ── Storage slot helpers ──────────────────────────────────────────────

fn slot_shielded_merkle_root() -> U256 {
    storage_slot(&[b"merkle_root"])
}

fn slot_shielded_nullifier(nullifier: [u8; 32]) -> U256 {
    storage_slot(&[b"nullifier", &nullifier])
}

fn slot_shielded_commitment_count() -> U256 {
    storage_slot(&[b"cm_count"])
}

fn slot_shielded_commitment(index: u64) -> U256 {
    storage_slot(&[b"commitment", &index.to_be_bytes()[..]])
}

fn slot_tree_node(level: u8, index: u64) -> U256 {
    storage_slot(&[b"tree", &[level], &index.to_be_bytes()])
}

fn is_nullifier_spent(nullifier: [u8; 32]) -> bool {
    StorageCtx::sload(SHIELDED_ADDRESS, slot_shielded_nullifier(nullifier))
        .map(|v| v.to_be_bytes::<32>()[31] == 1)
        .unwrap_or(false)
}

// ── Balance helpers ───────────────────────────────────────────────────

fn load_bal(asset_id: u64, addr: Address) -> u128 {
    StorageCtx::sload(crate::ASSET_ADDRESS, slot_balance(asset_id, addr))
        .map(u256_to_u128)
        .unwrap_or(0)
}

fn save_bal(asset_id: u64, addr: Address, amount: u128) {
    StorageCtx::sstore(crate::ASSET_ADDRESS, slot_balance(asset_id, addr), u128_to_u256(amount));
}

// ── Incremental Merkle Tree (Poseidon, depth=32) ──────────────────────

thread_local! {
    static EMPTY_HASH_CACHE: std::cell::RefCell<Vec<[u8; 32]>> =
        std::cell::RefCell::new(vec![[0u8; 32]]);
}

/// Compute the Poseidon empty hash for a given Merkle tree level.
/// Level 0 = [0;32], Level N = hash(empty(N-1), empty(N-1)).
fn empty_hash(level: usize) -> [u8; 32] {
    EMPTY_HASH_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        while cache.len() <= level {
            let last = cache[cache.len() - 1];
            cache.push(call_shielded::poseidon::poseidon_hash_pair(&last, &last));
        }
        cache[level]
    })
}

/// Insert a commitment into the incremental Poseidon Merkle tree stored in
/// EVM storage. Updates the tree node slots along the insertion path and
/// writes the new root to `slot_shielded_merkle_root`.
///
/// Returns the new Merkle root.
fn insert_commitment(commitment: [u8; 32]) -> [u8; 32] {
    let count = StorageCtx::sload(SHIELDED_ADDRESS, slot_shielded_commitment_count())
        .map(u256_to_u64)
        .unwrap_or(0);

    let mut idx = count as usize;
    let mut current = commitment;
    const DEPTH: usize = 32;

    for level in 0..DEPTH {
        // Write the current node to its tree slot
        StorageCtx::sstore(
            SHIELDED_ADDRESS,
            slot_tree_node(level as u8, idx as u64),
            U256::from_be_slice(&current),
        );

        let is_left = idx & 1 == 0;
        let parent = if is_left {
            let sibling = empty_hash(level);
            call_shielded::poseidon::poseidon_hash_pair(&current, &sibling)
        } else {
            let left = StorageCtx::sload(
                SHIELDED_ADDRESS,
                slot_tree_node(level as u8, (idx - 1) as u64),
            )
            .map(|v| v.to_be_bytes::<32>())
            .unwrap_or_else(|| empty_hash(level));
            call_shielded::poseidon::poseidon_hash_pair(&left, &current)
        };

        current = parent;
        idx >>= 1;
    }

    // Write the new root
    StorageCtx::sstore(
        SHIELDED_ADDRESS,
        slot_shielded_merkle_root(),
        U256::from_be_slice(&current),
    );

    current
}

// ── ShieldedPrecompile ────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct ShieldedPrecompile;

impl ShieldedPrecompile {
    // deposit(uint64 assetId, uint128 amount, bytes32 commitment) -> 0x168f44f5
    fn deposit(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 50000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 68 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let amount = decode_u128(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;
        let commitment = decode_bytes32(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid commitment".into()))?;

        // Deduct transparent balance
        let sender_bal = load_bal(asset_id, msg_sender)
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("shielded deposit: insufficient balance".into()))?;
        save_bal(asset_id, msg_sender, sender_bal);

        // Incremental Merkle tree update (A: auto-update root on deposit)
        // Must happen before count is incremented so the insertion index is correct.
        insert_commitment(commitment);

        // Store commitment
        let count = StorageCtx::sload(SHIELDED_ADDRESS, slot_shielded_commitment_count())
            .map(u256_to_u64)
            .unwrap_or(0);
        StorageCtx::sstore(
            SHIELDED_ADDRESS,
            slot_shielded_commitment(count),
            U256::from_be_slice(&commitment),
        );
        StorageCtx::sstore(
            SHIELDED_ADDRESS,
            slot_shielded_commitment_count(),
            u64_to_u256(count + 1),
        );

        ok_empty()
    }

    // withdraw(uint64 assetId, address target, uint128 amount, bytes32 nullifier,
    //           bytes32 merkleRoot, bytes proofData) -> 0x175231d5
    //
    // Backward-compatible: if calldata is the old 132-byte format, skip ZK proof
    // verification. If merkleRoot + proofData are provided, verify the Groth16 proof.
    fn withdraw(
        &self,
        input: &[u8],
        _msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 50000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let target = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid target".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;
        let nullifier = decode_bytes32(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid nullifier".into()))?;

        // Optional ZK proof verification (extended calldata)
        if input.len() >= 164 {
            let merkle_root = decode_bytes32(input, 132)
                .ok_or_else(|| PrecompileError::Other("invalid merkleRoot".into()))?;

            // Verify merkleRoot matches current storage root
            let stored_root = StorageCtx::sload(SHIELDED_ADDRESS, slot_shielded_merkle_root())
                .unwrap_or(U256::ZERO)
                .to_be_bytes::<32>();
            if merkle_root != stored_root {
                return Err(PrecompileError::Other(
                    "shielded withdraw: merkle root mismatch".into(),
                ));
            }

            // Verify ZK proof if proofData is provided
            if input.len() >= 196 {
                let proof_offset = decode_u256_usize(input, 164)
                    .ok_or_else(|| PrecompileError::Other("invalid proofOffset".into()))?;
                let abs_offset = 4 + proof_offset;
                if input.len() >= abs_offset + 32 {
                    let proof_len = decode_u256_usize(input, abs_offset)
                        .ok_or_else(|| PrecompileError::Other("invalid proofLen".into()))?;
                    let proof_start = abs_offset + 32;
                    if input.len() >= proof_start + proof_len {
                        let proof_data = input[proof_start..proof_start + proof_len].to_vec();

                        let proof = call_shielded::ZkProof {
                            proof_data,
                            nullifiers: vec![call_shielded::Nullifier::new(
                                call_primitives::Hash::from_slice(&nullifier),
                            )],
                            commitments: vec![],
                            asset_id,
                        };
                        match call_shielded::verify_shielded_proof(
                            &proof,
                            "withdraw",
                            Some(&merkle_root),
                            Some(amount),
                        ) {
                            Ok(true) => {}
                            Ok(false) => {
                                return Err(PrecompileError::Other(
                                    "shielded withdraw: invalid ZK proof".into(),
                                ));
                            }
                            Err(e) => {
                                return Err(PrecompileError::Other(
                                    format!("ZK verification error: {e}").into(),
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Check nullifier not spent
        if is_nullifier_spent(nullifier) {
            return Err(PrecompileError::Other(
                "shielded withdraw: nullifier already spent".into(),
            ));
        }

        // Mark nullifier spent
        StorageCtx::sstore(
            SHIELDED_ADDRESS,
            slot_shielded_nullifier(nullifier),
            U256::from(1u8),
        );

        // Credit transparent balance
        let target_bal = load_bal(asset_id, target)
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("shielded withdraw: balance overflow".into()))?;
        save_bal(asset_id, target, target_bal);

        ok_empty()
    }

    // transfer(uint64 assetId, bytes32[] nullifiers, bytes32[] commitments) -> 0x1c3b10f8
    fn transfer(
        &self,
        input: &[u8],
        _msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 100000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let _asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let nullifiers = decode_bytes32_array(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid nullifiers".into()))?;
        let commitments = decode_bytes32_array(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid commitments".into()))?;

        // Check all nullifiers not spent
        for nf in &nullifiers {
            if is_nullifier_spent(*nf) {
                return Err(PrecompileError::Other(
                    "shielded transfer: nullifier already spent".into(),
                ));
            }
        }

        // Mark nullifiers spent
        for nf in &nullifiers {
            StorageCtx::sstore(
                SHIELDED_ADDRESS,
                slot_shielded_nullifier(*nf),
                U256::from(1u8),
            );
        }

        // Store commitments and update Merkle tree for each
        for cm in &commitments {
            // Incremental Merkle tree update first so insertion index is correct
            insert_commitment(*cm);

            let count = StorageCtx::sload(SHIELDED_ADDRESS, slot_shielded_commitment_count())
                .map(u256_to_u64)
                .unwrap_or(0);
            StorageCtx::sstore(
                SHIELDED_ADDRESS,
                slot_shielded_commitment(count),
                U256::from_be_slice(cm),
            );
            StorageCtx::sstore(
                SHIELDED_ADDRESS,
                slot_shielded_commitment_count(),
                u64_to_u256(count + 1),
            );
        }

        ok_empty()
    }

    // getMerkleRoot() -> bytes32 -> 0xe0c7497f
    fn get_merkle_root(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let root = StorageCtx::sload(SHIELDED_ADDRESS, slot_shielded_merkle_root())
            .unwrap_or(U256::ZERO);

        let out = revm_precompile::PrecompileOutput::new(0, alloy_primitives::Bytes::from(root.to_be_bytes::<32>().to_vec()));
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getCommitmentCount() -> uint64 -> 0x11985ba9
    fn get_commitment_count(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let count = StorageCtx::sload(SHIELDED_ADDRESS, slot_shielded_commitment_count())
            .map(u256_to_u64)
            .unwrap_or(0);

        let out = revm_precompile::PrecompileOutput::new(0, encode_u64(count).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getCommitment(uint64 index) -> bytes32 -> 0x2382d4c4
    fn get_commitment(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 2000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let index = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid index".into()))?;

        let cm = StorageCtx::sload(SHIELDED_ADDRESS, slot_shielded_commitment(index))
            .unwrap_or(U256::ZERO);

        let out = revm_precompile::PrecompileOutput::new(0, alloy_primitives::Bytes::from(cm.to_be_bytes::<32>().to_vec()));
        Ok(crate::storage::fill_precompile_output(out))
    }

    // isNullifierSpent(bytes32 nullifier) -> bool -> 0x371dff59
    fn is_nullifier_spent_call(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 2000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let nullifier = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid nullifier".into()))?;

        let spent = is_nullifier_spent(nullifier);

        let out = revm_precompile::PrecompileOutput::new(0, encode_u8(if spent { 1 } else { 0 }).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }
}

impl StatefulPrecompile for ShieldedPrecompile {
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("too short".into()));
        }
        let selector = [calldata[0], calldata[1], calldata[2], calldata[3]];
        match selector {
            [0x16, 0x8f, 0x44, 0xf5] => self.deposit(calldata, msg_sender),
            [0x17, 0x52, 0x31, 0xd5] => self.withdraw(calldata, msg_sender),
            [0x1c, 0x3b, 0x10, 0xf8] => self.transfer(calldata, msg_sender),
            [0xe0, 0xc7, 0x49, 0x7f] => self.get_merkle_root(calldata),
            [0x11, 0x98, 0x5b, 0xa9] => self.get_commitment_count(calldata),
            [0x23, 0x82, 0xd4, 0xc4] => self.get_commitment(calldata),
            [0x37, 0x1d, 0xff, 0x59] => self.is_nullifier_spent_call(calldata),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shielded_address() {
        assert_eq!(
            SHIELDED_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000202")
        );
    }

    #[test]
    fn test_shielded_precompile_deposit_and_get() {
        let mut provider = crate::storage::HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed sender balance
            let sender_slot = slot_balance(1, sender);
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                sender_slot,
                u128_to_u256(10_000),
            );

            let mut precompile = ShieldedPrecompile;

            // deposit(assetId=1, amount=1_000, commitment)
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x16, 0x8f, 0x44, 0xf5]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[52..68].copy_from_slice(&1_000u128.to_be_bytes());
            input[68..100].copy_from_slice(&test_commitment(1));

            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "deposit failed: {:?}", result.err());

            // getCommitmentCount
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0x11, 0x98, 0x5b, 0xa9]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let count = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&result.bytes[24..32]);
                buf
            });
            assert_eq!(count, 1);

            // getCommitment(0)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x23, 0x82, 0xd4, 0xc4]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(&result.bytes[..], &test_commitment(1));

            // isNullifierSpent(commitment) -> false
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x37, 0x1d, 0xff, 0x59]);
            input[4..36].copy_from_slice(&test_commitment(1));
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 0);
        });
    }

    #[test]
    fn test_shielded_precompile_withdraw() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x55);
        let target = Address::repeat_byte(0x66);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed target balance area (not needed but ok)
            let mut precompile = ShieldedPrecompile;

            // withdraw(assetId=0, target, amount=500, nullifier)
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x17, 0x52, 0x31, 0xd5]);
            input[28..36].copy_from_slice(&0u64.to_be_bytes());
            input[48..68].copy_from_slice(target.as_slice());
            input[84..100].copy_from_slice(&500u128.to_be_bytes());
            input[100..132].copy_from_slice(&[0xBBu8; 32]);

            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "withdraw failed: {:?}", result.err());

            // isNullifierSpent(0xBB) -> true
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x37, 0x1d, 0xff, 0x59]);
            input[4..36].copy_from_slice(&[0xBBu8; 32]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1);

            // Target balance should be 500
            let target_slot = slot_balance(0, target);
            let bal = crate::storage::StorageCtx::sload(crate::ASSET_ADDRESS, target_slot)
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(bal, 500);
        });
    }

    #[test]
    fn test_shielded_precompile_transfer() {
        let mut provider = crate::storage::HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = ShieldedPrecompile;

            // transfer(assetId=1, nullifiers=[0xCC], commitments=[0xDD, 0xEE])
            // Layout: selector(4) + assetId(32) + nullifiers_offset(32) + commitments_offset(32)
            let mut input = vec![0u8; 264];
            input[0..4].copy_from_slice(&[0x1c, 0x3b, 0x10, 0xf8]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            // nullifiers_offset = 96 (0x60) at last 8 bytes of word at offset 36
            input[60..68].copy_from_slice(&96u64.to_be_bytes());
            // commitments_offset = 160 (0xA0) at last 8 bytes of word at offset 68
            input[92..100].copy_from_slice(&160u64.to_be_bytes());

            // nullifiers array at abs_offset = 4 + 96 = 100
            // length word at 100-131, value in last 8 bytes (124-131)
            input[124..132].copy_from_slice(&1u64.to_be_bytes());
            // element 0 at 132-163
            input[132..164].copy_from_slice(&[0xCCu8; 32]);

            // commitments array at abs_offset = 4 + 160 = 164
            // length word at 164-195, value in last 8 bytes (188-195)
            input[188..196].copy_from_slice(&2u64.to_be_bytes());
            // element 0 at 196-227
            input[196..228].copy_from_slice(&test_commitment(3));
            // element 1 at 228-259
            input[228..260].copy_from_slice(&test_commitment(4));

            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "transfer failed: {:?}", result.err());

            // getCommitmentCount should be 2
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0x11, 0x98, 0x5b, 0xa9]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let count = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&result.bytes[24..32]);
                buf
            });
            assert_eq!(count, 2);

            // isNullifierSpent(0xCC) -> true
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x37, 0x1d, 0xff, 0x59]);
            input[4..36].copy_from_slice(&[0xCCu8; 32]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1);
        });
    }

    fn test_commitment(n: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0] = n;
        out
    }

    #[test]
    fn test_shielded_precompile_deposit_updates_merkle_root() {
        let mut provider = crate::storage::HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        crate::storage::StorageCtx::enter(&mut provider, || {
            let sender_slot = slot_balance(1, sender);
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                sender_slot,
                u128_to_u256(10_000),
            );

            let mut precompile = ShieldedPrecompile;
            let commitment = test_commitment(1);

            // deposit
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x16, 0x8f, 0x44, 0xf5]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[52..68].copy_from_slice(&1_000u128.to_be_bytes());
            input[68..100].copy_from_slice(&commitment);
            precompile.call(&input, sender).unwrap();

            // getMerkleRoot — should return non-zero
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0xe0, 0xc7, 0x49, 0x7f]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let root1: [u8; 32] = result.bytes[..32].try_into().unwrap();
            assert_ne!(root1, [0u8; 32], "merkle root should not be zero after deposit");

            // Compute expected root locally using the same Poseidon tree
            let mut tree = call_shielded::PoseidonMerkleTree::new(32);
            let tree_root = tree.insert(&commitment);
            assert_eq!(root1, tree_root, "merkle root should match local computation");

            // Second deposit — root should change
            let commitment2 = test_commitment(2);
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x16, 0x8f, 0x44, 0xf5]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[52..68].copy_from_slice(&2_000u128.to_be_bytes());
            input[68..100].copy_from_slice(&commitment2);
            precompile.call(&input, sender).unwrap();

            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0xe0, 0xc7, 0x49, 0x7f]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let root2: [u8; 32] = result.bytes[..32].try_into().unwrap();
            assert_ne!(root1, root2, "merkle root should change after second deposit");

            let mut tree = call_shielded::PoseidonMerkleTree::new(32);
            tree.insert(&commitment);
            tree.insert(&commitment2);
            let expected_root2 = tree.root();
            assert_eq!(root2, expected_root2);
        });
    }

    #[test]
    fn test_shielded_precompile_transfer_updates_merkle_root() {
        let mut provider = crate::storage::HashMapStorageProvider::new(5_000_000);
        let sender = Address::repeat_byte(0x55);

        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = ShieldedPrecompile;

            // transfer(nullifiers=[0xCC], commitments=[0xDD])
            // Layout: selector(4) + assetId(32) + nullifiers_offset(32) + commitments_offset(32)
            // = 100 bytes fixed
            // nullifiers_offset = 96 -> abs = 100, len = 1, elem = 32 -> total 64
            // commitments_offset = 160 -> abs = 164, len = 1, elem = 32 -> total 64
            // total = 100 + 64 + 64 = 228
            let mut input = vec![0u8; 228];
            input[0..4].copy_from_slice(&[0x1c, 0x3b, 0x10, 0xf8]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            // nullifiers_offset = 96 (0x60)
            input[60..68].copy_from_slice(&96u64.to_be_bytes());
            // commitments_offset = 160 (0xA0)
            input[92..100].copy_from_slice(&160u64.to_be_bytes());

            // nullifiers array at abs_offset = 4 + 96 = 100
            input[124..132].copy_from_slice(&1u64.to_be_bytes());
            input[132..164].copy_from_slice(&[0xCCu8; 32]);

            // commitments array at abs_offset = 4 + 160 = 164
            input[188..196].copy_from_slice(&1u64.to_be_bytes());
            input[196..228].copy_from_slice(&test_commitment(3));

            precompile.call(&input, sender).unwrap();

            // getMerkleRoot — should reflect the new commitment
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0xe0, 0xc7, 0x49, 0x7f]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let root: [u8; 32] = result.bytes[..32].try_into().unwrap();
            assert_ne!(root, [0u8; 32], "merkle root should not be zero after transfer");

            let mut tree = call_shielded::PoseidonMerkleTree::new(32);
            tree.insert(&test_commitment(3));
            assert_eq!(root, tree.root());
        });
    }
}
