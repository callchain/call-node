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
use call_precompile::{
    dispatch, require_caller, slot_asset_meta, slot_balance, slot_erc20_balance_of_base,
    slot_erc20_total_supply, slot_evm_contract, storage::StorageProvider, u128_to_u256,
    u256_to_address, u256_to_u128, u256_to_u64, StorageRef, ASSET_ADDRESS,
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
    AssetHasNoErc20Bridge,
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
            SwitchError::AssetHasNoErc20Bridge => {
                write!(f, "asset has no ERC-20 bridge; use protocol-only operations")
            }
            SwitchError::EvmContractNotRegistered => {
                write!(f, "EVM contract not registered for asset")
            }
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

// ── SwitchStorage ─────────────────────────────────────────────────────

pub struct SwitchStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> SwitchStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    // ── Protocol balance helpers ────────────────────────────────────

    fn load_protocol_bal(&mut self, asset_id: u64, addr: Address) -> u128 {
        u256_to_u128(
            self.backend
                .load(ASSET_ADDRESS, slot_balance(asset_id, addr)),
        )
    }

    fn save_protocol_bal(&mut self, asset_id: u64, addr: Address, amount: u128) {
        self.backend.store(
            ASSET_ADDRESS,
            slot_balance(asset_id, addr),
            u128_to_u256(amount),
        );
    }

    fn add_protocol_bal(
        &mut self,
        asset_id: u64,
        addr: Address,
        amount: u128,
    ) -> Result<(), SwitchError> {
        let bal = self
            .load_protocol_bal(asset_id, addr)
            .checked_add(amount)
            .ok_or(SwitchError::ProtocolBalanceOverflow)?;
        self.save_protocol_bal(asset_id, addr, bal);
        Ok(())
    }

    fn sub_protocol_bal(
        &mut self,
        asset_id: u64,
        addr: Address,
        amount: u128,
    ) -> Result<(), SwitchError> {
        let bal = self
            .load_protocol_bal(asset_id, addr)
            .checked_sub(amount)
            .ok_or(SwitchError::InsufficientProtocolBalance)?;
        self.save_protocol_bal(asset_id, addr, bal);
        Ok(())
    }

    // ── Protocol supply helpers ─────────────────────────────────────

    fn load_protocol_supply(&mut self, asset_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply")),
        )
    }

    fn save_protocol_supply(&mut self, asset_id: u64, amount: u128) {
        self.backend.store(
            ASSET_ADDRESS,
            slot_asset_meta(asset_id, b"supply"),
            u128_to_u256(amount),
        );
    }

    // ── EVM contract lookup ─────────────────────────────────────────

    fn read_evm_contract(&mut self, asset_id: u64) -> Result<Address, SwitchError> {
        let addr = u256_to_address(
            self.backend
                .load(ASSET_ADDRESS, slot_evm_contract(asset_id)),
        );
        if addr == Address::ZERO {
            return Err(SwitchError::EvmContractNotRegistered);
        }
        Ok(addr)
    }

    fn check_asset_active(&mut self, asset_id: u64) -> Result<(), SwitchError> {
        let status = self
            .backend
            .load(ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
            .to_be_bytes::<32>()[31];
        if status != 0 {
            return Err(SwitchError::AssetNotActive);
        }
        Ok(())
    }

    fn check_has_erc20(&mut self, asset_id: u64) -> Result<(), SwitchError> {
        // CALL (asset_id == 1) is always allowed — it bridges as native EVM balance.
        if asset_id == call_protocol::CALL_ASSET_ID {
            return Ok(());
        }
        let has_erc20 = self
            .backend
            .load(ASSET_ADDRESS, slot_asset_meta(asset_id, b"has_erc20"))
            .to_be_bytes::<32>()[31];
        if has_erc20 != 1 {
            return Err(SwitchError::AssetHasNoErc20Bridge);
        }
        Ok(())
    }

    // ── ERC-20 storage layout (read from asset metadata, with defaults) ─

    /// Read the ERC-20 `balanceOf` mapping base slot for an asset from ASSET_ADDRESS metadata.
    /// Falls back to default 4 if not set.
    fn erc20_balance_of_base(&mut self, asset_id: u64) -> u64 {
        let slot = self
            .backend
            .load(ASSET_ADDRESS, slot_erc20_balance_of_base(asset_id));
        let val = u256_to_u64(slot);
        if val == 0 {
            4
        } else {
            val
        }
    }

    /// Read the ERC-20 `totalSupply` slot for an asset from ASSET_ADDRESS metadata.
    /// Falls back to default 3 if not set.
    fn erc20_total_supply_slot(&mut self, asset_id: u64) -> U256 {
        let slot = self
            .backend
            .load(ASSET_ADDRESS, slot_erc20_total_supply(asset_id));
        let val = u256_to_u64(slot);
        if val == 0 {
            U256::from(3)
        } else {
            slot
        }
    }

    /// Compute the storage slot for `balanceOf[holder]` using the asset's registered base slot.
    fn erc20_balance_of_slot(&mut self, asset_id: u64, holder: Address) -> U256 {
        let base = self.erc20_balance_of_base(asset_id);
        let mut padded = [0u8; 32];
        padded[12..32].copy_from_slice(holder.as_slice());
        mapping_slot(&padded, base)
    }

    // ── ERC-20 mint/burn helpers ────────────────────────────────────

    fn erc20_mint(
        &mut self,
        asset_id: u64,
        contract: Address,
        to: Address,
        amount: u128,
    ) -> Result<(), SwitchError> {
        let ts_slot = self.erc20_total_supply_slot(asset_id);
        let total_supply = u256_to_u128(self.backend.load(contract, ts_slot));
        let total_supply = total_supply
            .checked_add(amount)
            .ok_or(SwitchError::Erc20TotalSupplyOverflow)?;
        self.backend
            .store(contract, ts_slot, u128_to_u256(total_supply));

        let balance_slot = self.erc20_balance_of_slot(asset_id, to);
        let to_balance = u256_to_u128(self.backend.load(contract, balance_slot));
        let to_balance = to_balance
            .checked_add(amount)
            .ok_or(SwitchError::Erc20BalanceOverflow)?;
        self.backend
            .store(contract, balance_slot, u128_to_u256(to_balance));
        Ok(())
    }

    fn erc20_burn(
        &mut self,
        asset_id: u64,
        contract: Address,
        from: Address,
        amount: u128,
    ) -> Result<(), SwitchError> {
        let ts_slot = self.erc20_total_supply_slot(asset_id);
        let total_supply = u256_to_u128(self.backend.load(contract, ts_slot));
        let total_supply = total_supply
            .checked_sub(amount)
            .ok_or(SwitchError::Erc20TotalSupplyUnderflow)?;
        self.backend
            .store(contract, ts_slot, u128_to_u256(total_supply));

        let balance_slot = self.erc20_balance_of_slot(asset_id, from);
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
        storage: &mut dyn StorageProvider,
    ) -> Result<(), SwitchError> {
        if amount == 0 {
            return Err(SwitchError::AmountMustBePositive);
        }
        if to == Address::ZERO {
            return Err(SwitchError::ToCannotBeZero);
        }
        self.check_asset_active(asset_id)?;
        self.check_has_erc20(asset_id)?;

        // 1. Deduct protocol balance from sender
        self.sub_protocol_bal(asset_id, sender, amount)?;

        // 2. Credit EVM side
        if asset_id == call_protocol::CALL_ASSET_ID {
            // CALL: add native EVM balance
            storage
                .balance_add(to, U256::from(amount))
                .map_err(|_| SwitchError::InsufficientEvmBalance)?;
        } else {
            // ERC-20: mint (increase totalSupply and balanceOf[to])
            let contract = self.read_evm_contract(asset_id)?;
            self.erc20_mint(asset_id, contract, to, amount)?;
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
        storage: &mut dyn StorageProvider,
    ) -> Result<(), SwitchError> {
        if amount == 0 {
            return Err(SwitchError::AmountMustBePositive);
        }
        if to == Address::ZERO {
            return Err(SwitchError::ToCannotBeZero);
        }
        self.check_asset_active(asset_id)?;
        self.check_has_erc20(asset_id)?;

        // 1. Deduct EVM side
        if asset_id == call_protocol::CALL_ASSET_ID {
            // CALL: subtract native EVM balance from sender
            storage
                .balance_sub(sender, U256::from(amount))
                .map_err(|_| SwitchError::InsufficientEvmBalance)?;
        } else {
            // ERC-20: burn (decrease totalSupply and balanceOf[sender])
            let contract = self.read_evm_contract(asset_id)?;
            self.erc20_burn(asset_id, contract, sender, amount)?;
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
    fn switch_to_evm(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolSwitch::switchToEvmCall, _>(
            calldata,
            20000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = SwitchStorage::new(sr);
                store
                    .switch_to_evm(call.assetId, call.to, call.amount, caller, storage)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn switch_to_protocol(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolSwitch::switchToProtocolCall, _>(
            calldata,
            20000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = SwitchStorage::new(sr);
                store
                    .switch_to_protocol(call.assetId, call.to, call.amount, caller, storage)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for SwitchPrecompile {
    #[allow(clippy::expect_used)]
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let selector: [u8; 4] = calldata[..4]
            .try_into()
            .expect("slice length checked above");
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolSwitch::switchToEvmCall::SELECTOR => {
                self.switch_to_evm(calldata, msg_sender, storage, sr)
            }
            IProtocolSwitch::switchToProtocolCall::SELECTOR => {
                self.switch_to_protocol(calldata, msg_sender, storage, sr)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{
        address_to_u256, slot_asset_meta, u128_to_u256, u256_to_u128, StatefulPrecompile,
        StorageRef,
    };

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

        // Seed protocol balance for sender
        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, sender), u128_to_u256(1000))
            .unwrap();

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&500u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "switchToEvm failed: {:?}", result.err());

        // Protocol balance deducted
        let sender_bal = provider
            .sload(ASSET_ADDRESS, slot_balance(1, sender))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(sender_bal, 500);

        // Native EVM balance credited
        let evm_bal = provider.balance_get(recipient).ok();
        assert_eq!(evm_bal, Some(U256::from(500)));
    }

    #[test]
    fn test_switch_to_protocol_call() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);

        // Seed native balance for sender
        provider.balance_add(sender, U256::from(800)).unwrap();

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToProtocolCall::SELECTOR);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&300u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(
            result.is_ok(),
            "switchToProtocol failed: {:?}",
            result.err()
        );

        // Native EVM balance deducted
        let evm_bal = provider.balance_get(sender).ok();
        assert_eq!(evm_bal, Some(U256::from(500)));

        // Protocol balance credited to recipient
        let recipient_bal = provider
            .sload(ASSET_ADDRESS, slot_balance(1, recipient))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(recipient_bal, 300);
    }

    #[test]
    fn test_switch_to_evm_erc20() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);
        let contract = addr(0xAA);
        let asset_id = 2u64;

        // Seed protocol balance
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, sender),
                u128_to_u256(1000),
            )
            .unwrap();
        // Register asset as active
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            )
            .unwrap();
        // Register EVM contract address
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_evm_contract(asset_id),
                address_to_u256(contract),
            )
            .unwrap();
        // Mark asset as ERC-20 backed (has_erc20 = 1)
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"has_erc20"),
                U256::from(1),
            )
            .unwrap();
        // Seed supply tracking
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"supply"),
                u128_to_u256(1000),
            )
            .unwrap();

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&400u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(
            result.is_ok(),
            "switchToEvm ERC-20 failed: {:?}",
            result.err()
        );

        // Protocol balance deducted
        let sender_bal = provider
            .sload(ASSET_ADDRESS, slot_balance(asset_id, sender))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(sender_bal, 600);

        // ERC-20 totalSupply increased (default slot 3)
        let total_supply = provider
            .sload(contract, U256::from(3))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(total_supply, 400);

        // ERC-20 balanceOf recipient increased (default base slot 4)
        let mut padded = [0u8; 32];
        padded[12..32].copy_from_slice(recipient.as_slice());
        let recipient_bal = provider
            .sload(contract, mapping_slot(&padded, 4))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(recipient_bal, 400);

        // Protocol supply tracking decreased
        let protocol_supply = provider
            .sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(protocol_supply, 600);
    }

    #[test]
    fn test_switch_to_protocol_erc20() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);
        let contract = addr(0xAA);
        let asset_id = 3u64;

        // Register asset as active
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            )
            .unwrap();
        // Register EVM contract address
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_evm_contract(asset_id),
                address_to_u256(contract),
            )
            .unwrap();
        // Mark asset as ERC-20 backed (has_erc20 = 1)
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"has_erc20"),
                U256::from(1),
            )
            .unwrap();
        // Seed ERC-20 totalSupply (default slot 3)
        provider
            .sstore(contract, U256::from(3), u128_to_u256(500))
            .unwrap();
        // Seed ERC-20 balance for sender (default base slot 4)
        let mut padded = [0u8; 32];
        padded[12..32].copy_from_slice(sender.as_slice());
        provider
            .sstore(contract, mapping_slot(&padded, 4), u128_to_u256(500))
            .unwrap();
        // Seed protocol supply tracking
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"supply"),
                u128_to_u256(0),
            )
            .unwrap();

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToProtocolCall::SELECTOR);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&200u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(
            result.is_ok(),
            "switchToProtocol ERC-20 failed: {:?}",
            result.err()
        );

        // ERC-20 totalSupply decreased (default slot 3)
        let total_supply = provider
            .sload(contract, U256::from(3))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(total_supply, 300);

        // ERC-20 balanceOf sender decreased (default base slot 4)
        let mut padded = [0u8; 32];
        padded[12..32].copy_from_slice(sender.as_slice());
        let sender_bal = provider
            .sload(contract, mapping_slot(&padded, 4))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(sender_bal, 300);

        // Protocol balance credited to recipient
        let recipient_bal = provider
            .sload(ASSET_ADDRESS, slot_balance(asset_id, recipient))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(recipient_bal, 200);

        // Protocol supply tracking increased
        let protocol_supply = provider
            .sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"supply"))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(protocol_supply, 200);
    }

    #[test]
    fn test_switch_to_evm_insufficient_protocol_balance_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);

        // Only 100 protocol balance
        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, sender), u128_to_u256(100))
            .unwrap();

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(sender.as_slice());
        input[84..100].copy_from_slice(&200u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "should fail due to insufficient balance");

        // Balance unchanged
        let bal = provider
            .sload(ASSET_ADDRESS, slot_balance(1, sender))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(bal, 100);
    }

    #[test]
    fn test_switch_to_evm_erc20_no_contract_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let asset_id = 5u64;

        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, sender),
                u128_to_u256(1000),
            )
            .unwrap();
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            )
            .unwrap();
        // evm_contract NOT set

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(sender.as_slice());
        input[84..100].copy_from_slice(&100u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(
            result.is_err(),
            "should fail when EVM contract not registered"
        );
    }

    // ── Lib-layer SwitchStorage tests ─────────────────────────────────

    #[test]
    fn test_switch_storage_protocol_bal_add_sub() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = addr(0x33);
        let asset_id = 7u64;

        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, addr),
                u128_to_u256(1000),
            )
            .unwrap();

        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));

        store.sub_protocol_bal(asset_id, addr, 300).unwrap();
        assert_eq!(store.load_protocol_bal(asset_id, addr), 700);

        store.add_protocol_bal(asset_id, addr, 200).unwrap();
        assert_eq!(store.load_protocol_bal(asset_id, addr), 900);
    }

    #[test]
    fn test_switch_storage_erc20_mint_burn() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let contract = addr(0xAA);
        let holder = addr(0xBB);
        let asset_id = 2u64;

        // Default totalSupply slot = 3, balanceOf base = 4
        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));

        store.erc20_mint(asset_id, contract, holder, 500).unwrap();

        let ts = provider
            .sload(contract, U256::from(3))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(ts, 500);

        let mut padded = [0u8; 32];
        padded[12..32].copy_from_slice(holder.as_slice());
        let bal = provider
            .sload(contract, mapping_slot(&padded, 4))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(bal, 500);

        store.erc20_burn(asset_id, contract, holder, 200).unwrap();

        let ts2 = provider
            .sload(contract, U256::from(3))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(ts2, 300);
    }

    #[test]
    fn test_switch_storage_check_asset_active() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let asset_id = 5u64;

        // status = 0 means active
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            )
            .unwrap();

        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));
        assert!(store.check_asset_active(asset_id).is_ok());

        // status != 0 means inactive
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(1),
            )
            .unwrap();
        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));
        assert!(matches!(
            store.check_asset_active(asset_id),
            Err(SwitchError::AssetNotActive)
        ));
    }

    #[test]
    fn test_switch_storage_read_evm_contract() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let contract = addr(0xAA);
        let asset_id = 3u64;

        provider
            .sstore(
                ASSET_ADDRESS,
                slot_evm_contract(asset_id),
                address_to_u256(contract),
            )
            .unwrap();

        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));
        assert_eq!(store.read_evm_contract(asset_id).unwrap(), contract);
    }

    #[test]
    fn test_switch_storage_read_evm_contract_not_registered() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let asset_id = 3u64;

        // evm_contract NOT set (remains zero)
        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));
        assert!(matches!(
            store.read_evm_contract(asset_id),
            Err(SwitchError::EvmContractNotRegistered)
        ));
    }

    #[test]
    fn test_switch_to_evm_amount_zero_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);

        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, sender), u128_to_u256(1000))
            .unwrap();

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&0u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "amount=0 should fail");
    }

    #[test]
    fn test_switch_to_evm_to_zero_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);

        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, sender), u128_to_u256(1000))
            .unwrap();

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(Address::ZERO.as_slice());
        input[84..100].copy_from_slice(&100u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "to=ZERO should fail");
    }

    #[test]
    fn test_switch_to_evm_asset_not_active_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);
        let asset_id = 9u64;

        // Seed balance but asset status is not set (defaults to 0? actually defaults to 0 which means active)
        // Set status to 1 (inactive)
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, sender),
                u128_to_u256(1000),
            )
            .unwrap();
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(1),
            )
            .unwrap();

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&100u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "inactive asset should fail");
    }

    #[test]
    fn test_switch_storage_erc20_burn_underflow_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let contract = addr(0xAA);
        let holder = addr(0xBB);
        let asset_id = 2u64;

        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));

        // Mint some tokens first
        store.erc20_mint(asset_id, contract, holder, 100).unwrap();

        // Manually reduce holder balance so totalSupply is fine but balance underflows
        let mut padded = [0u8; 32];
        padded[12..32].copy_from_slice(holder.as_slice());
        provider
            .sstore(contract, mapping_slot(&padded, 4), u128_to_u256(50))
            .unwrap();

        // Burn more than balance (totalSupply=100 is fine, balance=50 is not)
        let result = store.erc20_burn(asset_id, contract, holder, 100);
        assert!(
            matches!(result, Err(SwitchError::Erc20BalanceUnderflow)),
            "expected Erc20BalanceUnderflow, got {:?}",
            result
        );
    }

    #[test]
    fn test_switch_storage_protocol_bal_overflow_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = addr(0x33);
        let asset_id = 7u64;

        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, addr),
                u128_to_u256(u128::MAX),
            )
            .unwrap();

        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));

        let result = store.add_protocol_bal(asset_id, addr, 1);
        assert!(
            matches!(result, Err(SwitchError::ProtocolBalanceOverflow)),
            "expected ProtocolBalanceOverflow, got {:?}",
            result
        );
    }

    #[test]
    fn test_switch_storage_protocol_bal_underflow_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = addr(0x33);
        let asset_id = 7u64;

        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, addr),
                u128_to_u256(10),
            )
            .unwrap();

        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));

        let result = store.sub_protocol_bal(asset_id, addr, 20);
        assert!(
            matches!(result, Err(SwitchError::InsufficientProtocolBalance)),
            "expected InsufficientProtocolBalance, got {:?}",
            result
        );
    }

    #[test]
    fn test_switch_to_protocol_insufficient_evm_balance_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);

        // Only 100 native EVM balance
        provider.balance_add(sender, U256::from(100)).unwrap();

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToProtocolCall::SELECTOR);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&200u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "insufficient EVM balance should fail");

        // Balance unchanged
        let evm_bal = provider.balance_get(sender).ok();
        assert_eq!(evm_bal, Some(U256::from(100)));
    }

    #[test]
    fn test_switch_storage_erc20_total_supply_underflow_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let contract = addr(0xAA);
        let holder = addr(0xBB);
        let asset_id = 2u64;

        let mut store = SwitchStorage::new(StorageRef::new(&mut provider));

        // Mint
        store.erc20_mint(asset_id, contract, holder, 100).unwrap();

        // Burn more than total supply (impossible in normal flow but test the check)
        // First, manually increase holder balance without touching total supply
        let mut padded = [0u8; 32];
        padded[12..32].copy_from_slice(holder.as_slice());
        provider
            .sstore(contract, mapping_slot(&padded, 4), u128_to_u256(200))
            .unwrap();

        let result = store.erc20_burn(asset_id, contract, holder, 200);
        assert!(
            matches!(result, Err(SwitchError::Erc20TotalSupplyUnderflow)),
            "expected Erc20TotalSupplyUnderflow, got {:?}",
            result
        );
    }
}
