//! Validator precompile at 0x204
//!
//! Functions: stake, unstake, claimUnbonded, getValidatorStake, getValidatorStatus,
//!            getValidatorPubkey, getUnbondHeight, getValidatorByIndex

use alloy_primitives::{address, Address, U256};
use revm_precompile::{PrecompileError, PrecompileOutput};

use crate::StatefulPrecompile;
use crate::storage::{storage_slot, StorageCtx};

#[allow(dead_code)]
pub(crate) const VALIDATOR_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000204");

const CALL_ASSET_ID: u64 = 1;
const MIN_SELF_STAKE: u128 = 1_000_000;
const UNBONDING_PERIOD_BLOCKS: u64 = 120_960;
const STAKING_ESCROW: Address = Address::repeat_byte(0);

// ── ABI decoding helpers ──────────────────────────────────────────────

fn decode_address(input: &[u8], slot_offset: usize) -> Option<Address> {
    let start = slot_offset + 12;
    if input.len() < start + 20 {
        return None;
    }
    Some(Address::from_slice(&input[start..start + 20]))
}

fn decode_u64(input: &[u8], slot_offset: usize) -> Option<u64> {
    let start = slot_offset + 24;
    if input.len() < start + 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&input[start..start + 8]);
    Some(u64::from_be_bytes(buf))
}

fn decode_u128(input: &[u8], slot_offset: usize) -> Option<u128> {
    let start = slot_offset + 16;
    if input.len() < start + 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&input[start..start + 16]);
    Some(u128::from_be_bytes(buf))
}

fn decode_bytes32(input: &[u8], slot_offset: usize) -> Option<[u8; 32]> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&input[slot_offset..slot_offset + 32]);
    Some(buf)
}

// ── Encoding helpers ──────────────────────────────────────────────────

fn encode_u128(value: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&value.to_be_bytes());
    out
}

fn encode_u64(value: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&value.to_be_bytes());
    out
}

fn encode_u8(value: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[31] = value;
    out
}

// ── Storage slot helpers (match evm_instructions.rs layout) ───────────

fn slot_validator_count() -> U256 {
    U256::ZERO
}

pub fn slot_validator_by_addr(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"validator_id"])
}

fn slot_validator_addr(index: u64) -> U256 {
    storage_slot(&[b"validators"]) + U256::from(index)
}

fn slot_validator_stake(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"stake"])
}

fn slot_validator_pubkey(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"pubkey"])
}

fn slot_validator_status(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"status"])
}

fn slot_validator_unbond_height(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"unbond_at"])
}

fn slot_unbonding_count() -> U256 {
    U256::from(1)
}

fn slot_unbonding(index: u64) -> U256 {
    storage_slot(&[b"unbonding"]) + U256::from(index)
}

fn slot_balance(asset_id: u64, addr: Address) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], addr.as_slice()])
}

// ── U256 <-> primitive helpers ────────────────────────────────────────

fn u256_to_u64(v: U256) -> u64 {
    u64::from_be_bytes(v.to_be_bytes::<32>()[24..32].try_into().unwrap())
}

fn u256_to_u128(v: U256) -> u128 {
    u128::from_be_bytes(v.to_be_bytes::<32>()[16..32].try_into().unwrap())
}

fn u128_to_u256(v: u128) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[16..32].copy_from_slice(&v.to_be_bytes());
    U256::from_be_bytes::<32>(bytes)
}

fn u64_to_u256(v: u64) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[24..32].copy_from_slice(&v.to_be_bytes());
    U256::from_be_bytes::<32>(bytes)
}

fn address_to_u256(addr: Address) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[12..32].copy_from_slice(addr.as_slice());
    U256::from_be_bytes::<32>(bytes)
}

fn u256_to_address(v: U256) -> Address {
    Address::from_slice(&v.to_be_bytes::<32>()[12..32])
}

// ── ValidatorPrecompile ───────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct ValidatorPrecompile;

impl ValidatorPrecompile {
    // stake(bytes32,uint128) -> 0x48720640
    fn stake(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 50000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 68 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let pubkey = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid pubkey".into()))?;
        let amount = decode_u128(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        if amount < MIN_SELF_STAKE {
            return Err(PrecompileError::Other(
                format!("validator stake: below minimum {MIN_SELF_STAKE}").into(),
            ));
        }

        // Check not already staked
        let existing_id = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_by_addr(msg_sender))
            .map(u256_to_u64)
            .unwrap_or(0);
        if existing_id != 0 {
            return Err(PrecompileError::Other("validator stake: already staked".into()));
        }

        // Deduct CALL from sender, credit escrow
        let sender_slot = slot_balance(CALL_ASSET_ID, msg_sender);
        let sender_bal = StorageCtx::sload(crate::ASSET_ADDRESS, sender_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        let sender_bal = sender_bal
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("validator stake: insufficient balance".into()))?;
        StorageCtx::sstore(crate::ASSET_ADDRESS, sender_slot, u128_to_u256(sender_bal));

        let escrow_slot = slot_balance(CALL_ASSET_ID, STAKING_ESCROW);
        let escrow_bal = StorageCtx::sload(crate::ASSET_ADDRESS, escrow_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        let escrow_bal = escrow_bal
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("validator stake: escrow overflow".into()))?;
        StorageCtx::sstore(crate::ASSET_ADDRESS, escrow_slot, u128_to_u256(escrow_bal));

        // Register validator
        let count = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_count())
            .map(u256_to_u64)
            .unwrap_or(0);
        let validator_id = count + 1;
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_count(), u64_to_u256(validator_id));
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_by_addr(msg_sender), u64_to_u256(validator_id));
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_addr(validator_id), address_to_u256(msg_sender));
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_stake(msg_sender), u128_to_u256(amount));
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_pubkey(msg_sender), U256::from_be_slice(&pubkey));
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_status(msg_sender), U256::from(1u8));

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // unstake(uint64) -> 0xd29ab87a
    fn unstake(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let validator_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid validator_id".into()))?;

        let stored_id = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_by_addr(msg_sender))
            .map(u256_to_u64)
            .unwrap_or(0);
        if stored_id == 0 {
            return Err(PrecompileError::Other("validator unstake: not a validator".into()));
        }
        if stored_id != validator_id {
            return Err(PrecompileError::Other("validator unstake: id mismatch".into()));
        }

        let status = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_status(msg_sender))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        if status != 1 {
            return Err(PrecompileError::Other("validator unstake: already unbonding".into()));
        }

        let stake = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_stake(msg_sender))
            .map(u256_to_u128)
            .unwrap_or(0);

        // Set status to unbonding (2)
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_status(msg_sender), U256::from(2u8));

        // Record unbond height
        let block_number = StorageCtx::block_number();
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_unbond_height(msg_sender), u64_to_u256(block_number));

        // Add to unbonding queue
        let unbonding_count = StorageCtx::sload(VALIDATOR_ADDRESS, slot_unbonding_count())
            .map(u256_to_u64)
            .unwrap_or(0);
        let mut packed = [0u8; 32];
        packed[8..16].copy_from_slice(&stored_id.to_be_bytes());
        packed[16..32].copy_from_slice(&stake.to_be_bytes());
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_unbonding(unbonding_count), U256::from_be_slice(&packed));
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_unbonding_count(), u64_to_u256(unbonding_count + 1));

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // claimUnbonded(uint64) -> 0x6ab76049
    fn claim_unbonded(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 12 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let validator_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid validator_id".into()))?;

        let stored_id = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_by_addr(msg_sender))
            .map(u256_to_u64)
            .unwrap_or(0);
        if stored_id == 0 {
            return Err(PrecompileError::Other("validator claim: not a validator".into()));
        }
        if stored_id != validator_id {
            return Err(PrecompileError::Other("validator claim: id mismatch".into()));
        }

        let status = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_status(msg_sender))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        if status != 2 {
            return Err(PrecompileError::Other("validator claim: not unbonding".into()));
        }

        let unbond_height = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_unbond_height(msg_sender))
            .map(u256_to_u64)
            .unwrap_or(0);
        let current_block = StorageCtx::block_number();
        if current_block < unbond_height + UNBONDING_PERIOD_BLOCKS {
            return Err(PrecompileError::Other("validator claim: unbonding period not elapsed".into()));
        }

        // Find unbonding request
        let unbonding_count = StorageCtx::sload(VALIDATOR_ADDRESS, slot_unbonding_count())
            .map(u256_to_u64)
            .unwrap_or(0);
        let mut amount = 0u128;
        let mut found = false;
        for i in 0..unbonding_count {
            let packed = StorageCtx::sload(VALIDATOR_ADDRESS, slot_unbonding(i))
                .map(|v| v.to_be_bytes::<32>())
                .unwrap_or([0u8; 32]);
            let entry_id = u64::from_be_bytes(packed[8..16].try_into().unwrap());
            if entry_id == stored_id {
                amount = u128::from_be_bytes(packed[16..32].try_into().unwrap());
                found = true;
                break;
            }
        }
        if !found {
            return Err(PrecompileError::Other("validator claim: no unbonding request found".into()));
        }

        // Return stake from escrow to sender
        let escrow_slot = slot_balance(CALL_ASSET_ID, STAKING_ESCROW);
        let escrow_bal = StorageCtx::sload(crate::ASSET_ADDRESS, escrow_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        let escrow_bal = escrow_bal
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("validator claim: escrow underflow".into()))?;
        StorageCtx::sstore(crate::ASSET_ADDRESS, escrow_slot, u128_to_u256(escrow_bal));

        let sender_slot = slot_balance(CALL_ASSET_ID, msg_sender);
        let sender_bal = StorageCtx::sload(crate::ASSET_ADDRESS, sender_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        let sender_bal = sender_bal
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("validator claim: balance overflow".into()))?;
        StorageCtx::sstore(crate::ASSET_ADDRESS, sender_slot, u128_to_u256(sender_bal));

        // Clear validator state
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_by_addr(msg_sender), U256::ZERO);
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_stake(msg_sender), U256::ZERO);
        StorageCtx::sstore(VALIDATOR_ADDRESS, slot_validator_status(msg_sender), U256::ZERO);

        let out = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getValidatorStake(address) -> 0x34664846
    fn get_validator_stake(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let addr = decode_address(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid address".into()))?;

        let stake = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_stake(addr))
            .map(u256_to_u128)
            .unwrap_or(0);

        let out = PrecompileOutput::new(0, encode_u128(stake).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getValidatorStatus(address) -> 0xa310624f
    fn get_validator_status(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let addr = decode_address(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid address".into()))?;

        let status = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_status(addr))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);

        let out = PrecompileOutput::new(0, encode_u8(status).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getValidatorPubkey(address) -> 0x9511f44f
    fn get_validator_pubkey(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let addr = decode_address(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid address".into()))?;

        let pubkey = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_pubkey(addr))
            .map(|v| v.to_be_bytes::<32>())
            .unwrap_or([0u8; 32]);

        let out = PrecompileOutput::new(0, pubkey.to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getUnbondHeight(address) -> 0x528e09b6
    fn get_unbond_height(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let addr = decode_address(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid address".into()))?;

        let height = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_unbond_height(addr))
            .map(u256_to_u64)
            .unwrap_or(0);

        let out = PrecompileOutput::new(0, encode_u64(height).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // getValidatorByIndex(uint256) -> 0x3fce1b0d
    fn get_validator_by_index(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let index = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid index".into()))?;

        let addr = StorageCtx::sload(VALIDATOR_ADDRESS, slot_validator_addr(index))
            .map(u256_to_address)
            .unwrap_or(Address::ZERO);

        let mut out = [0u8; 32];
        out[12..32].copy_from_slice(addr.as_slice());
        let out = PrecompileOutput::new(0, out.to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }
}

impl StatefulPrecompile for ValidatorPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        match &calldata[..4] {
            &[0x48, 0x72, 0x06, 0x40] => self.stake(calldata, msg_sender),
            &[0xd2, 0x9a, 0xb8, 0x7a] => self.unstake(calldata, msg_sender),
            &[0x6a, 0xb7, 0x60, 0x49] => self.claim_unbonded(calldata, msg_sender),
            &[0x34, 0x66, 0x48, 0x46] => self.get_validator_stake(calldata),
            &[0xa3, 0x10, 0x62, 0x4f] => self.get_validator_status(calldata),
            &[0x95, 0x11, 0xf4, 0x4f] => self.get_validator_pubkey(calldata),
            &[0x52, 0x8e, 0x09, 0xb6] => self.get_unbond_height(calldata),
            &[0x3f, 0xce, 0x1b, 0x0d] => self.get_validator_by_index(calldata),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;

    #[test]
    fn test_validator_address() {
        assert_eq!(
            VALIDATOR_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000204")
        );
    }

    #[test]
    fn test_validator_precompile_stake_and_get() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x11);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed sender balance
            let sender_slot = slot_balance(CALL_ASSET_ID, sender);
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                sender_slot,
                u128_to_u256(10_000_000),
            );

            let mut precompile = ValidatorPrecompile;

            // stake(pubkey, amount=5_000_000)
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x48, 0x72, 0x06, 0x40]);
            input[4..36].copy_from_slice(&[0xAAu8; 32]); // pubkey
            input[52..68].copy_from_slice(&5_000_000u128.to_be_bytes());

            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "stake failed: {:?}", result.err());

            // getValidatorStake(sender)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x34, 0x66, 0x48, 0x46]);
            input[16..36].copy_from_slice(sender.as_slice());

            let result = precompile.call(&input, Address::ZERO).unwrap();
            let stake = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(stake, 5_000_000);

            // getValidatorStatus(sender)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0xa3, 0x10, 0x62, 0x4f]);
            input[16..36].copy_from_slice(sender.as_slice());

            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1); // active
        });
    }

    #[test]
    fn test_validator_precompile_unstake_and_claim() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x11);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed sender balance
            let sender_slot = slot_balance(CALL_ASSET_ID, sender);
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                sender_slot,
                u128_to_u256(10_000_000),
            );

            let mut precompile = ValidatorPrecompile;

            // stake first
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x48, 0x72, 0x06, 0x40]);
            input[4..36].copy_from_slice(&[0xAAu8; 32]);
            input[52..68].copy_from_slice(&5_000_000u128.to_be_bytes());
            precompile.call(&input, sender).unwrap();

            // unstake(validatorId=1)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0xd2, 0x9a, 0xb8, 0x7a]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "unstake failed: {:?}", result.err());

            // status should be 2 (unbonding)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0xa3, 0x10, 0x62, 0x4f]);
            input[16..36].copy_from_slice(sender.as_slice());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 2);

            // claimUnbonded should fail: period not elapsed
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x6a, 0xb7, 0x60, 0x49]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            let result = precompile.call(&input, sender);
            assert!(result.is_err(), "claim should fail before period elapsed");
        });
    }

    #[test]
    fn test_validator_precompile_read_methods() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x11);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed sender balance
            let sender_slot = slot_balance(CALL_ASSET_ID, sender);
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                sender_slot,
                u128_to_u256(10_000_000),
            );

            let mut precompile = ValidatorPrecompile;

            // stake first
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x48, 0x72, 0x06, 0x40]);
            input[4..36].copy_from_slice(&[0xAAu8; 32]);
            input[52..68].copy_from_slice(&5_000_000u128.to_be_bytes());
            precompile.call(&input, sender).unwrap();

            // getValidatorPubkey(sender)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x95, 0x11, 0xf4, 0x4f]);
            input[16..36].copy_from_slice(sender.as_slice());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(&result.bytes[..], &[0xAAu8; 32]);

            // getValidatorByIndex(1)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x3f, 0xce, 0x1b, 0x0d]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let returned_addr = Address::from_slice(&result.bytes[12..32]);
            assert_eq!(returned_addr, sender);

            // unstake to set unbond height
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0xd2, 0x9a, 0xb8, 0x7a]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            precompile.call(&input, sender).unwrap();

            // getUnbondHeight(sender)
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x52, 0x8e, 0x09, 0xb6]);
            input[16..36].copy_from_slice(sender.as_slice());
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let height = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&result.bytes[24..32]);
                buf
            });
            assert_eq!(height, 0); // block_number defaults to 0 in test provider
        });
    }
}
