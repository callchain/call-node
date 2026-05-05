//! EVM account data type.

use alloy_primitives::{U256, Bytes};
use std::collections::HashMap;

/// EVM account info.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvmAccount {
    pub nonce: u64,
    pub balance: U256,
    pub code: Bytes,
    pub storage: HashMap<U256, U256>,
}

impl Default for EvmAccount {
    fn default() -> Self {
        Self {
            nonce: 0,
            balance: U256::ZERO,
            code: Bytes::default(),
            storage: HashMap::new(),
        }
    }
}
