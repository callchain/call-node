//! Callchain primitives — shared base types

use alloy_primitives::{Address, B256, U256, FixedBytes};

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
