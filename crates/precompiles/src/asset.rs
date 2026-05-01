//! Asset precompile at 0x201
//!
//! Unified asset operations: getBalance, getAssetInfo, transfer, approve,
//! transferFrom, mint, burn, register.
//!
//! Replaces the deprecated 0x102 Balance precompile.

use alloy_primitives::{address, Address};
use revm_precompile::{PrecompileError, PrecompileResult, PrecompileOutput};

use crate::{
    address_to_u256, decode_address, decode_address_array, decode_string, decode_u128,
    decode_u128_array, decode_u64, encode_u128, encode_u64, load_bal, ok_empty, read_string32,
    require_caller, save_bal, slot_asset_meta, u128_to_u256, u256_to_address,
    u256_to_u128, u256_to_u64, u64_to_u256, write_string32,
};

pub const ASSET_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000201");

// ── AssetPrecompile (stateful, uses StorageCtx) ───────────────────────

use crate::StatefulPrecompile;
use crate::storage::storage_slot;

/// Compute the EVM storage slot for an allowance.
pub fn slot_allowance(asset_id: u64, owner: Address, spender: Address) -> alloy_primitives::U256 {
    storage_slot(&[
        &asset_id.to_be_bytes()[..],
        owner.as_slice(),
        spender.as_slice(),
    ])
}

// ── Asset metadata helpers ────────────────────────────────────────────

/// Read a single asset metadata slot from storage.
fn load_meta(asset_id: u64, key: &[u8]) -> Option<alloy_primitives::U256> {
    crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_asset_meta(asset_id, key))
}

fn load_meta_u128(asset_id: u64, key: &[u8]) -> u128 {
    load_meta(asset_id, key).map(u256_to_u128).unwrap_or(0)
}

fn load_meta_u8(asset_id: u64, key: &[u8]) -> u8 {
    load_meta(asset_id, key).map(|v| v.to_be_bytes::<32>()[31]).unwrap_or(0)
}

fn load_meta_address(asset_id: u64, key: &[u8]) -> Address {
    load_meta(asset_id, key).map(u256_to_address).unwrap_or(Address::ZERO)
}

fn load_meta_string(asset_id: u64, key: &[u8]) -> String {
    load_meta(asset_id, key).map(read_string32).unwrap_or_default()
}

/// Asset metadata loaded from storage in one shot.
pub struct AssetMeta {
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub issuer: Address,
    pub max_supply: u128,
    pub status: u8,
}

impl AssetMeta {
    /// Load all metadata fields for the given asset_id from storage.
    pub fn load(asset_id: u64) -> Self {
        Self {
            symbol: load_meta_string(asset_id, b"symbol"),
            name: load_meta_string(asset_id, b"name"),
            decimals: load_meta_u8(asset_id, b"decimals"),
            issuer: load_meta_address(asset_id, b"issuer"),
            max_supply: load_meta_u128(asset_id, b"max_supply"),
            status: load_meta_u8(asset_id, b"status"),
        }
    }
}

// ── Balance helpers ───────────────────────────────────────────────────

/// Decode the common (asset_id, address, amount) pattern from ABI input.
fn decode_asset_addr_amount(input: &[u8]) -> Option<(u64, Address, u128)> {
    if input.len() < 100 {
        return None;
    }
    Some((decode_u64(input, 4)?, decode_address(input, 36)?, decode_u128(input, 68)?))
}

/// Stateful asset precompile backed by EVM storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct AssetPrecompile;

impl AssetPrecompile {
    fn check_compliance(asset_id: u64, addr: &Address) -> Result<(), PrecompileError> {
        // Read compliance policy from asset storage
        let policy_id = load_meta(asset_id, b"compliance").map(u256_to_u64).unwrap_or(0);

        if policy_id == 0 {
            return Ok(());
        }

        // Read compliance status from COMPLIANCE_ADDRESS storage
        let status = crate::storage::StorageCtx::sload(
            crate::COMPLIANCE_ADDRESS,
            crate::slot_compliance(*addr, policy_id as u8),
        )
        .map(|v| v.to_be_bytes::<32>()[31])
        .unwrap_or(0);

        // status == 0 means Clear (allowed), anything else is restricted
        if status == 0 {
            Ok(())
        } else {
            Err(PrecompileError::Other("compliance check failed".into()))
        }
    }

    fn get_balance(&self, input: &[u8]) -> PrecompileResult {
        const GAS_COST: u64 = 800;
        crate::storage::StorageCtx::deduct_gas(GAS_COST)
            .ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 68 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid asset_id".into()))?;
        let addr = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid address".into()))?;

        let balance = load_bal(asset_id, addr);

        let output = PrecompileOutput::new(0, encode_u128(balance).to_vec().into());
        Ok(crate::storage::fill_precompile_output(output))
    }

    fn get_asset_info(&self, input: &[u8]) -> PrecompileResult {
        const GAS_COST: u64 = 1000;
        crate::storage::StorageCtx::deduct_gas(GAS_COST)
            .ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid asset_id".into()))?;

        let meta = AssetMeta::load(asset_id);

        let mut out = [0u8; 192];
        out[0..32].copy_from_slice(&write_string32(&meta.symbol).to_be_bytes::<32>());
        out[32..64].copy_from_slice(&write_string32(&meta.name).to_be_bytes::<32>());
        out[95] = meta.decimals;
        out[108..128].copy_from_slice(meta.issuer.as_slice());
        out[128..160].copy_from_slice(&encode_u128(meta.max_supply));
        out[191] = meta.status;

        let output = PrecompileOutput::new(0, out.to_vec().into());
        Ok(crate::storage::fill_precompile_output(output))
    }

    fn transfer(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        const GAS_COST: u64 = 5000;
        crate::storage::StorageCtx::deduct_gas(GAS_COST)
            .ok_or(PrecompileError::OutOfGas)?;
        let (asset_id, to, amount) = decode_asset_addr_amount(input)
            .ok_or_else(|| PrecompileError::Other("invalid input".into()))?;

        let from = require_caller(msg_sender)?;

        Self::check_compliance(asset_id, &from)?;
        Self::check_compliance(asset_id, &to)?;

        let from_balance = load_bal(asset_id, from)
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("insufficient balance".into()))?;
        let to_balance = load_bal(asset_id, to)
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("balance overflow".into()))?;

        save_bal(asset_id, from, from_balance);
        save_bal(asset_id, to, to_balance);

        ok_empty()
    }

    fn batch_transfer(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        if input.len() < 100 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid asset_id".into()))?;
        let recipients = decode_address_array(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid recipients array".into()))?;
        let amounts = decode_u128_array(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amounts array".into()))?;

        if recipients.len() != amounts.len() {
            return Err(PrecompileError::Other(
                "recipients and amounts length mismatch".into(),
            ));
        }
        if recipients.is_empty() {
            return Err(PrecompileError::Other("empty batch".into()));
        }

        const GAS_COST_PER: u64 = 5000;
        let total_gas = GAS_COST_PER * recipients.len() as u64;
        crate::storage::StorageCtx::deduct_gas(total_gas)
            .ok_or(PrecompileError::OutOfGas)?;

        let from = require_caller(msg_sender)?;

        Self::check_compliance(asset_id, &from)?;
        for to in &recipients {
            Self::check_compliance(asset_id, to)?;
        }

        let mut from_balance = load_bal(asset_id, from);

        for (to, amount) in recipients.iter().zip(amounts.iter()) {
            from_balance = from_balance.checked_sub(*amount)
                .ok_or_else(|| PrecompileError::Other("insufficient balance".into()))?;
            let to_balance = load_bal(asset_id, *to)
                .checked_add(*amount)
                .ok_or_else(|| PrecompileError::Other("balance overflow".into()))?;
            save_bal(asset_id, *to, to_balance);
        }

        save_bal(asset_id, from, from_balance);

        ok_empty()
    }

    fn approve(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        const GAS_COST: u64 = 3000;
        crate::storage::StorageCtx::deduct_gas(GAS_COST)
            .ok_or(PrecompileError::OutOfGas)?;
        let (asset_id, spender, amount) = decode_asset_addr_amount(input)
            .ok_or_else(|| PrecompileError::Other("invalid input".into()))?;

        let owner = require_caller(msg_sender)?;

        let slot = slot_allowance(asset_id, owner, spender);
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot, u128_to_u256(amount));

        ok_empty()
    }

    fn transfer_from(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        const GAS_COST: u64 = 6000;
        crate::storage::StorageCtx::deduct_gas(GAS_COST)
            .ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid asset_id".into()))?;
        let from = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid from address".into()))?;
        let to = decode_address(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid to address".into()))?;
        let amount = decode_u128(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        let spender = require_caller(msg_sender)?;

        Self::check_compliance(asset_id, &from)?;
        Self::check_compliance(asset_id, &to)?;

        let allowance_slot = slot_allowance(asset_id, from, spender);
        let allowance = crate::storage::StorageCtx::sload(ASSET_ADDRESS, allowance_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        if allowance < amount {
            return Err(PrecompileError::Other("insufficient allowance".into()));
        }
        let new_allowance = allowance - amount;
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, allowance_slot, u128_to_u256(new_allowance));

        let from_balance = load_bal(asset_id, from)
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("insufficient balance".into()))?;
        let to_balance = load_bal(asset_id, to)
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("balance overflow".into()))?;

        save_bal(asset_id, from, from_balance);
        save_bal(asset_id, to, to_balance);

        ok_empty()
    }

    fn mint(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        const GAS_COST: u64 = 10000;
        crate::storage::StorageCtx::deduct_gas(GAS_COST)
            .ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let (asset_id, to, amount) = decode_asset_addr_amount(input)
            .ok_or_else(|| PrecompileError::Other("invalid input".into()))?;

        let caller = require_caller(msg_sender)?;

        let meta = AssetMeta::load(asset_id);
        if meta.issuer != caller {
            return Err(PrecompileError::Other("not asset issuer".into()));
        }

        let supply = load_meta_u128(asset_id, b"supply");
        let new_supply = supply.checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("supply overflow".into()))?;
        if meta.max_supply > 0 && new_supply > meta.max_supply {
            return Err(PrecompileError::Other("max supply exceeded".into()));
        }
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(new_supply));

        let to_balance = load_bal(asset_id, to)
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("balance overflow".into()))?;
        save_bal(asset_id, to, to_balance);

        ok_empty()
    }

    fn burn(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        const GAS_COST: u64 = 8000;
        crate::storage::StorageCtx::deduct_gas(GAS_COST)
            .ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let (asset_id, from, amount) = decode_asset_addr_amount(input)
            .ok_or_else(|| PrecompileError::Other("invalid input".into()))?;

        let caller = require_caller(msg_sender)?;

        if caller != from {
            let allowance_slot = slot_allowance(asset_id, from, caller);
            let allowance = crate::storage::StorageCtx::sload(ASSET_ADDRESS, allowance_slot)
                .map(u256_to_u128)
                .unwrap_or(0);
            if allowance < amount {
                return Err(PrecompileError::Other("insufficient allowance".into()));
            }
            crate::storage::StorageCtx::sstore(ASSET_ADDRESS, allowance_slot, u128_to_u256(allowance - amount));
        }

        let supply = load_meta_u128(asset_id, b"supply");
        let new_supply = supply.checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("supply underflow".into()))?;
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(new_supply));

        let from_balance = load_bal(asset_id, from)
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("insufficient balance".into()))?;
        save_bal(asset_id, from, from_balance);

        ok_empty()
    }

    fn register(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        const GAS_COST: u64 = 50000;
        crate::storage::StorageCtx::deduct_gas(GAS_COST)
            .ok_or(PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        let symbol = decode_string(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid symbol".into()))?;
        let name = decode_string(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid name".into()))?;
        let decimals = decode_u64(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid decimals".into()))? as u8;
        let max_supply = decode_u128(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid maxSupply".into()))?;

        let caller = require_caller(msg_sender)?;

        let next_id_slot = alloy_primitives::U256::from(0);
        let asset_id = crate::storage::StorageCtx::sload(ASSET_ADDRESS, next_id_slot)
            .map(u256_to_u64)
            .unwrap_or(0);
        let asset_id = if asset_id == 0 { 1 } else { asset_id };
        let next_id = asset_id.checked_add(1)
            .ok_or_else(|| PrecompileError::Other("asset id overflow".into()))?;
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, next_id_slot, u64_to_u256(next_id));

        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"symbol"), write_string32(&symbol));
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"name"), write_string32(&name));
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"decimals"), alloy_primitives::U256::from(decimals));
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer"), address_to_u256(caller));
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"max_supply"), u128_to_u256(max_supply));
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), alloy_primitives::U256::from(0));
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"), alloy_primitives::U256::from(0));
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"compliance"), alloy_primitives::U256::from(0));
        crate::storage::StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"registered_at"), u64_to_u256(0));

        let output = PrecompileOutput::new(0, encode_u64(asset_id).to_vec().into());
        Ok(crate::storage::fill_precompile_output(output))
    }
}

impl StatefulPrecompile for AssetPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let selector = &calldata[..4];
        match selector {
            &[0xd2, 0x14, 0x25, 0xdf] => self.get_balance(calldata),
            &[0x4e, 0xc3, 0xce, 0x7f] => self.get_asset_info(calldata),
            &[0xd1, 0x5d, 0xcd, 0x62] => self.transfer(calldata, msg_sender),
            &[0x5f, 0x91, 0x61, 0xbb] => self.batch_transfer(calldata, msg_sender),
            &[0x7e, 0x2e, 0xad, 0x93] => self.approve(calldata, msg_sender),
            &[0xa1, 0x3e, 0x0f, 0xba] => self.transfer_from(calldata, msg_sender),
            &[0xf2, 0xbe, 0x45, 0x99] => self.mint(calldata, msg_sender),
            &[0x2b, 0x7d, 0x14, 0x80] => self.mint(calldata, msg_sender), // issuerMint alias
            &[0x73, 0x71, 0x28, 0x63] => self.burn(calldata, msg_sender),
            &[0x48, 0x4a, 0x57, 0x3d] => self.register(calldata, msg_sender),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot_balance;
    use call_primitives::Address;

    #[test]
    fn test_asset_address() {
        assert_eq!(
            ASSET_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000201")
        );
    }

    #[test]
    fn test_decode_u64() {
        let mut input = [0u8; 36];
        input[28..36].copy_from_slice(&42u64.to_be_bytes());
        assert_eq!(decode_u64(&input, 4), Some(42));
    }

    #[test]
    fn test_decode_address() {
        let mut input = [0u8; 36];
        let addr = Address::repeat_byte(0xAB);
        input[12..32].copy_from_slice(addr.as_slice());
        assert_eq!(decode_address(&input, 0), Some(addr));
    }

    #[test]
    fn test_write_string32() {
        let v = write_string32("TEST");
        let encoded = v.to_be_bytes::<32>();
        assert_eq!(&encoded[0..4], b"TEST");
        assert_eq!(&encoded[4..32], &[0u8; 28]);
    }
    #[test]
    fn test_asset_precompile_get_balance() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let addr = Address::repeat_byte(0xAB);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed balance
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, addr),
                u128_to_u256(5000),
            );

            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0xd2, 0x14, 0x25, 0xdf]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(addr.as_slice());

            let mut precompile = AssetPrecompile;
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let balance = u256_to_u128(alloy_primitives::U256::from_be_bytes::<32>(
                result.bytes.as_ref().try_into().unwrap()
            ));
            assert_eq!(balance, 5000);
        });
    }

    #[test]
    fn test_asset_precompile_transfer() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let to = Address::repeat_byte(0xCD);

        crate::storage::StorageCtx::enter(&mut provider, || {
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, from),
                u128_to_u256(1000),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0xd1, 0x5d, 0xcd, 0x62]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(to.as_slice());
            input[84..100].copy_from_slice(&500u128.to_be_bytes());

            let mut precompile = AssetPrecompile;
            let result = precompile.call(&input, from);
            assert!(result.is_ok(), "transfer failed: {:?}", result.err());

            let from_bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, from))
                .map(u256_to_u128).unwrap_or(0);
            let to_bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, to))
                .map(u256_to_u128).unwrap_or(0);
            assert_eq!(from_bal, 500);
            assert_eq!(to_bal, 500);
        });
    }

    #[test]
    fn test_asset_precompile_mint_and_burn() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Register asset
            let mut input = vec![0u8; 4 + 4 * 32 + 64 + 64];
            input[0..4].copy_from_slice(&[0x48, 0x4a, 0x57, 0x3d]);
            // offset_symbol = 128
            input[4 + 24..4 + 32].copy_from_slice(&128u64.to_be_bytes());
            // offset_name
            let name_offset = 128 + 64;
            input[36 + 24..36 + 32].copy_from_slice(&(name_offset as u64).to_be_bytes());
            // decimals = 18
            input[68 + 31] = 18;
            // maxSupply = 10000
            input[100 + 16..100 + 32].copy_from_slice(&10000u128.to_be_bytes());
            // symbol data
            let sym_abs = 4 + 128;
            input[sym_abs + 24..sym_abs + 32].copy_from_slice(&4u64.to_be_bytes());
            input[sym_abs + 32..sym_abs + 36].copy_from_slice(b"GOLD");
            // name data
            let name_abs = 4 + name_offset;
            input[name_abs + 24..name_abs + 32].copy_from_slice(&4u64.to_be_bytes());
            input[name_abs + 32..name_abs + 36].copy_from_slice(b"Gold");

            let mut precompile = AssetPrecompile;
            let result = precompile.call(&input, issuer).unwrap();
            let asset_id = u64::from_be_bytes([
                result.bytes[24], result.bytes[25], result.bytes[26], result.bytes[27],
                result.bytes[28], result.bytes[29], result.bytes[30], result.bytes[31],
            ]);
            assert_eq!(asset_id, 1);

            // Mint
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0xf2, 0xbe, 0x45, 0x99]);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&500u128.to_be_bytes());

            let result = precompile.call(&input, issuer);
            assert!(result.is_ok(), "mint failed: {:?}", result.err());

            let bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(asset_id, recipient))
                .map(u256_to_u128).unwrap_or(0);
            assert_eq!(bal, 500);

            let supply = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"))
                .map(u256_to_u128).unwrap_or(0);
            assert_eq!(supply, 500);

            // Mint to issuer
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0xf2, 0xbe, 0x45, 0x99]);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(issuer.as_slice());
            input[84..100].copy_from_slice(&400u128.to_be_bytes());

            let result = precompile.call(&input, issuer);
            assert!(result.is_ok());

            // Burn from issuer
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x73, 0x71, 0x28, 0x63]);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(issuer.as_slice());
            input[84..100].copy_from_slice(&200u128.to_be_bytes());

            let result = precompile.call(&input, issuer);
            assert!(result.is_ok(), "burn failed: {:?}", result.err());

            let issuer_bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(asset_id, issuer))
                .map(u256_to_u128).unwrap_or(0);
            assert_eq!(issuer_bal, 200);

            let supply = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"))
                .map(u256_to_u128).unwrap_or(0);
            assert_eq!(supply, 700);
        });
    }

    #[test]
    fn test_asset_precompile_approve_and_transfer_from() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let owner = Address::repeat_byte(0xAB);
        let spender = Address::repeat_byte(0xEF);
        let recipient = Address::repeat_byte(0xCD);

        crate::storage::StorageCtx::enter(&mut provider, || {
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, owner),
                u128_to_u256(1000),
            );

            let mut precompile = AssetPrecompile;

            // Approve
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x7e, 0x2e, 0xad, 0x93]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(spender.as_slice());
            input[84..100].copy_from_slice(&300u128.to_be_bytes());

            let result = precompile.call(&input, owner);
            assert!(result.is_ok());

            // transferFrom
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0xa1, 0x3e, 0x0f, 0xba]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(owner.as_slice());
            input[80..100].copy_from_slice(recipient.as_slice());
            input[116..132].copy_from_slice(&200u128.to_be_bytes());

            let result = precompile.call(&input, spender);
            assert!(result.is_ok(), "transfer_from failed: {:?}", result.err());

            let owner_bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, owner))
                .map(u256_to_u128).unwrap_or(0);
            let recipient_bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, recipient))
                .map(u256_to_u128).unwrap_or(0);
            let allowance = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_allowance(1, owner, spender))
                .map(u256_to_u128).unwrap_or(0);

            assert_eq!(owner_bal, 800);
            assert_eq!(recipient_bal, 200);
            assert_eq!(allowance, 100);
        });
    }

    #[test]
    fn test_asset_precompile_register_and_get_info() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let caller = Address::repeat_byte(0x33);

        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut input = vec![0u8; 4 + 4 * 32 + 64 + 64];
            input[0..4].copy_from_slice(&[0x48, 0x4a, 0x57, 0x3d]);
            input[4 + 24..4 + 32].copy_from_slice(&128u64.to_be_bytes());
            let name_offset = 128 + 64;
            input[36 + 24..36 + 32].copy_from_slice(&(name_offset as u64).to_be_bytes());
            input[68 + 31] = 18;
            input[100 + 16..100 + 32].copy_from_slice(&1_000_000u128.to_be_bytes());
            let sym_abs = 4 + 128;
            input[sym_abs + 24..sym_abs + 32].copy_from_slice(&4u64.to_be_bytes());
            input[sym_abs + 32..sym_abs + 36].copy_from_slice(b"TEST");
            let name_abs = 4 + name_offset;
            input[name_abs + 24..name_abs + 32].copy_from_slice(&10u64.to_be_bytes());
            input[name_abs + 32..name_abs + 42].copy_from_slice(b"Test Token");

            let mut precompile = AssetPrecompile;
            let result = precompile.call(&input, caller).unwrap();
            let asset_id = u64::from_be_bytes([
                result.bytes[24], result.bytes[25], result.bytes[26], result.bytes[27],
                result.bytes[28], result.bytes[29], result.bytes[30], result.bytes[31],
            ]);
            assert_eq!(asset_id, 1);

            // getAssetInfo
            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x4e, 0xc3, 0xce, 0x7f]);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());

            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes.len(), 192);
            let symbol = read_string32(alloy_primitives::U256::from_be_bytes::<32>(
                result.bytes[0..32].try_into().unwrap()
            ));
            let name = read_string32(alloy_primitives::U256::from_be_bytes::<32>(
                result.bytes[32..64].try_into().unwrap()
            ));
            assert_eq!(symbol, "TEST");
            assert_eq!(name, "Test Token");
            assert_eq!(result.bytes[95], 18);
            assert_eq!(&result.bytes[108..128], caller.as_slice());
        });
    }
}
