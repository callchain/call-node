//! Asset precompile entry point (0x201).
//!
//! Thin wrapper that routes EVM calls to [`AssetStorage`] backed by
//! [`JournalBackend`].  Business logic lives in [`AssetStorage`]; this file
//! only handles ABI decode/encode, gas accounting, selector dispatch and
//! cross-domain concerns (compliance).

use crate::{AssetError, AssetStorage};
use call_precompiles::{
    journal_backend::JournalBackend, decode_address, decode_address_array, decode_string,
    decode_u128, decode_u128_array, decode_u64, encode_u128, encode_u64, ok_empty,
    require_caller, slot_allowance, slot_asset_meta, u128_to_u256, u256_to_u128, u256_to_u64,
    write_string32, ASSET_ADDRESS, COMPLIANCE_ADDRESS, slot_compliance,
};
use call_primitives::Address;
use revm_precompile::{PrecompileError, PrecompileOutput, PrecompileResult};

/// Selector constants.
mod selector {
    pub const GET_BALANCE:      [u8; 4] = [0xd2, 0x14, 0x25, 0xdf];
    pub const GET_ASSET_INFO:   [u8; 4] = [0x4e, 0xc3, 0xce, 0x7f];
    pub const TRANSFER:         [u8; 4] = [0xd1, 0x5d, 0xcd, 0x62];
    pub const BATCH_TRANSFER:   [u8; 4] = [0x5f, 0x91, 0x61, 0xbb];
    pub const APPROVE:          [u8; 4] = [0x7e, 0x2e, 0xad, 0x93];
    pub const TRANSFER_FROM:    [u8; 4] = [0xa1, 0x3e, 0x0f, 0xba];
    pub const MINT:             [u8; 4] = [0xf2, 0xbe, 0x45, 0x99];
    pub const ISSUER_MINT:      [u8; 4] = [0x2b, 0x7d, 0x14, 0x80];
    pub const BURN:             [u8; 4] = [0x73, 0x71, 0x28, 0x63];
    pub const REGISTER:         [u8; 4] = [0x48, 0x4a, 0x57, 0x3d];
}

/// Stateful asset precompile backed by EVM storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct AssetPrecompile;

impl AssetPrecompile {
    /// Decode the common (asset_id, address, amount) pattern from ABI input.
    fn decode_asset_addr_amount(input: &[u8]) -> Option<(u64, Address, u128)> {
        if input.len() < 100 {
            return None;
        }
        Some((decode_u64(input, 4)?, decode_address(input, 36)?, decode_u128(input, 68)?))
    }

    /// Check compliance for an address against the asset's compliance policy.
    fn check_compliance(asset_id: u64, addr: &Address) -> Result<(), PrecompileError> {
        let policy_id = call_precompiles::storage::StorageCtx::sload(
            ASSET_ADDRESS,
            slot_asset_meta(asset_id, b"compliance"),
        )
        .map(|v| v.to_be_bytes::<32>()[31] as u64)
        .unwrap_or(0);

        if policy_id == 0 {
            return Ok(());
        }

        let status = call_precompiles::storage::StorageCtx::sload(
            COMPLIANCE_ADDRESS,
            slot_compliance(*addr, policy_id as u8),
        )
        .map(|v| v.to_be_bytes::<32>()[31])
        .unwrap_or(0);

        if status == 0 {
            Ok(())
        } else {
            Err(PrecompileError::Other("compliance check failed".into()))
        }
    }

    fn deduct(gas: u64) -> Result<(), PrecompileError> {
        call_precompiles::storage::StorageCtx::deduct_gas(gas)
            .ok_or(PrecompileError::OutOfGas)
    }

    fn get_balance(&self, input: &[u8]) -> PrecompileResult {
        Self::deduct(800)?;
        if input.len() < 68 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid asset_id".into()))?;
        let addr = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid address".into()))?;

        let store = AssetStorage::new(JournalBackend);
        let balance = store.read_balance(asset_id, addr);

        let output = PrecompileOutput::new(0, encode_u128(balance).to_vec().into());
        Ok(call_precompiles::storage::fill_precompile_output(output))
    }

    fn get_asset_info(&self, input: &[u8]) -> PrecompileResult {
        Self::deduct(1000)?;
        if input.len() < 36 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid asset_id".into()))?;

        let store = AssetStorage::new(JournalBackend);
        let meta = store.read_meta(asset_id);

        let mut out = [0u8; 192];
        out[0..32].copy_from_slice(&write_string32(&meta.symbol).to_be_bytes::<32>()
        );
        out[32..64].copy_from_slice(&write_string32(&meta.name).to_be_bytes::<32>()
        );
        out[95] = meta.decimals;
        out[108..128].copy_from_slice(meta.issuer.as_slice());
        out[128..160].copy_from_slice(&encode_u128(meta.max_supply));
        out[191] = meta.status;

        let output = PrecompileOutput::new(0, out.to_vec().into());
        Ok(call_precompiles::storage::fill_precompile_output(output))
    }

    fn transfer(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        Self::deduct(5000)?;
        let (asset_id, to, amount) = Self::decode_asset_addr_amount(input)
            .ok_or_else(|| PrecompileError::Other("invalid input".into()))?;

        let from = require_caller(msg_sender)?;

        Self::check_compliance(asset_id, &from)?;
        Self::check_compliance(asset_id, &to)?;

        let mut store = AssetStorage::new(JournalBackend);
        store
            .transfer(asset_id, from, to, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

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

        let total_gas = 5000u64 * recipients.len() as u64;
        Self::deduct(total_gas)?;

        let from = require_caller(msg_sender)?;
        Self::check_compliance(asset_id, &from)?;
        for to in &recipients {
            Self::check_compliance(asset_id, to)?;
        }

        let pairs: Vec<(Address, u128)> = recipients.into_iter().zip(amounts.into_iter()).collect();
        let mut store = AssetStorage::new(JournalBackend);
        store
            .batch_transfer(asset_id, from, &pairs)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

        ok_empty()
    }

    fn approve(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        Self::deduct(3000)?;
        let (asset_id, spender, amount) = Self::decode_asset_addr_amount(input)
            .ok_or_else(|| PrecompileError::Other("invalid input".into()))?;

        let owner = require_caller(msg_sender)?;

        let mut store = AssetStorage::new(JournalBackend);
        store.approve(asset_id, owner, spender, amount);

        ok_empty()
    }

    fn transfer_from(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        Self::deduct(6000)?;
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

        let mut store = AssetStorage::new(JournalBackend);
        store
            .transfer_from(asset_id, spender, from, to, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

        ok_empty()
    }

    fn mint(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        Self::deduct(10000)?;
        let (asset_id, to, amount) = Self::decode_asset_addr_amount(input)
            .ok_or_else(|| PrecompileError::Other("invalid input".into()))?;

        let caller = require_caller(msg_sender)?;

        let mut store = AssetStorage::new(JournalBackend);
        store
            .mint(asset_id, caller, to, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

        ok_empty()
    }

    fn burn(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        Self::deduct(8000)?;
        let (asset_id, from, amount) = Self::decode_asset_addr_amount(input)
            .ok_or_else(|| PrecompileError::Other("invalid input".into()))?;

        let caller = require_caller(msg_sender)?;

        let mut store = AssetStorage::new(JournalBackend);
        store
            .burn(asset_id, caller, from, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

        ok_empty()
    }

    fn register(&self, input: &[u8], msg_sender: Address) -> PrecompileResult {
        Self::deduct(50000)?;
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

        let mut store = AssetStorage::new(JournalBackend);
        let asset_id = store
            .register(&symbol, &name, decimals, max_supply, caller)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

        let output = PrecompileOutput::new(0, encode_u64(asset_id).to_vec().into());
        Ok(call_precompiles::storage::fill_precompile_output(output))
    }
}

impl call_precompiles::StatefulPrecompile for AssetPrecompile {
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
            &[0x2b, 0x7d, 0x14, 0x80] => self.mint(calldata, msg_sender),
            &[0x73, 0x71, 0x28, 0x63] => self.burn(calldata, msg_sender),
            &[0x48, 0x4a, 0x57, 0x3d] => self.register(calldata, msg_sender),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompiles::storage::HashMapStorageProvider;
    use call_precompiles::{slot_balance, u128_to_u256, StatefulPrecompile};
    use call_primitives::Address;

    #[test]
    fn test_asset_precompile_get_balance() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::repeat_byte(0xAB);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, addr),
                u128_to_u256(5000),
            );

            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&selector::GET_BALANCE);
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
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let to = Address::repeat_byte(0xCD);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, from),
                u128_to_u256(1000),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&selector::TRANSFER);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(to.as_slice());
            input[84..100].copy_from_slice(&500u128.to_be_bytes());

            let mut precompile = AssetPrecompile;
            let result = precompile.call(&input, from);
            assert!(result.is_ok(), "transfer failed: {:?}", result.err());

            let mut store = AssetStorage::new(JournalBackend);
            assert_eq!(store.read_balance(1, from), 500);
            assert_eq!(store.read_balance(1, to), 500);
        });
    }

    #[test]
    fn test_asset_precompile_mint_and_burn() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            // Register
            let mut input = vec![0u8; 4 + 4 * 32 + 64 + 64];
            input[0..4].copy_from_slice(&selector::REGISTER);
            input[4 + 24..4 + 32].copy_from_slice(&128u64.to_be_bytes());
            let name_offset = 128 + 64;
            input[36 + 24..36 + 32].copy_from_slice(&(name_offset as u64).to_be_bytes());
            input[68 + 31] = 18;
            input[100 + 16..100 + 32].copy_from_slice(&10000u128.to_be_bytes());
            let sym_abs = 4 + 128;
            input[sym_abs + 24..sym_abs + 32].copy_from_slice(&4u64.to_be_bytes());
            input[sym_abs + 32..sym_abs + 36].copy_from_slice(b"GOLD");
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
            input[0..4].copy_from_slice(&selector::MINT);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&500u128.to_be_bytes());

            let result = precompile.call(&input, issuer);
            assert!(result.is_ok(), "mint failed: {:?}", result.err());

            let mut store = AssetStorage::new(JournalBackend);
            assert_eq!(store.read_balance(asset_id, recipient), 500);
            assert_eq!(store.read_meta(asset_id).supply, 500);

            // Mint to issuer
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&selector::MINT);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(issuer.as_slice());
            input[84..100].copy_from_slice(&400u128.to_be_bytes());
            let result = precompile.call(&input, issuer);
            assert!(result.is_ok());

            // Burn from issuer
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&selector::BURN);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(issuer.as_slice());
            input[84..100].copy_from_slice(&200u128.to_be_bytes());

            let result = precompile.call(&input, issuer);
            assert!(result.is_ok(), "burn failed: {:?}", result.err());

            assert_eq!(store.read_balance(asset_id, issuer), 200);
            assert_eq!(store.read_meta(asset_id).supply, 700);
        });
    }

    #[test]
    fn test_asset_precompile_approve_and_transfer_from() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let owner = Address::repeat_byte(0xAB);
        let spender = Address::repeat_byte(0xEF);
        let recipient = Address::repeat_byte(0xCD);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, owner),
                u128_to_u256(1000),
            );

            let mut precompile = AssetPrecompile;

            // Approve
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&selector::APPROVE);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(spender.as_slice());
            input[84..100].copy_from_slice(&100u128.to_be_bytes());

            let result = precompile.call(&input, owner);
            assert!(result.is_ok(), "approve failed: {:?}", result.err());

            // TransferFrom
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&selector::TRANSFER_FROM);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(owner.as_slice());
            input[80..100].copy_from_slice(recipient.as_slice());
            input[116..132].copy_from_slice(&50u128.to_be_bytes());

            let result = precompile.call(&input, spender);
            assert!(result.is_ok(), "transfer_from failed: {:?}", result.err());

            let mut store = AssetStorage::new(JournalBackend);
            assert_eq!(store.read_balance(1, owner), 950);
            assert_eq!(store.read_balance(1, recipient), 50);
            assert_eq!(
                store.read_allowance(1, owner, spender),
                50
            );
        });
    }
}
