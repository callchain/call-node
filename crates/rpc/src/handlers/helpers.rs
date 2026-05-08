//! Helper functions and response types.

use call_primitives::{Address, AssetId, Balance, Hash};
use jsonrpsee::types::ErrorObjectOwned;

// ── Structured Error Codes ───────────────────────────────────────────

/// Callchain application-level JSON-RPC error codes.
///
/// Uses the -32000 to -32099 range reserved for server-defined errors
/// per the JSON-RPC 2.0 specification. Standard codes (-32600 to -32603)
/// are kept for protocol-level issues (parse error, invalid request,
/// method not found, invalid params, internal error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcErrorCode {
    /// Generic application internal error (-32000).
    InternalError = -32000,
    /// EVM execution reverted (-32001).
    ExecutionReverted = -32001,
    /// Resource temporarily unavailable, e.g. lock poisoned (-32002).
    ResourceUnavailable = -32002,
    /// Database or persistence layer error (-32003).
    DatabaseError = -32003,
    /// Method or feature not available in current configuration (-32004).
    MethodNotAvailable = -32004,
    /// Transaction validation failed (-32005).
    TransactionValidationFailed = -32005,
    /// Filter not found (-32006).
    FilterNotFound = -32006,
    /// Light client verification failed (-32007).
    LightClientVerificationFailed = -32007,
    /// Invalid hex / base16 decoding (-32010).
    InvalidHex = -32010,
}

impl RpcErrorCode {
    pub const fn code(self) -> i32 {
        self as i32
    }
}

/// Build a structured `ErrorObjectOwned` with a machine-readable code.
pub fn rpc_error(code: RpcErrorCode, msg: impl Into<String>) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(code.code(), msg.into(), None::<()>)
}

/// Helper: create an invalid params error (standard JSON-RPC -32602).
pub fn invalid_params(msg: impl Into<String>) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32602, msg.into(), None::<()>)
}

/// Helper: create an internal error (application-level -32000).
///
/// Previously used -32603 (generic JSON-RPC internal error). Changed to
/// -32000 so that -32603 is reserved for transport/server failures and
/// -32000 signals an application-layer internal problem.
pub fn internal_error(msg: impl Into<String>) -> ErrorObjectOwned {
    rpc_error(RpcErrorCode::InternalError, msg)
}

/// Helper: create an execution reverted error (-32001).
pub fn execution_reverted(msg: impl Into<String>) -> ErrorObjectOwned {
    rpc_error(RpcErrorCode::ExecutionReverted, msg)
}

/// Helper: create a resource-unavailable error (-32002).
pub fn resource_unavailable(msg: impl Into<String>) -> ErrorObjectOwned {
    rpc_error(RpcErrorCode::ResourceUnavailable, msg)
}

/// Helper: create a database error (-32003).
pub fn db_error(msg: impl Into<String>) -> ErrorObjectOwned {
    rpc_error(RpcErrorCode::DatabaseError, msg)
}

/// Helper: create a method-not-available error (-32004).
pub fn method_not_available(msg: impl Into<String>) -> ErrorObjectOwned {
    rpc_error(RpcErrorCode::MethodNotAvailable, msg)
}

/// Helper: create a transaction-validation-failed error (-32005).
pub fn tx_validation_failed(msg: impl Into<String>) -> ErrorObjectOwned {
    rpc_error(RpcErrorCode::TransactionValidationFailed, msg)
}

/// Helper: create a filter-not-found error (-32006).
pub fn filter_not_found(msg: impl Into<String>) -> ErrorObjectOwned {
    rpc_error(RpcErrorCode::FilterNotFound, msg)
}

/// Helper: create a light-client-verification-failed error (-32007).
pub fn light_client_verification_failed(msg: impl Into<String>) -> ErrorObjectOwned {
    rpc_error(RpcErrorCode::LightClientVerificationFailed, msg)
}

// ── Response types

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
