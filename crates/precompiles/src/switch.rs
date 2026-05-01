//! Switch precompile at 0x207
//!
//! Bidirectional bridge between protocol balance and EVM wrapped tokens:
//! - switchToEvm: protocol balance -> EVM ERC-20 / native balance
//! - switchToProtocol: EVM ERC-20 / native balance -> protocol balance
//!
//! Protocol balances live in ASSET_ADDRESS (0x201) storage slots.
//! EVM-side mutations happen through StorageProvider:
//!   - CALL (asset_id = 1): native EVM balance via balance_add / balance_sub
//!   - Other assets: ERC-20 storage writes (totalSupply slot 3, balanceOf slot keccak256(addr,4))

use alloy_primitives::{address, Address, U256};
use revm_precompile::PrecompileError;

use crate::StatefulPrecompile;
use crate::storage::StorageCtx;
use crate::{
    decode_address, decode_u128, decode_u64, ok_empty, require_caller, slot_asset_meta,
    slot_balance, slot_evm_contract, u128_to_u256, u256_to_u128, ASSET_ADDRESS,
};

pub const SWITCH_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000207");

// ── ERC-20 storage layout helpers ─────────────────────────────────────

/// Solidity mapping slot: keccak256(abi.encodePacked(key, base_slot))
fn mapping_slot(key_bytes: &[u8; 32], base_slot: u64) -> U256 {
    let mut hasher = alloy_primitives::Keccak256::new();
    hasher.update(key_bytes);
    let mut base = [0u8; 32];
    base[24..32].copy_from_slice(&base_slot.to_be_bytes());
    hasher.update(base);
    U256::from_be_slice(hasher.finalize().as_slice())
}

/// Storage slot for `balanceOf[holder]` in WrappedToken (mapping base slot = 4).
fn erc20_balance_of_slot(holder: Address) -> U256 {
    let mut padded = [0u8; 32];
    padded[12..32].copy_from_slice(holder.as_slice());
    mapping_slot(&padded, 4)
}

/// Storage slot for `totalSupply` in WrappedToken (slot 3).
const ERC20_TOTAL_SUPPLY_SLOT: U256 = U256::from_limbs([3, 0, 0, 0]);

// ── Protocol balance helpers ──────────────────────────────────────────

fn load_protocol_bal(asset_id: u64, addr: Address) -> u128 {
    StorageCtx::sload(ASSET_ADDRESS, slot_balance(asset_id, addr))
        .map(u256_to_u128)
        .unwrap_or(0)
}

fn save_protocol_bal(asset_id: u64, addr: Address, amount: u128) {
    StorageCtx::sstore(ASSET_ADDRESS, slot_balance(asset_id, addr), u128_to_u256(amount));
}

fn add_protocol_bal(asset_id: u64, addr: Address, amount: u128) -> Result<(), PrecompileError> {
    let bal = load_protocol_bal(asset_id, addr)
        .checked_add(amount)
        .ok_or_else(|| PrecompileError::Other("protocol balance overflow".into()))?;
    save_protocol_bal(asset_id, addr, bal);
    Ok(())
}

fn sub_protocol_bal(asset_id: u64, addr: Address, amount: u128) -> Result<(), PrecompileError> {
    let bal = load_protocol_bal(asset_id, addr)
        .checked_sub(amount)
        .ok_or_else(|| PrecompileError::Other("insufficient protocol balance".into()))?;
    save_protocol_bal(asset_id, addr, bal);
    Ok(())
}

// ── Protocol supply helpers ───────────────────────────────────────────

fn load_protocol_supply(asset_id: u64) -> u128 {
    StorageCtx::sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"))
        .map(u256_to_u128)
        .unwrap_or(0)
}

fn save_protocol_supply(asset_id: u64, amount: u128) {
    StorageCtx::sstore(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(amount));
}

// ── ERC-20 mint/burn helpers ──────────────────────────────────────────

fn erc20_mint(contract: Address, to: Address, amount: u128) -> Result<(), PrecompileError> {
    let total_supply = StorageCtx::sload(contract, ERC20_TOTAL_SUPPLY_SLOT)
        .map(u256_to_u128)
        .unwrap_or(0);
    let total_supply = total_supply
        .checked_add(amount)
        .ok_or_else(|| PrecompileError::Other("totalSupply overflow".into()))?;
    StorageCtx::sstore(contract, ERC20_TOTAL_SUPPLY_SLOT, u128_to_u256(total_supply));

    let balance_slot = erc20_balance_of_slot(to);
    let to_balance = StorageCtx::sload(contract, balance_slot)
        .map(u256_to_u128)
        .unwrap_or(0);
    let to_balance = to_balance
        .checked_add(amount)
        .ok_or_else(|| PrecompileError::Other("ERC-20 balance overflow".into()))?;
    StorageCtx::sstore(contract, balance_slot, u128_to_u256(to_balance));
    Ok(())
}

fn erc20_burn(contract: Address, from: Address, amount: u128) -> Result<(), PrecompileError> {
    let total_supply = StorageCtx::sload(contract, ERC20_TOTAL_SUPPLY_SLOT)
        .map(u256_to_u128)
        .unwrap_or(0);
    let total_supply = total_supply
        .checked_sub(amount)
        .ok_or_else(|| PrecompileError::Other("totalSupply underflow".into()))?;
    StorageCtx::sstore(contract, ERC20_TOTAL_SUPPLY_SLOT, u128_to_u256(total_supply));

    let balance_slot = erc20_balance_of_slot(from);
    let from_balance = StorageCtx::sload(contract, balance_slot)
        .map(u256_to_u128)
        .unwrap_or(0);
    let from_balance = from_balance
        .checked_sub(amount)
        .ok_or_else(|| PrecompileError::Other("insufficient ERC-20 balance".into()))?;
    StorageCtx::sstore(contract, balance_slot, u128_to_u256(from_balance));
    Ok(())
}

// ── SwitchPrecompile ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct SwitchPrecompile;

impl SwitchPrecompile {
    fn read_evm_contract(asset_id: u64) -> Result<Address, PrecompileError> {
        let addr = StorageCtx::sload(ASSET_ADDRESS, slot_evm_contract(asset_id))
            .and_then(|v| {
                let bytes = v.to_be_bytes::<32>();
                // non-zero in low 20 bytes
                if bytes[12..32].iter().any(|b| *b != 0) {
                    Some(Address::from_slice(&bytes[12..32]))
                } else {
                    None
                }
            })
            .ok_or_else(|| PrecompileError::Other("EVM contract not registered for asset".into()))?;
        Ok(addr)
    }

    fn check_asset_active(asset_id: u64) -> Result<(), PrecompileError> {
        let status = StorageCtx::sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        if status != 0 {
            return Err(PrecompileError::Other("asset not active".into()));
        }
        Ok(())
    }

    // switchToEvm(uint64 assetId, address to, uint128 amount) -> 0x4311f613
    fn switch_to_evm(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 20_000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;

        if input.len() < 100 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid asset_id".into()))?;
        let to = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid to address".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        let sender = require_caller(msg_sender)?;
        Self::check_asset_active(asset_id)?;

        let _guard = StorageCtx::checkpoint();

        // 1. Deduct protocol balance from sender
        sub_protocol_bal(asset_id, sender, amount)?;

        // 2. Credit EVM side
        if asset_id == 1 {
            // CALL: add native EVM balance
            StorageCtx::balance_add(to, U256::from(amount))
                .ok_or_else(|| PrecompileError::Other("native balance add failed".into()))?;
        } else {
            // ERC-20: mint (increase totalSupply and balanceOf[to])
            let contract = Self::read_evm_contract(asset_id)?;
            erc20_mint(contract, to, amount)?;
        }

        // 3. Update protocol-side supply tracking for non-CALL assets
        if asset_id != 1 {
            let supply = load_protocol_supply(asset_id)
                .checked_sub(amount)
                .ok_or_else(|| PrecompileError::Other("protocol supply underflow".into()))?;
            save_protocol_supply(asset_id, supply);
        }

        _guard.commit();
        ok_empty()
    }

    // switchToProtocol(uint64 assetId, address to, uint128 amount) -> 0xbd8d87d4
    fn switch_to_protocol(
        &self,
        input: &[u8],
        msg_sender: Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 20_000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;

        if input.len() < 100 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid asset_id".into()))?;
        let to = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid to address".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        let sender = require_caller(msg_sender)?;
        Self::check_asset_active(asset_id)?;

        let _guard = StorageCtx::checkpoint();

        // 1. Deduct EVM side
        if asset_id == 1 {
            // CALL: subtract native EVM balance from sender
            StorageCtx::balance_sub(sender, U256::from(amount))
                .ok_or_else(|| PrecompileError::Other("insufficient native balance".into()))?;
        } else {
            // ERC-20: burn (decrease totalSupply and balanceOf[sender])
            let contract = Self::read_evm_contract(asset_id)?;
            erc20_burn(contract, sender, amount)?;
        }

        // 2. Credit protocol balance to `to`
        add_protocol_bal(asset_id, to, amount)?;

        // 3. Update protocol-side supply tracking for non-CALL assets
        if asset_id != 1 {
            let supply = load_protocol_supply(asset_id)
                .checked_add(amount)
                .ok_or_else(|| PrecompileError::Other("protocol supply overflow".into()))?;
            save_protocol_supply(asset_id, supply);
        }

        _guard.commit();
        ok_empty()
    }
}

impl StatefulPrecompile for SwitchPrecompile {
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: alloy_primitives::Address,
    ) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("too short".into()));
        }
        let selector = [calldata[0], calldata[1], calldata[2], calldata[3]];
        match selector {
            [0x43, 0x11, 0xf6, 0x13] => self.switch_to_evm(calldata, msg_sender),
            [0xbd, 0x8d, 0x87, 0xd4] => self.switch_to_protocol(calldata, msg_sender),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{slot_asset_meta, u128_to_u256, ASSET_ADDRESS};

    fn addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_switch_address() {
        assert_eq!(
            SWITCH_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000207")
        );
    }

    #[test]
    fn test_switch_to_evm_call() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed protocol balance for sender
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, sender),
                u128_to_u256(1000),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x43, 0x11, 0xf6, 0x13]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&500u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "switchToEvm failed: {:?}", result.err());

            // Protocol balance deducted
            let sender_bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, sender))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(sender_bal, 500);

            // Native EVM balance credited
            let evm_bal = crate::storage::StorageCtx::balance_get(recipient);
            assert_eq!(evm_bal, Some(U256::from(500)));
        });
    }

    #[test]
    fn test_switch_to_protocol_call() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed native balance for sender
            crate::storage::StorageCtx::balance_add(sender, U256::from(800)).unwrap();
            // Seed protocol balance for recipient (not needed, just checking)

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0xbd, 0x8d, 0x87, 0xd4]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&300u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "switchToProtocol failed: {:?}", result.err());

            // Native EVM balance deducted
            let evm_bal = crate::storage::StorageCtx::balance_get(sender);
            assert_eq!(evm_bal, Some(U256::from(500)));

            // Protocol balance credited to recipient
            let recipient_bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, recipient))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(recipient_bal, 300);
        });
    }

    #[test]
    fn test_switch_to_evm_erc20() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);
        let contract = addr(0xAA);
        let asset_id = 2u64;

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed protocol balance
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, sender),
                u128_to_u256(1000),
            );
            // Register asset as active
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            );
            // Register EVM contract address
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_evm_contract(asset_id),
                crate::address_to_u256(contract),
            );
            // Seed supply tracking
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"supply"),
                u128_to_u256(1000),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x43, 0x11, 0xf6, 0x13]);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&400u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "switchToEvm ERC-20 failed: {:?}", result.err());

            // Protocol balance deducted
            let sender_bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(asset_id, sender))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(sender_bal, 600);

            // ERC-20 totalSupply increased
            let total_supply = crate::storage::StorageCtx::sload(contract, ERC20_TOTAL_SUPPLY_SLOT)
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(total_supply, 400);

            // ERC-20 balanceOf recipient increased
            let recipient_bal = crate::storage::StorageCtx::sload(contract, erc20_balance_of_slot(recipient))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(recipient_bal, 400);

            // Protocol supply tracking decreased
            let protocol_supply = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(protocol_supply, 600);
        });
    }

    #[test]
    fn test_switch_to_protocol_erc20() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);
        let contract = addr(0xAA);
        let asset_id = 3u64;

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Register asset as active
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            );
            // Register EVM contract address
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_evm_contract(asset_id),
                crate::address_to_u256(contract),
            );
            // Seed ERC-20 totalSupply
            crate::storage::StorageCtx::sstore(
                contract,
                ERC20_TOTAL_SUPPLY_SLOT,
                u128_to_u256(500),
            );
            // Seed ERC-20 balance for sender
            crate::storage::StorageCtx::sstore(
                contract,
                erc20_balance_of_slot(sender),
                u128_to_u256(500),
            );
            // Seed protocol supply tracking
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"supply"),
                u128_to_u256(0),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0xbd, 0x8d, 0x87, 0xd4]);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&200u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "switchToProtocol ERC-20 failed: {:?}", result.err());

            // ERC-20 totalSupply decreased
            let total_supply = crate::storage::StorageCtx::sload(contract, ERC20_TOTAL_SUPPLY_SLOT)
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(total_supply, 300);

            // ERC-20 balanceOf sender decreased
            let sender_bal = crate::storage::StorageCtx::sload(contract, erc20_balance_of_slot(sender))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(sender_bal, 300);

            // Protocol balance credited to recipient
            let recipient_bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(asset_id, recipient))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(recipient_bal, 200);

            // Protocol supply tracking increased
            let protocol_supply = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(protocol_supply, 200);
        });
    }

    #[test]
    fn test_switch_to_evm_insufficient_protocol_balance_fails() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Only 100 protocol balance
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, sender),
                u128_to_u256(100),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x43, 0x11, 0xf6, 0x13]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(sender.as_slice());
            input[84..100].copy_from_slice(&200u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_err(), "should fail due to insufficient balance");

            // Balance unchanged
            let bal = crate::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, sender))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(bal, 100);
        });
    }

    #[test]
    fn test_switch_to_evm_erc20_no_contract_fails() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let asset_id = 5u64;

        crate::storage::StorageCtx::enter(&mut provider, || {
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, sender),
                u128_to_u256(1000),
            );
            crate::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            );
            // evm_contract NOT set

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x43, 0x11, 0xf6, 0x13]);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(sender.as_slice());
            input[84..100].copy_from_slice(&100u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_err(), "should fail when EVM contract not registered");
        });
    }
}
