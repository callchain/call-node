//! Helper functions and response types.

use call_primitives::{Address, AssetId, Balance, Hash};
use jsonrpsee::types::ErrorObjectOwned;

/// Helper: create an invalid params error
pub fn invalid_params(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32602, msg, None::<()>)
}

/// Helper: create an internal error
pub fn internal_error(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32603, msg, None::<()>)
}

/// Response types

#[derive(Debug, Clone, serde::Serialize)]
pub struct AssetInfoResponse {
    pub id: AssetId,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub issuer: Address,
    pub protocol_supply: Balance,
    pub evm_supply: Balance,
    pub all_supply: Balance,
    pub max_supply: Balance,
    pub status: String,
    pub compliance_policy: u8,
    pub registered_at: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentInfoResponse {
    pub agent_id: u64,
    pub owner: Address,
    pub name: String,
    pub url: String,
    pub domain_verified: bool,
    pub registered_at: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShieldedTreeStateResponse {
    pub merkle_root: Hash,
    pub leaf_count: u64,
    pub nullifier_count: usize,
}
