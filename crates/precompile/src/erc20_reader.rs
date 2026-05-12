//! ERC-20 metadata reader for precompiles.
//!
//! Reads `name`, `symbol`, and `decimals` directly from an ERC-20 contract's
//! EVM storage slots, supporting both OpenZeppelin v4 and v5 layouts.
//!
//! # Storage Layout Detection
//!
//! | Version | `_name` | `_symbol` | `_decimals` |
//! |---------|---------|-----------|-------------|
//! | OZ v5   | slot 0  | slot 1    | slot 2      |
//! | OZ v4   | slot 3  | slot 4    | not stored (assume 18) |
//!
//! The reader tries v5 first (slots 0, 1, 2). If slot 0 does not contain a
//! valid string, it falls back to v4 (slots 3, 4).

use alloy_primitives::{keccak256, Address, U256};
use revm_precompile::PrecompileError;

use crate::storage::StorageProvider;

/// ERC-20 metadata discovered from contract storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Erc20Metadata {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
}

/// Read ERC-20 metadata from contract storage.
///
/// Tries OpenZeppelin v5 layout first, then v4. Returns an error if the
/// contract does not appear to follow either layout.
pub fn read_erc20_metadata(
    storage: &mut dyn StorageProvider,
    contract: Address,
) -> Result<Erc20Metadata, PrecompileError> {
    // Ensure the contract has code.
    let code = storage.code_get(contract)?;
    if code.is_empty() {
        return Err(PrecompileError::Other("no code at address".into()));
    }

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
                // OZ v4 ERC20 does not store decimals; default to 18.
                return Ok(Erc20Metadata {
                    name,
                    symbol,
                    decimals: 18,
                });
            }
        }
    }

    Err(PrecompileError::Other(
        "unable to read ERC-20 metadata from contract storage".into(),
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
