//! Switch precompile entry point (0x207).
//!
//! Bidirectional bridge between protocol balance and EVM wrapped tokens:
//! - switchToEvm: protocol balance -> EVM ERC-20 / native balance
//! - switchToProtocol: EVM ERC-20 / native balance -> protocol balance
//!
//! Protocol balances live in ASSET_ADDRESS (0x201) storage slots.
//! EVM-side mutations happen through StorageProvider:
//!   - CALL (asset_id = 1): native EVM balance via balance_add / balance_sub
//!   - Other assets: ERC-20 storage writes (totalSupply slot 3, balanceOf slot keccak256(addr,4))

use alloy_sol_types::{sol, SolCall};
use call_precompiles::{
    dispatch, journal_backend::JournalBackend, require_caller,
    slot_asset_meta, slot_balance, slot_evm_contract, storage::StorageCtx, u128_to_u256,
    u256_to_u128, u256_to_address, ASSET_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;
use revm_precompile::{PrecompileError, PrecompileResult};

pub const SWITCH_ADDRESS: Address =
    alloy_primitives::address!("0000000000000000000000000000000000000207");

// ── Error type ────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SwitchError {
    AssetNotActive,
    EvmContractNotRegistered,
    InsufficientProtocolBalance,
    InsufficientEvmBalance,
    ProtocolBalanceOverflow,
    ProtocolSupplyOverflow,
    ProtocolSupplyUnderflow,
    Erc20BalanceOverflow,
    Erc20TotalSupplyOverflow,
    Erc20TotalSupplyUnderflow,
    Erc20BalanceUnderflow,
    AmountMustBePositive,
    ToCannotBeZero,
}

impl std::fmt::Display for SwitchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwitchError::AssetNotActive => write!(f, "asset not active"),
            SwitchError::EvmContractNotRegistered => write!(f, "EVM contract not registered for asset"),
            SwitchError::InsufficientProtocolBalance => write!(f, "insufficient protocol balance"),
            SwitchError::InsufficientEvmBalance => write!(f, "insufficient EVM balance"),
            SwitchError::ProtocolBalanceOverflow => write!(f, "protocol balance overflow"),
            SwitchError::ProtocolSupplyOverflow => write!(f, "protocol supply overflow"),
            SwitchError::ProtocolSupplyUnderflow => write!(f, "protocol supply underflow"),
            SwitchError::Erc20BalanceOverflow => write!(f, "ERC-20 balance overflow"),
            SwitchError::Erc20TotalSupplyOverflow => write!(f, "ERC-20 totalSupply overflow"),
            SwitchError::Erc20TotalSupplyUnderflow => write!(f, "ERC-20 totalSupply underflow"),
            SwitchError::Erc20BalanceUnderflow => write!(f, "insufficient ERC-20 balance"),
            SwitchError::AmountMustBePositive => write!(f, "amount must be > 0"),
            SwitchError::ToCannotBeZero => write!(f, "to cannot be zero address"),
        }
    }
}

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

// ── SwitchStorage ─────────────────────────────────────────────────────

pub struct SwitchStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> SwitchStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    // ── Protocol balance helpers ────────────────────────────────────

    fn load_protocol_bal(&self, asset_id: u64, addr: Address) -> u128 {
        u256_to_u128(self.backend.load(ASSET_ADDRESS, slot_balance(asset_id, addr)))
    }

    fn save_protocol_bal(&mut self, asset_id: u64, addr: Address, amount: u128) {
        self.backend
            .store(ASSET_ADDRESS, slot_balance(asset_id, addr), u128_to_u256(amount));
    }

    fn add_protocol_bal(&mut self, asset_id: u64, addr: Address, amount: u128) -> Result<(), SwitchError> {
        let bal = self
            .load_protocol_bal(asset_id, addr)
            .checked_add(amount)
            .ok_or(SwitchError::ProtocolBalanceOverflow)?;
        self.save_protocol_bal(asset_id, addr, bal);
        Ok(())
    }

    fn sub_protocol_bal(&mut self, asset_id: u64, addr: Address, amount: u128) -> Result<(), SwitchError> {
        let bal = self
            .load_protocol_bal(asset_id, addr)
            .checked_sub(amount)
            .ok_or(SwitchError::InsufficientProtocolBalance)?;
        self.save_protocol_bal(asset_id, addr, bal);
        Ok(())
    }

    // ── Protocol supply helpers ─────────────────────────────────────

    fn load_protocol_supply(&self, asset_id: u64) -> u128 {
        u256_to_u128(self.backend.load(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply")))
    }

    fn save_protocol_supply(&mut self, asset_id: u64, amount: u128) {
        self.backend
            .store(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"), u128_to_u256(amount));
    }

    // ── EVM contract lookup ─────────────────────────────────────────

    fn read_evm_contract(&self, asset_id: u64) -> Result<Address, SwitchError> {
        let addr = u256_to_address(self.backend.load(ASSET_ADDRESS, slot_evm_contract(asset_id)));
        if addr == Address::ZERO {
            return Err(SwitchError::EvmContractNotRegistered);
        }
        Ok(addr)
    }

    fn check_asset_active(&self, asset_id: u64) -> Result<(), SwitchError> {
        let status = self
            .backend
            .load(ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
            .to_be_bytes::<32>()[31];
        if status != 0 {
            return Err(SwitchError::AssetNotActive);
        }
        Ok(())
    }

    // ── ERC-20 mint/burn helpers ────────────────────────────────────

    fn erc20_mint(&mut self, contract: Address, to: Address, amount: u128) -> Result<(), SwitchError> {
        let total_supply = u256_to_u128(self.backend.load(contract, ERC20_TOTAL_SUPPLY_SLOT));
        let total_supply = total_supply
            .checked_add(amount)
            .ok_or(SwitchError::Erc20TotalSupplyOverflow)?;
        self.backend
            .store(contract, ERC20_TOTAL_SUPPLY_SLOT, u128_to_u256(total_supply));

        let balance_slot = erc20_balance_of_slot(to);
        let to_balance = u256_to_u128(self.backend.load(contract, balance_slot));
        let to_balance = to_balance
            .checked_add(amount)
            .ok_or(SwitchError::Erc20BalanceOverflow)?;
        self.backend
            .store(contract, balance_slot, u128_to_u256(to_balance));
        Ok(())
    }

    fn erc20_burn(&mut self, contract: Address, from: Address, amount: u128) -> Result<(), SwitchError> {
        let total_supply = u256_to_u128(self.backend.load(contract, ERC20_TOTAL_SUPPLY_SLOT));
        let total_supply = total_supply
            .checked_sub(amount)
            .ok_or(SwitchError::Erc20TotalSupplyUnderflow)?;
        self.backend
            .store(contract, ERC20_TOTAL_SUPPLY_SLOT, u128_to_u256(total_supply));

        let balance_slot = erc20_balance_of_slot(from);
        let from_balance = u256_to_u128(self.backend.load(contract, balance_slot));
        let from_balance = from_balance
            .checked_sub(amount)
            .ok_or(SwitchError::Erc20BalanceUnderflow)?;
        self.backend
            .store(contract, balance_slot, u128_to_u256(from_balance));
        Ok(())
    }

    // ── Business logic ──────────────────────────────────────────────

    pub fn switch_to_evm(
        &mut self,
        asset_id: u64,
        to: Address,
        amount: u128,
        sender: Address,
    ) -> Result<(), SwitchError> {
        if amount == 0 {
            return Err(SwitchError::AmountMustBePositive);
        }
        if to == Address::ZERO {
            return Err(SwitchError::ToCannotBeZero);
        }
        self.check_asset_active(asset_id)?;

        // 1. Deduct protocol balance from sender
        self.sub_protocol_bal(asset_id, sender, amount)?;

        // 2. Credit EVM side
        if asset_id == 1 {
            // CALL: add native EVM balance
            StorageCtx::balance_add(to, U256::from(amount))
                .ok_or(SwitchError::InsufficientEvmBalance)?;
        } else {
            // ERC-20: mint (increase totalSupply and balanceOf[to])
            let contract = self.read_evm_contract(asset_id)?;
            self.erc20_mint(contract, to, amount)?;
        }

        // 3. Update protocol-side supply tracking for non-CALL assets
        if asset_id != 1 {
            let supply = self
                .load_protocol_supply(asset_id)
                .checked_sub(amount)
                .ok_or(SwitchError::ProtocolSupplyUnderflow)?;
            self.save_protocol_supply(asset_id, supply);
        }

        Ok(())
    }

    pub fn switch_to_protocol(
        &mut self,
        asset_id: u64,
        to: Address,
        amount: u128,
        sender: Address,
    ) -> Result<(), SwitchError> {
        if amount == 0 {
            return Err(SwitchError::AmountMustBePositive);
        }
        if to == Address::ZERO {
            return Err(SwitchError::ToCannotBeZero);
        }
        self.check_asset_active(asset_id)?;

        // 1. Deduct EVM side
        if asset_id == 1 {
            // CALL: subtract native EVM balance from sender
            StorageCtx::balance_sub(sender, U256::from(amount))
                .ok_or(SwitchError::InsufficientEvmBalance)?;
        } else {
            // ERC-20: burn (decrease totalSupply and balanceOf[sender])
            let contract = self.read_evm_contract(asset_id)?;
            self.erc20_burn(contract, sender, amount)?;
        }

        // 2. Credit protocol balance to `to`
        self.add_protocol_bal(asset_id, to, amount)?;

        // 3. Update protocol-side supply tracking for non-CALL assets
        if asset_id != 1 {
            let supply = self
                .load_protocol_supply(asset_id)
                .checked_add(amount)
                .ok_or(SwitchError::ProtocolSupplyOverflow)?;
            self.save_protocol_supply(asset_id, supply);
        }

        Ok(())
    }
}

// ── sol! interface ────────────────────────────────────────────────────

sol! {
    interface IProtocolSwitch {
        function switchToEvm(uint64 assetId, address to, uint128 amount) external;
        function switchToProtocol(uint64 assetId, address to, uint128 amount) external;
    }
}

// ── SwitchPrecompile ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct SwitchPrecompile;

impl SwitchPrecompile {
    fn switch_to_evm(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolSwitch::switchToEvmCall, _>(calldata, 20000, |call| {
            let caller = require_caller(msg_sender)?;
            let mut store = SwitchStorage::new(JournalBackend);
            store
                .switch_to_evm(call.assetId, call.to, call.amount, caller)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }

    fn switch_to_protocol(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolSwitch::switchToProtocolCall, _>(calldata, 20000, |call| {
            let caller = require_caller(msg_sender)?;
            let mut store = SwitchStorage::new(JournalBackend);
            store
                .switch_to_protocol(call.assetId, call.to, call.amount, caller)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }
}

impl call_precompiles::StatefulPrecompile for SwitchPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let selector: [u8; 4] = calldata[..4].try_into().unwrap();
        match selector {
            IProtocolSwitch::switchToEvmCall::SELECTOR => self.switch_to_evm(calldata, msg_sender),
            IProtocolSwitch::switchToProtocolCall::SELECTOR => {
                self.switch_to_protocol(calldata, msg_sender)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompiles::{
        address_to_u256, slot_asset_meta, u128_to_u256, u256_to_u128, StatefulPrecompile,
    };
    use call_precompiles::storage::HashMapStorageProvider;

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
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            // Seed protocol balance for sender
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, sender),
                u128_to_u256(1000),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&500u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "switchToEvm failed: {:?}", result.err());

            // Protocol balance deducted
            let sender_bal = call_precompiles::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, sender))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(sender_bal, 500);

            // Native EVM balance credited
            let evm_bal = call_precompiles::storage::StorageCtx::balance_get(recipient);
            assert_eq!(evm_bal, Some(U256::from(500)));
        });
    }

    #[test]
    fn test_switch_to_protocol_call() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            // Seed native balance for sender
            call_precompiles::storage::StorageCtx::balance_add(sender, U256::from(800)).unwrap();

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&IProtocolSwitch::switchToProtocolCall::SELECTOR);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&300u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "switchToProtocol failed: {:?}", result.err());

            // Native EVM balance deducted
            let evm_bal = call_precompiles::storage::StorageCtx::balance_get(sender);
            assert_eq!(evm_bal, Some(U256::from(500)));

            // Protocol balance credited to recipient
            let recipient_bal = call_precompiles::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, recipient))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(recipient_bal, 300);
        });
    }

    #[test]
    fn test_switch_to_evm_erc20() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);
        let contract = addr(0xAA);
        let asset_id = 2u64;

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            // Seed protocol balance
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, sender),
                u128_to_u256(1000),
            );
            // Register asset as active
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            );
            // Register EVM contract address
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_evm_contract(asset_id),
                address_to_u256(contract),
            );
            // Seed supply tracking
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"supply"),
                u128_to_u256(1000),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&400u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "switchToEvm ERC-20 failed: {:?}", result.err());

            // Protocol balance deducted
            let sender_bal = call_precompiles::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(asset_id, sender))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(sender_bal, 600);

            // ERC-20 totalSupply increased
            let total_supply = call_precompiles::storage::StorageCtx::sload(contract, ERC20_TOTAL_SUPPLY_SLOT)
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(total_supply, 400);

            // ERC-20 balanceOf recipient increased
            let recipient_bal = call_precompiles::storage::StorageCtx::sload(contract, erc20_balance_of_slot(recipient))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(recipient_bal, 400);

            // Protocol supply tracking decreased
            let protocol_supply = call_precompiles::storage::StorageCtx::sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(protocol_supply, 600);
        });
    }

    #[test]
    fn test_switch_to_protocol_erc20() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);
        let contract = addr(0xAA);
        let asset_id = 3u64;

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            // Register asset as active
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            );
            // Register EVM contract address
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_evm_contract(asset_id),
                address_to_u256(contract),
            );
            // Seed ERC-20 totalSupply
            call_precompiles::storage::StorageCtx::sstore(
                contract,
                ERC20_TOTAL_SUPPLY_SLOT,
                u128_to_u256(500),
            );
            // Seed ERC-20 balance for sender
            call_precompiles::storage::StorageCtx::sstore(
                contract,
                erc20_balance_of_slot(sender),
                u128_to_u256(500),
            );
            // Seed protocol supply tracking
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"supply"),
                u128_to_u256(0),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&IProtocolSwitch::switchToProtocolCall::SELECTOR);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(recipient.as_slice());
            input[84..100].copy_from_slice(&200u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_ok(), "switchToProtocol ERC-20 failed: {:?}", result.err());

            // ERC-20 totalSupply decreased
            let total_supply = call_precompiles::storage::StorageCtx::sload(contract, ERC20_TOTAL_SUPPLY_SLOT)
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(total_supply, 300);

            // ERC-20 balanceOf sender decreased
            let sender_bal = call_precompiles::storage::StorageCtx::sload(contract, erc20_balance_of_slot(sender))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(sender_bal, 300);

            // Protocol balance credited to recipient
            let recipient_bal = call_precompiles::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(asset_id, recipient))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(recipient_bal, 200);

            // Protocol supply tracking increased
            let protocol_supply = call_precompiles::storage::StorageCtx::sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(protocol_supply, 200);
        });
    }

    #[test]
    fn test_switch_to_evm_insufficient_protocol_balance_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            // Only 100 protocol balance
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, sender),
                u128_to_u256(100),
            );

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(sender.as_slice());
            input[84..100].copy_from_slice(&200u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_err(), "should fail due to insufficient balance");

            // Balance unchanged
            let bal = call_precompiles::storage::StorageCtx::sload(ASSET_ADDRESS, slot_balance(1, sender))
                .map(u256_to_u128)
                .unwrap_or(0);
            assert_eq!(bal, 100);
        });
    }

    #[test]
    fn test_switch_to_evm_erc20_no_contract_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let asset_id = 5u64;

        call_precompiles::storage::StorageCtx::enter(&mut provider, || {
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, sender),
                u128_to_u256(1000),
            );
            call_precompiles::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            );
            // evm_contract NOT set

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(sender.as_slice());
            input[84..100].copy_from_slice(&100u128.to_be_bytes());

            let mut precompile = SwitchPrecompile;
            let result = precompile.call(&input, sender);
            assert!(result.is_err(), "should fail when EVM contract not registered");
        });
    }
}
