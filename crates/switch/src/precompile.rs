//! Switch precompile entry point (0x207).
//!
//! Bidirectional bridge between protocol balance and EVM ERC-20 tokens using
//! an escrow model:
//! - switchToEvm: protocol balance -> EVM ERC-20 (transfer from 0x207 escrow)
//! - switchToProtocol: EVM ERC-20 -> protocol balance (transferFrom into 0x207 escrow)
//!
//! Protocol balances live in ASSET_ADDRESS (0x201) storage slots.
//! EVM-side mutations happen through nested revm calls:
//!   - CALL (asset_id = 1): native EVM balance via balance_add / balance_sub
//!   - Other assets: ERC-20 transfer / transferFrom via nested EVM execution

use alloy_primitives::{keccak256, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use call_precompile::{
    dispatch, require_caller, slot_asset_meta, slot_balance, slot_evm_contract,
    storage::StorageProvider, u128_to_u256, u256_to_address, u256_to_u128, StorageRef,
    ASSET_ADDRESS,
};
use call_protocol::storage_backend::StorageBackend;
use revm::database_interface::Database;
use revm::primitives::{B256, TxKind};
use revm::{ExecuteEvm, MainBuilder, MainContext};
use revm_precompile::{PrecompileError, PrecompileResult};

pub const SWITCH_ADDRESS: Address =
    alloy_primitives::address!("0000000000000000000000000000000000000207");

/// Maximum gas allowed for a single nested ERC-20 call.
const NESTED_CALL_GAS_LIMIT: u64 = 100_000;

// ── ABI selectors ─────────────────────────────────────────────────────

/// `keccak256("transfer(address,uint256)")[:4]`
const SELECTOR_TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
/// `keccak256("transferFrom(address,address,uint256)")[:4]`
const SELECTOR_TRANSFER_FROM: [u8; 4] = [0x23, 0xb8, 0x72, 0xdd];

// ── Error type ────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SwitchError {
    AssetNotActive,
    AssetHasNoErc20Bridge,
    EvmContractNotRegistered,
    InsufficientProtocolBalance,
    InsufficientEvmBalance,
    ProtocolBalanceOverflow,
    ProtocolBalanceUnderflow,
    AmountMustBePositive,
    ToCannotBeZero,
    EvmCallFailed,
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
            SwitchError::ProtocolBalanceUnderflow => write!(f, "protocol balance underflow"),
            SwitchError::AmountMustBePositive => write!(f, "amount must be > 0"),
            SwitchError::ToCannotBeZero => write!(f, "to cannot be zero address"),
            SwitchError::EvmCallFailed => write!(f, "nested EVM call failed"),
        }
    }
}

impl From<SwitchError> for PrecompileError {
    fn from(e: SwitchError) -> Self {
        PrecompileError::Other(e.to_string().into())
    }
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

    pub fn load_protocol_bal(&mut self, asset_id: u64, addr: Address) -> u128 {
        u256_to_u128(
            self.backend
                .load(ASSET_ADDRESS, slot_balance(asset_id, addr)),
        )
    }

    pub fn save_protocol_bal(&mut self, asset_id: u64, addr: Address, amount: u128) {
        self.backend.store(
            ASSET_ADDRESS,
            slot_balance(asset_id, addr),
            u128_to_u256(amount),
        );
    }

    pub fn add_protocol_bal(
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

    pub fn sub_protocol_bal(
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

    // ── EVM contract lookup ─────────────────────────────────────────

    pub fn read_evm_contract(&mut self, asset_id: u64) -> Result<Address, SwitchError> {
        let addr = u256_to_address(
            self.backend
                .load(ASSET_ADDRESS, slot_evm_contract(asset_id)),
        );
        if addr == Address::ZERO {
            return Err(SwitchError::EvmContractNotRegistered);
        }
        Ok(addr)
    }

    pub fn check_asset_active(&mut self, asset_id: u64) -> Result<(), SwitchError> {
        let status = self
            .backend
            .load(ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
            .to_be_bytes::<32>()[31];
        if status != 0 {
            return Err(SwitchError::AssetNotActive);
        }
        Ok(())
    }

    pub fn check_has_erc20(&mut self, asset_id: u64) -> Result<(), SwitchError> {
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
}

// ── Database adapter for nested EVM calls ─────────────────────────────

struct StorageProviderDb<'a> {
    provider: &'a mut dyn StorageProvider,
}

impl<'a> Database for StorageProviderDb<'a> {
    type Error = revm::database_interface::ErasedError;

    fn basic(
        &mut self,
        address: Address,
    ) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        let balance = self
            .provider
            .raw_balance_get(address)
            .map_err(revm::database_interface::ErasedError::new)?;
        let code = self
            .provider
            .raw_code_get(address)
            .map_err(revm::database_interface::ErasedError::new)?;
        let code_hash = if code.is_empty() {
            keccak256(&[])
        } else {
            keccak256(&code)
        };
        let code = if code.is_empty() {
            None
        } else {
            Some(revm::state::Bytecode::new_legacy(code))
        };
        Ok(Some(revm::state::AccountInfo {
            balance,
            nonce: 0,
            code_hash,
            code,
            account_id: None,
        }))
    }

    fn code_by_hash(
        &mut self,
        _hash: B256,
    ) -> Result<revm::state::Bytecode, Self::Error> {
        Ok(revm::state::Bytecode::default())
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.provider
            .raw_sload(address, index)
            .map_err(revm::database_interface::ErasedError::new)
    }

    fn block_hash(&mut self, _number: u64) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

// ── Nested EVM call helpers ───────────────────────────────────────────

fn execute_evm_call(
    db: &mut StorageProviderDb,
    contract: Address,
    data: Bytes,
) -> Result<(revm::context_interface::result::ExecutionResult, revm::state::EvmState), PrecompileError> {
    let block_number = db.provider.block_number();
    let timestamp = db.provider.timestamp();

    let tx_env = revm::context::TxEnv::builder()
        .caller(SWITCH_ADDRESS)
        .gas_limit(NESTED_CALL_GAS_LIMIT)
        .gas_price(0)
        .kind(TxKind::Call(contract))
        .value(U256::ZERO)
        .data(data)
        .build()
        .map_err(|e| PrecompileError::Other(format!("tx build: {e:?}").into()))?;

    let result = {
        let ctx = revm::Context::mainnet()
            .with_db(&mut *db)
            .modify_cfg_chained(|cfg| {
                cfg.set_spec(revm::primitives::hardfork::SpecId::CANCUN)
            });
        let mut evm = ctx.build_mainnet();
        let mut block_env = revm::context::BlockEnv::default();
        block_env.number = U256::from(block_number);
        block_env.timestamp = timestamp;
        evm.set_block(block_env);
        evm.transact(tx_env)
            .map_err(|e| PrecompileError::Other(format!("evm call: {e:?}").into()))?
    };

    let gas_spent = match &result.result {
        revm::context_interface::result::ExecutionResult::Success { gas, .. } => gas.spent(),
        revm::context_interface::result::ExecutionResult::Revert { gas, .. } => gas.spent(),
        revm::context_interface::result::ExecutionResult::Halt { gas, .. } => gas.spent(),
    };
    db.provider.deduct_gas(gas_spent)?;

    Ok((result.result, result.state))
}

fn apply_state_changes(
    storage: &mut dyn StorageProvider,
    state: revm::state::EvmState,
) -> Result<(), PrecompileError> {
    for (address, account) in state {
        if !account.status.is_touched() {
            continue;
        }

        // Apply storage changes
        for (slot, storage_slot) in account.storage {
            if storage_slot.present_value != storage_slot.original_value {
                storage.raw_sstore(address, slot, storage_slot.present_value)?;
            }
        }

        // Apply balance changes
        let old_balance = account.original_info.balance;
        let new_balance = account.info.balance;
        if old_balance != new_balance {
            if new_balance > old_balance {
                storage.balance_add(address, new_balance - old_balance)?;
            } else {
                storage.balance_sub(address, old_balance - new_balance)?;
            }
        }
    }
    Ok(())
}

fn encode_transfer(to: Address, amount: u128) -> Bytes {
    let mut data = vec![0u8; 68];
    data[0..4].copy_from_slice(&SELECTOR_TRANSFER);
    data[16..36].copy_from_slice(to.as_slice());
    data[36..68].copy_from_slice(&U256::from(amount).to_be_bytes::<32>());
    Bytes::from(data)
}

fn encode_transfer_from(from: Address, to: Address, amount: u128) -> Bytes {
    let mut data = vec![0u8; 100];
    data[0..4].copy_from_slice(&SELECTOR_TRANSFER_FROM);
    data[16..36].copy_from_slice(from.as_slice());
    data[48..68].copy_from_slice(to.as_slice());
    data[68..100].copy_from_slice(&U256::from(amount).to_be_bytes::<32>());
    Bytes::from(data)
}

/// Decode an ABI-encoded `bool` return value.
/// Returns `true` only if the output is at least 32 bytes and the last byte is 1.
fn decode_abi_bool(output: &revm::context_interface::result::Output) -> bool {
    let bytes = match output {
        revm::context_interface::result::Output::Call(b) => b.as_ref(),
        revm::context_interface::result::Output::Create(b, _) => b.as_ref(),
    };
    bytes.len() >= 32 && bytes[31] == 1
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
                if call.amount == 0 {
                    return Err(PrecompileError::Other("amount must be > 0".into()));
                }
                if call.to == Address::ZERO {
                    return Err(PrecompileError::Other("to cannot be zero address".into()));
                }
                let mut store = SwitchStorage::new(sr);
                store.check_asset_active(call.assetId)?;
                store.check_has_erc20(call.assetId)?;

                // 1. Deduct protocol balance
                store
                    .sub_protocol_bal(call.assetId, caller, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                // 2. Credit EVM side
                if call.assetId == call_protocol::CALL_ASSET_ID {
                    storage
                        .balance_add(call.to, U256::from(call.amount))
                        .map_err(|e| PrecompileError::Other(format!("native balance add: {e}").into()))?;
                } else {
                    let contract = store
                        .read_evm_contract(call.assetId)
                        .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                    let code = storage.code_get(contract)?;
                    if code.is_empty() {
                        return Err(PrecompileError::Other("no code at ERC-20 contract".into()));
                    }

                    let data = encode_transfer(call.to, call.amount);
                    let mut db = StorageProviderDb { provider: storage };
                    let (result, state) = execute_evm_call(&mut db, contract, data)?;
                    match result {
                        revm::context_interface::result::ExecutionResult::Success { output, .. } => {
                            if !decode_abi_bool(&output) {
                                return Err(PrecompileError::Other("ERC-20 transfer returned false".into()));
                            }
                            apply_state_changes(storage, state)?;
                        }
                        _ => return Err(PrecompileError::Other("ERC-20 transfer failed".into())),
                    }
                }
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
                if call.amount == 0 {
                    return Err(PrecompileError::Other("amount must be > 0".into()));
                }
                if call.to == Address::ZERO {
                    return Err(PrecompileError::Other("to cannot be zero address".into()));
                }
                let mut store = SwitchStorage::new(sr);
                store.check_asset_active(call.assetId)?;
                store.check_has_erc20(call.assetId)?;

                // 1. Deduct EVM side
                if call.assetId == call_protocol::CALL_ASSET_ID {
                    storage
                        .balance_sub(caller, U256::from(call.amount))
                        .map_err(|e| PrecompileError::Other(format!("native balance sub: {e}").into()))?;
                } else {
                    let contract = store
                        .read_evm_contract(call.assetId)
                        .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                    let code = storage.code_get(contract)?;
                    if code.is_empty() {
                        return Err(PrecompileError::Other("no code at ERC-20 contract".into()));
                    }

                    let data = encode_transfer_from(caller, SWITCH_ADDRESS, call.amount);
                    let mut db = StorageProviderDb { provider: storage };
                    let (result, state) = execute_evm_call(&mut db, contract, data)?;
                    match result {
                        revm::context_interface::result::ExecutionResult::Success { output, .. } => {
                            if !decode_abi_bool(&output) {
                                return Err(PrecompileError::Other("ERC-20 transferFrom returned false".into()));
                            }
                            apply_state_changes(storage, state)?;
                        }
                        _ => return Err(PrecompileError::Other("ERC-20 transferFrom failed".into())),
                    }
                }

                // 2. Credit protocol balance
                store
                    .add_protocol_bal(call.assetId, call.to, call.amount)
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
    fn test_switch_to_evm_erc20_reverts_without_escrow() {
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

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&400u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        // Should fail because the contract has no code (empty escrow)
        assert!(
            result.is_err(),
            "switchToEvm ERC-20 should fail when escrow has no tokens: {:?}",
            result
        );

        // Protocol balance must be unchanged (atomic rollback)
        let sender_bal = provider
            .sload(ASSET_ADDRESS, slot_balance(asset_id, sender))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(sender_bal, 1000);
    }

    #[test]
    fn test_switch_to_protocol_erc20_reverts_without_approval() {
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

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToProtocolCall::SELECTOR);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&200u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        // Should fail because the contract has no code (no tokens to withdraw)
        assert!(
            result.is_err(),
            "switchToProtocol ERC-20 should fail without tokens: {:?}",
            result
        );

        // Protocol balance must be unchanged
        let recipient_bal = provider
            .sload(ASSET_ADDRESS, slot_balance(asset_id, recipient))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(recipient_bal, 0);
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

        // Seed balance but asset status is not set (defaults to 0 which means active)
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

    // ── ERC-20 escrow success path tests ────────────────────────────────

    const WRAPPED_TOKEN_RUNTIME: &str =
        include_str!("../../evm/contracts/WrappedToken.bin-runtime");

    fn mapping_slot(base: u64, key: Address) -> U256 {
        let mut input = [0u8; 64];
        input[12..32].copy_from_slice(key.as_slice());
        input[32..64].copy_from_slice(&U256::from(base).to_be_bytes::<32>());
        U256::from_be_bytes::<32>(keccak256(input).0)
    }

    fn nested_mapping_slot(base: u64, key1: Address, key2: Address) -> U256 {
        let inner = mapping_slot(base, key1);
        let mut input = [0u8; 64];
        input[12..32].copy_from_slice(key2.as_slice());
        input[32..64].copy_from_slice(&inner.to_be_bytes::<32>());
        U256::from_be_bytes::<32>(keccak256(input).0)
    }

    #[test]
    fn test_switch_to_evm_erc20_success_with_escrow() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);
        let contract = addr(0xAA);
        let asset_id = 2u64;

        // Register asset as active with ERC-20 bridge
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            )
            .unwrap();
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"has_erc20"),
                U256::from(1),
            )
            .unwrap();
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_evm_contract(asset_id),
                address_to_u256(contract),
            )
            .unwrap();

        // Seed protocol balance for sender
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(asset_id, sender),
                u128_to_u256(1000),
            )
            .unwrap();

        // Deploy WrappedToken runtime bytecode
        let bytecode = hex::decode(WRAPPED_TOKEN_RUNTIME.trim())
            .expect("valid runtime hex");
        provider.set_code(contract, alloy_primitives::Bytes::from(bytecode));

        // Seed escrow (0x207) with ERC-20 tokens so transfer can succeed
        let escrow_bal_slot = mapping_slot(4, SWITCH_ADDRESS);
        provider.set(contract, escrow_bal_slot, u128_to_u256(500));

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToEvmCall::SELECTOR);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&300u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(
            result.is_ok(),
            "switchToEvm ERC-20 should succeed with escrow: {:?}",
            result.err()
        );

        // Protocol balance deducted
        let sender_bal = provider
            .get(ASSET_ADDRESS, slot_balance(asset_id, sender))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(sender_bal, 700);

        // Escrow balance reduced
        let escrow_bal = provider
            .get(contract, escrow_bal_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(escrow_bal, 200);

        // Recipient credited with ERC-20 tokens
        let recipient_bal_slot = mapping_slot(4, recipient);
        let recipient_bal = provider
            .get(contract, recipient_bal_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(recipient_bal, 300);
    }

    #[test]
    fn test_switch_to_protocol_erc20_success() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = addr(0x33);
        let recipient = addr(0x44);
        let contract = addr(0xAA);
        let asset_id = 3u64;

        // Register asset as active with ERC-20 bridge
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"status"),
                U256::from(0),
            )
            .unwrap();
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(asset_id, b"has_erc20"),
                U256::from(1),
            )
            .unwrap();
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_evm_contract(asset_id),
                address_to_u256(contract),
            )
            .unwrap();

        // Deploy WrappedToken runtime bytecode
        let bytecode = hex::decode(WRAPPED_TOKEN_RUNTIME.trim())
            .expect("valid runtime hex");
        provider.set_code(contract, alloy_primitives::Bytes::from(bytecode));

        // Seed sender with ERC-20 tokens
        let sender_bal_slot = mapping_slot(4, sender);
        provider.set(contract, sender_bal_slot, u128_to_u256(500));

        // Approve 0x207 to spend sender's tokens
        let approval_slot = nested_mapping_slot(5, sender, SWITCH_ADDRESS);
        provider.set(contract, approval_slot, u128_to_u256(300));

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&IProtocolSwitch::switchToProtocolCall::SELECTOR);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(recipient.as_slice());
        input[84..100].copy_from_slice(&300u128.to_be_bytes());

        let mut precompile = SwitchPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(
            result.is_ok(),
            "switchToProtocol ERC-20 should succeed with approval: {:?}",
            result.err()
        );

        // Sender ERC-20 balance reduced
        let sender_bal = provider
            .get(contract, sender_bal_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(sender_bal, 200);

        // Escrow (0x207) credited with ERC-20 tokens
        let escrow_bal_slot = mapping_slot(4, SWITCH_ADDRESS);
        let escrow_bal = provider
            .get(contract, escrow_bal_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(escrow_bal, 300);

        // Allowance consumed
        let approval = provider
            .get(contract, approval_slot)
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(approval, 0);

        // Protocol balance credited to recipient
        let recipient_bal = provider
            .get(ASSET_ADDRESS, slot_balance(asset_id, recipient))
            .map(u256_to_u128)
            .unwrap_or(0);
        assert_eq!(recipient_bal, 300);
    }
}
