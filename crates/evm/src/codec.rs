//! Compact binary serialization for EVM state.
//!
//! Replaces `serde_json` with `postcard` for `EvmAccount` and raw fixed-width
//! bytes for `U256` storage values.  This reduces on-disk footprint and
//! eliminates JSON parsing overhead on every state read.
//!
//! # Backwards compatibility
//! `decode_account` tries `postcard` first and falls back to `serde_json` so
//! existing databases continue to work after the upgrade.  `decode_u256`
//! detects JSON by its leading quote (`0x22`) and falls back automatically.

use alloy_primitives::U256;

use crate::state::EvmAccount;

// ── EvmAccount ────────────────────────────────────────────────────────

/// Serialize an [`EvmAccount`] with `postcard` (compact binary).
pub fn encode_account(acc: &EvmAccount) -> Vec<u8> {
    postcard::to_allocvec(acc).unwrap_or_default()
}

/// Deserialize an [`EvmAccount`].
///
/// Tries `postcard` first, then falls back to `serde_json` for databases
/// written before this migration.
pub fn decode_account(bytes: &[u8]) -> Option<EvmAccount> {
    postcard::from_bytes(bytes)
        .or_else(|_| serde_json::from_slice(bytes))
        .ok()
}

// ── U256 storage values ───────────────────────────────────────────────

/// Serialize a [`U256`] as fixed 32-byte big-endian.
///
/// This is ~4x smaller than `serde_json::to_vec` (which emits a quoted
/// decimal string like `"12345678901234567890"`).
pub fn encode_u256(v: &U256) -> Vec<u8> {
    v.to_be_bytes::<32>().to_vec()
}

/// Deserialize a [`U256`] from fixed 32-byte big-endian.
///
/// If the first byte is `0x22` (`"`) we assume the value was written by the
/// old `serde_json` path and decode accordingly.
pub fn decode_u256(bytes: &[u8]) -> Option<U256> {
    if bytes.len() == 32 {
        // New compact binary format
        Some(U256::from_be_bytes::<32>(bytes.try_into().ok()?))
    } else if bytes.first() == Some(&b'"') || bytes.first() == Some(&b'[') {
        // Old serde_json format (quoted string or array)
        serde_json::from_slice(bytes).ok()
    } else {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Bytes, U256};
    use std::collections::HashMap;

    #[test]
    fn test_account_roundtrip() {
        let acc = EvmAccount {
            nonce: 42,
            balance: U256::from(1_000_000),
            code: Bytes::from(vec![0x60, 0x00, 0x55]),
            storage: {
                let mut m = HashMap::new();
                m.insert(U256::from(7), U256::from(123));
                m
            },
        };
        let encoded = encode_account(&acc);
        let decoded = decode_account(&encoded).expect("decode ok");
        assert_eq!(acc.nonce, decoded.nonce);
        assert_eq!(acc.balance, decoded.balance);
        assert_eq!(acc.code, decoded.code);
        assert_eq!(acc.storage, decoded.storage);
    }

    #[test]
    fn test_account_json_fallback() {
        let acc = EvmAccount {
            nonce: 5,
            balance: U256::from(100),
            code: Bytes::default(),
            storage: HashMap::new(),
        };
        let json = serde_json::to_vec(&acc).unwrap();
        let decoded = decode_account(&json).expect("json fallback ok");
        assert_eq!(acc.nonce, decoded.nonce);
        assert_eq!(acc.balance, decoded.balance);
    }

    #[test]
    fn test_u256_roundtrip() {
        let v = U256::from(0xDEADBEEFCAFEu128);
        let encoded = encode_u256(&v);
        assert_eq!(encoded.len(), 32);
        let decoded = decode_u256(&encoded).expect("decode ok");
        assert_eq!(v, decoded);
    }

    #[test]
    fn test_u256_zero_roundtrip() {
        let v = U256::ZERO;
        let encoded = encode_u256(&v);
        assert_eq!(encoded, vec![0u8; 32]);
        let decoded = decode_u256(&encoded).expect("decode ok");
        assert_eq!(v, decoded);
    }

    #[test]
    fn test_u256_max_roundtrip() {
        let v = U256::MAX;
        let encoded = encode_u256(&v);
        assert_eq!(encoded.len(), 32);
        let decoded = decode_u256(&encoded).expect("decode ok");
        assert_eq!(v, decoded);
    }

    #[test]
    fn test_u256_json_fallback() {
        let v = U256::from(12345);
        let json = serde_json::to_vec(&v).unwrap();
        let decoded = decode_u256(&json).expect("json fallback ok");
        assert_eq!(v, decoded);
    }
}
