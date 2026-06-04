//! ERC-20 metadata reader for precompiles.
//!
//! Reads `name`, `symbol`, and `decimals` via actual EVM static calls to the
//! target contract. Falls back to direct storage slot reading (OpenZeppelin v4/v5)
//! if the EVM calls do not return valid data.
//!
//! # Why EVM calls?
//!
//! Direct storage slot reading is fragile: it only works for contracts that follow
//! exact OZ layouts. EVM static calls work for any contract that correctly implements
//! the ERC-20 metadata interface, including proxies, computed properties, and
//! non-standard storage layouts.

use alloy_primitives::{keccak256, Address, Bytes, U256};
use revm::database_interface::Database;
use revm::primitives::{B256, TxKind};
use revm::{ExecuteEvm, MainBuilder, MainContext};
use revm_precompile::PrecompileError;

use crate::storage::StorageProvider;

// ── ABI selectors ─────────────────────────────────────────────────────

/// `keccak256("name()")[:4]`
const SELECTOR_NAME: [u8; 4] = [0x06, 0xfd, 0xde, 0x03];
/// `keccak256("symbol()")[:4]`
const SELECTOR_SYMBOL: [u8; 4] = [0x95, 0xd8, 0x9b, 0x41];
/// `keccak256("decimals()")[:4]`
const SELECTOR_DECIMALS: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

// ── Gas budget ────────────────────────────────────────────────────────

/// Maximum gas allowed for a single nested metadata view call.
const NESTED_CALL_GAS_LIMIT: u64 = 100_000;

// ── Public API ────────────────────────────────────────────────────────

/// ERC-20 metadata discovered from contract storage or EVM calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Erc20Metadata {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
}

/// Read ERC-20 metadata from a contract.
///
/// Tries EVM static calls first (`name()`, `symbol()`, `decimals()`). If the
/// contract does not return valid ABI-encoded data, falls back to direct
/// storage slot reading (OpenZeppelin v5 then v4 layouts).
pub fn read_erc20_metadata(
    storage: &mut dyn StorageProvider,
    contract: Address,
) -> Result<Erc20Metadata, PrecompileError> {
    let code = storage.code_get(contract)?;
    if code.is_empty() {
        return Err(PrecompileError::Other("no code at address".into()));
    }

    // Primary path: actual EVM static calls.
    if let Some(meta) = try_evm_call_metadata(storage, contract)? {
        if !meta.name.is_empty() && !meta.symbol.is_empty() {
            return Ok(meta);
        }
    }

    // Fallback: direct storage slot reading.
    try_storage_slot_metadata(storage, contract)
}

// ── EVM call path ─────────────────────────────────────────────────────

/// Attempt to read metadata via nested EVM static calls.
fn try_evm_call_metadata(
    storage: &mut dyn StorageProvider,
    contract: Address,
) -> Result<Option<Erc20Metadata>, PrecompileError> {
    let mut db = StorageProviderDb { provider: storage };

    let name =
        match execute_static_call(&mut db, contract, Bytes::from_static(&SELECTOR_NAME))? {
            Some(data) => match decode_abi_string(&data) {
                Ok(s) if !s.is_empty() => s,
                _ => return Ok(None),
            },
            None => return Ok(None),
        };

    let symbol =
        match execute_static_call(&mut db, contract, Bytes::from_static(&SELECTOR_SYMBOL))? {
            Some(data) => match decode_abi_string(&data) {
                Ok(s) if !s.is_empty() => s,
                _ => return Ok(None),
            },
            None => return Ok(None),
        };

    let decimals =
        match execute_static_call(&mut db, contract, Bytes::from_static(&SELECTOR_DECIMALS))? {
            Some(data) => decode_abi_uint8(&data),
            None => 18,
        };

    Ok(Some(Erc20Metadata {
        name,
        symbol,
        decimals,
    }))
}

/// Execute a single static call through a nested revm instance.
///
/// Deducts the **actual gas spent** from the outer `StorageProvider`.
fn execute_static_call(
    db: &mut StorageProviderDb,
    contract: Address,
    data: Bytes,
) -> Result<Option<Bytes>, PrecompileError> {
    let block_number = db.provider.block_number();
    let timestamp = db.provider.timestamp();

    let tx_env = revm::context::TxEnv::builder()
        .caller(Address::ZERO)
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

    // Deduct actual gas spent from the outer provider budget.
    let gas_spent = match &result.result {
        revm::context_interface::result::ExecutionResult::Success { gas, .. } => gas.spent(),
        revm::context_interface::result::ExecutionResult::Revert { gas, .. } => gas.spent(),
        revm::context_interface::result::ExecutionResult::Halt { gas, .. } => gas.spent(),
    };
    db.provider.charge_gas(gas_spent)?;

    match result.result {
        revm::context_interface::result::ExecutionResult::Success { output, .. } => {
            let bytes = match output {
                revm::context_interface::result::Output::Call(b) => b,
                revm::context_interface::result::Output::Create(b, _) => b,
            };
            Ok(if bytes.is_empty() { None } else { Some(bytes) })
        }
        _ => Ok(None),
    }
}

// ── Database adapter ──────────────────────────────────────────────────

/// [`Database`] implementation backed by a [`StorageProvider`].
///
/// Uses `raw_*` methods so that gas is **not** double-counted: the nested EVM
/// tracks its own gas, and only the final `gas.spent()` is deducted from the
/// outer provider.
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

// ── ABI decoding ──────────────────────────────────────────────────────

/// Decode an ABI-encoded `string` return value.
fn decode_abi_string(data: &[u8]) -> Result<String, PrecompileError> {
    if data.len() < 64 {
        return Err(PrecompileError::Other("string return too short".into()));
    }
    let offset = U256::from_be_slice(&data[0..32]).to::<usize>();
    if offset + 32 > data.len() {
        return Err(PrecompileError::Other("string offset out of bounds".into()));
    }
    let len = U256::from_be_slice(&data[offset..offset + 32]).to::<usize>();
    if offset + 32 + len > data.len() {
        return Err(PrecompileError::Other("string length out of bounds".into()));
    }
    let str_bytes = &data[offset + 32..offset + 32 + len];
    String::from_utf8(str_bytes.to_vec())
        .map_err(|_| PrecompileError::Other("invalid utf8 in string return".into()))
}

/// Decode an ABI-encoded `uint8` return value.
fn decode_abi_uint8(data: &[u8]) -> u8 {
    if data.len() >= 32 {
        data[31]
    } else {
        18
    }
}

// ── Fallback: direct storage slot reading ─────────────────────────────

fn try_storage_slot_metadata(
    storage: &mut dyn StorageProvider,
    contract: Address,
) -> Result<Erc20Metadata, PrecompileError> {
    // Try OZ v5 layout first.
    if let Some(name) = read_solidity_string(storage, contract, U256::from(0))? {
        if let Some(symbol) = read_solidity_string(storage, contract, U256::from(1))? {
            if !name.is_empty() && !symbol.is_empty() {
                let decimals = read_solidity_uint8(storage, contract, U256::from(2));
                return Ok(Erc20Metadata {
                    name,
                    symbol,
                    decimals,
                });
            }
        }
    }

    // Fall back to OZ v4 layout.
    if let Some(name) = read_solidity_string(storage, contract, U256::from(3))? {
        if let Some(symbol) = read_solidity_string(storage, contract, U256::from(4))? {
            if !name.is_empty() && !symbol.is_empty() {
                return Ok(Erc20Metadata {
                    name,
                    symbol,
                    decimals: 18,
                });
            }
        }
    }

    Err(PrecompileError::Other(
        "unable to read ERC-20 metadata from contract".into(),
    ))
}

// ── Solidity storage decoding ─────────────────────────────────────────

/// Read a Solidity `string` state variable from storage.
///
/// Returns `Ok(Some(s))` for a valid string, `Ok(None)` if the slot does not
/// contain a string header, and `Err` on I/O failure.
fn read_solidity_string(
    storage: &mut dyn StorageProvider,
    contract: Address,
    base_slot: U256,
) -> Result<Option<String>, PrecompileError> {
    let header = storage.sload(contract, base_slot)?;
    let bytes = header.to_be_bytes::<32>();
    let flag = bytes[31];

    if flag & 1 == 0 {
        // Short string: length = flag / 2, data in high bytes of same slot.
        let len = (flag / 2) as usize;
        if len > 31 {
            return Ok(None); // Invalid short-string length.
        }
        if len == 0 {
            return Ok(Some(String::new()));
        }
        // Data occupies bytes[0..len]; the rest should be zero.
        let data = &bytes[..len];
        // Sanity check: trailing bytes before the flag byte must be zero.
        if bytes[len..31].iter().any(|b| *b != 0) {
            return Ok(None);
        }
        String::from_utf8(data.to_vec())
            .map(Some)
            .map_err(|_| PrecompileError::Other("invalid utf8 in short string".into()))
    } else {
        // Long string: length = (flag - 1) / 2, data at keccak256(base_slot).
        let len = ((flag - 1) / 2) as usize;
        if len == 0 {
            return Ok(Some(String::new()));
        }
        let data_slot = U256::from_be_slice(keccak256(base_slot.to_be_bytes::<32>()).as_slice());
        let mut result = Vec::with_capacity(len);
        let mut current_slot = data_slot;
        let mut remaining = len;

        while remaining > 0 {
            let chunk = storage.sload(contract, current_slot)?.to_be_bytes::<32>();
            let to_take = remaining.min(32);
            result.extend_from_slice(&chunk[..to_take]);
            remaining -= to_take;
            current_slot += U256::from(1);
        }

        String::from_utf8(result)
            .map(Some)
            .map_err(|_| PrecompileError::Other("invalid utf8 in long string".into()))
    }
}

/// Read a Solidity `uint8` state variable from a single storage slot.
fn read_solidity_uint8(
    storage: &mut dyn StorageProvider,
    contract: Address,
    slot: U256,
) -> u8 {
    storage
        .sload(contract, slot)
        .map(|v| v.to_be_bytes::<32>()[31])
        .unwrap_or(18)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::HashMapStorageProvider;

    fn addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    /// Encode a short Solidity string into a U256 storage word.
    /// Data is left-aligned (high bytes), length*2 in the low byte.
    fn encode_short_string(s: &str) -> U256 {
        let len = s.len();
        assert!(len <= 31, "short string only");
        let mut bytes = [0u8; 32];
        bytes[..len].copy_from_slice(s.as_bytes());
        bytes[31] = (len * 2) as u8;
        U256::from_be_bytes::<32>(bytes)
    }

    /// Store a long Solidity string across multiple slots.
    fn store_long_string(
        provider: &mut HashMapStorageProvider,
        contract: Address,
        base_slot: U256,
        s: &str,
    ) {
        let len = s.len();
        // Header slot: len * 2 + 1
        let mut header = [0u8; 32];
        header[31] = (len * 2 + 1) as u8;
        provider
            .sstore(contract, base_slot, U256::from_be_bytes::<32>(header))
            .unwrap();

        // Data at keccak256(base_slot)
        let data_slot = U256::from_be_slice(keccak256(base_slot.to_be_bytes::<32>()).as_slice());
        let bytes = s.as_bytes();
        for (i, chunk) in bytes.chunks(32).enumerate() {
            let mut word = [0u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            provider
                .sstore(contract, data_slot + U256::from(i), U256::from_be_bytes::<32>(word))
                .unwrap();
        }
    }

    #[test]
    fn test_read_oz_v5_metadata() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let contract = addr(0xAB);

        // Set some dummy code so it's not rejected.
        provider.set_code(contract, alloy_primitives::bytes!("6000"));

        // OZ v5 layout: name@0, symbol@1, decimals@2
        provider
            .sstore(contract, U256::from(0), encode_short_string("MyToken"))
            .unwrap();
        provider
            .sstore(contract, U256::from(1), encode_short_string("MTK"))
            .unwrap();
        provider
            .sstore(contract, U256::from(2), U256::from(6))
            .unwrap();

        let meta = read_erc20_metadata(&mut provider, contract).unwrap();
        assert_eq!(meta.name, "MyToken");
        assert_eq!(meta.symbol, "MTK");
        assert_eq!(meta.decimals, 6);
    }

    #[test]
    fn test_read_oz_v4_metadata() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let contract = addr(0xAB);

        provider.set_code(contract, alloy_primitives::bytes!("6000"));

        // OZ v4 layout: name@3, symbol@4
        provider
            .sstore(contract, U256::from(3), encode_short_string("Wrapped Ether"))
            .unwrap();
        provider
            .sstore(contract, U256::from(4), encode_short_string("WETH"))
            .unwrap();

        let meta = read_erc20_metadata(&mut provider, contract).unwrap();
        assert_eq!(meta.name, "Wrapped Ether");
        assert_eq!(meta.symbol, "WETH");
        assert_eq!(meta.decimals, 18); // default for v4
    }

    #[test]
    fn test_read_long_name_v5() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let contract = addr(0xAB);

        provider.set_code(contract, alloy_primitives::bytes!("6000"));

        let long_name = "This is a very long token name that exceeds thirty one bytes";
        assert!(long_name.len() > 31);

        store_long_string(&mut provider, contract, U256::from(0), long_name);
        provider
            .sstore(contract, U256::from(1), encode_short_string("LONG"))
            .unwrap();
        provider
            .sstore(contract, U256::from(2), U256::from(9))
            .unwrap();

        let meta = read_erc20_metadata(&mut provider, contract).unwrap();
        assert_eq!(meta.name, long_name);
        assert_eq!(meta.symbol, "LONG");
        assert_eq!(meta.decimals, 9);
    }

    #[test]
    fn test_no_code_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let contract = addr(0xAB);
        // No code set
        let result = read_erc20_metadata(&mut provider, contract);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_layout_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let contract = addr(0xAB);

        provider.set_code(contract, alloy_primitives::bytes!("6000"));

        // Put non-string data in both v5 and v4 name slots.
        provider
            .sstore(contract, U256::from(0), U256::from(0xDEADBEEFu64))
            .unwrap();
        provider
            .sstore(contract, U256::from(3), U256::from(0xCAFEBABEu64))
            .unwrap();

        let result = read_erc20_metadata(&mut provider, contract);
        assert!(result.is_err());
    }
}
