//! Callchain primitives — shared base types

pub use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use serde::{Deserialize, Serialize};

// ─── Type aliases ───────────────────────────────────────────────

/// Transaction hash
pub type TxHash = B256;

/// Block hash
pub type BlockHash = B256;

/// Asset identifier
pub type AssetId = u64;

/// Balance in wei (u128, 18 decimals)
pub type Balance = u128;

/// Validator identifier
pub type ValidatorId = u32;

/// Nonce
pub type Nonce = u64;

/// Hash (H256)
pub type Hash = B256;

/// secp256k1 signature (65 bytes: r, s, v)
pub type Signature = [u8; 65];

/// secp256k1 public key (64 bytes: uncompressed x || y)
pub type PublicKey = [u8; 64];

/// Ed25519 public key (32 bytes)
pub type Ed25519PublicKey = [u8; 32];

// ─── Price pair ─────────────────────────────────────────────────

/// A trading pair for oracle price lookups.
///
/// `base` is the asset being priced; `quote` is the denomination.
/// Example: `PricePair { base: 1, quote: 0 }` = CALL/USD.
/// Asset ID `0` is reserved for the USD quote currency (not a real token).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PricePair {
    pub base: AssetId,
    pub quote: AssetId,
}

impl PricePair {
    /// CALL/USD price pair (base = CALL asset_id 1, quote = USD 0)
    pub const CALL_USD: Self = Self { base: 1, quote: 0 };

    /// Create a new price pair.
    pub const fn new(base: AssetId, quote: AssetId) -> Self {
        Self { base, quote }
    }
}

// ─── Protocol version ───────────────────────────────────────────

/// Protocol version (semver-like)
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable, Serialize, Deserialize,
)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl ProtocolVersion {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// ─── Fee currency ───────────────────────────────────────────────

/// Fee payment currency (per spec §12.3)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeeCurrency {
    /// Native CALL token
    Call,
    /// Registered stablecoin identified by AssetId
    Stablecoin(AssetId),
}

// ─── Execution status ───────────────────────────────────────────

/// Transaction execution status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionStatus {
    /// Execution succeeded
    Success,
    /// Execution reverted with reason
    Reverted { reason: String },
}

impl ExecutionStatus {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    pub fn is_reverted(&self) -> bool {
        matches!(self, Self::Reverted { .. })
    }
}

// ─── Constants ──────────────────────────────────────────────────

/// Minimum unit: 1 wei = 10^-18 CALL (per spec §12.1)
pub const MINIMUM_UNIT: u128 = 1;

/// Address byte length
pub const ADDRESS_LEN: usize = 20;

/// Signature byte length (secp256k1, 65 bytes)
pub const SIGNATURE_LEN: usize = 65;

/// Public key byte length (secp256k1 uncompressed, 64 bytes)
pub const PUBKEY_LEN: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_rlp::{Decodable, Encodable};

    #[test]
    fn test_address_zero() {
        let addr = Address::ZERO;
        assert_eq!(addr.as_slice().len(), 20);
        assert!(addr.is_zero());
    }

    #[test]
    fn test_address_checksum() {
        // EIP-55 checksum is handled by alloy_primitives::Address::parse_checksummed
        let addr = Address::parse_checksummed("0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed", None)
            .expect("valid checksum");
        assert_eq!(
            addr.to_checksum(None),
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed"
        );
    }

    #[test]
    fn test_address_serialization_rlp() {
        let addr = Address::repeat_byte(0xAB);
        let mut buf = Vec::new();
        addr.encode(&mut buf);
        let mut buf_ref = buf.as_slice();
        let decoded = Address::decode(&mut buf_ref).expect("valid rlp");
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_address_serialization_json() {
        let addr = Address::repeat_byte(0xAB);
        let json = serde_json::to_string(&addr).expect("serialize");
        let decoded: Address = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_fee_currency_equality() {
        let c1 = FeeCurrency::Call;
        let c2 = FeeCurrency::Call;
        assert_eq!(c1, c2);

        let s1 = FeeCurrency::Stablecoin(1);
        let s2 = FeeCurrency::Stablecoin(1);
        assert_eq!(s1, s2);
        assert_ne!(c1, s1);
    }

    #[test]
    fn test_type_alias_sizes() {
        assert_eq!(std::mem::size_of::<TxHash>(), 32);
        assert_eq!(std::mem::size_of::<BlockHash>(), 32);
        assert_eq!(std::mem::size_of::<AssetId>(), 8);
        assert_eq!(std::mem::size_of::<Balance>(), 16);
        assert_eq!(std::mem::size_of::<ValidatorId>(), 4);
        assert_eq!(std::mem::size_of::<Nonce>(), 8);
        assert_eq!(std::mem::size_of::<Signature>(), 65);
        assert_eq!(std::mem::size_of::<PublicKey>(), 64);
        assert_eq!(std::mem::size_of::<Ed25519PublicKey>(), 32);
    }

    #[test]
    fn test_execution_status() {
        let ok = ExecutionStatus::Success;
        assert!(ok.is_success());
        assert!(!ok.is_reverted());

        let fail = ExecutionStatus::Reverted {
            reason: "out of gas".into(),
        };
        assert!(!fail.is_success());
        assert!(fail.is_reverted());
    }

    #[test]
    fn test_protocol_version() {
        let v = ProtocolVersion::new(1, 2, 3);
        assert_eq!(v.to_string(), "1.2.3");
        assert_eq!(v, ProtocolVersion::new(1, 2, 3));
    }

    #[test]
    fn test_minimum_unit() {
        assert_eq!(MINIMUM_UNIT, 1);
    }

    #[test]
    fn test_price_pair_call_usd() {
        let pair = PricePair::CALL_USD;
        assert_eq!(pair.base, 1);
        assert_eq!(pair.quote, 0);
    }

    #[test]
    fn test_price_pair_equality() {
        let p1 = PricePair::new(2, 0);
        let p2 = PricePair::new(2, 0);
        let p3 = PricePair::new(2, 1);
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
    }

    #[test]
    fn test_price_pair_serialization() {
        let pair = PricePair::new(5, 0);
        let json = serde_json::to_string(&pair).unwrap();
        let decoded: PricePair = serde_json::from_str(&json).unwrap();
        assert_eq!(pair, decoded);
    }

    // ── Property-based tests (proptest) ───────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_price_pair_json_roundtrip(base in any::<u64>(), quote in any::<u64>()) {
            let pair = PricePair::new(base, quote);
            let json = serde_json::to_string(&pair).unwrap();
            let decoded: PricePair = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(pair, decoded);
        }

        #[test]
        fn prop_protocol_version_rlp_roundtrip(
            major in any::<u16>(),
            minor in any::<u16>(),
            patch in any::<u16>(),
        ) {
            let version = ProtocolVersion::new(major, minor, patch);
            let mut buf = Vec::new();
            version.encode(&mut buf);
            let decoded = ProtocolVersion::decode(&mut buf.as_slice()).unwrap();
            prop_assert_eq!(version, decoded);
        }

        #[test]
        fn prop_protocol_version_json_roundtrip(
            major in any::<u16>(),
            minor in any::<u16>(),
            patch in any::<u16>(),
        ) {
            let version = ProtocolVersion::new(major, minor, patch);
            let json = serde_json::to_string(&version).unwrap();
            let decoded: ProtocolVersion = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(version, decoded);
        }
    }
}
