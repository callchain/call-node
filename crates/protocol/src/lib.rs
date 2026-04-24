//! Callchain Protocol Layer — Core protocol types, execution, and economics.

pub mod security;
pub mod registry;
pub mod account;
pub mod compliance;
pub mod instructions;
pub mod transaction;
pub mod smart_accounts;
pub mod fee_currency;
pub mod sponsor;
pub mod receipts;
pub mod issuer;
pub mod economics;

pub use registry::*;
pub use account::*;
pub use compliance::*;
pub use instructions::*;
pub use transaction::*;
pub use smart_accounts::*;
pub use fee_currency::*;
pub use sponsor::*;
pub use receipts::*;
pub use issuer::*;
pub use economics::*;

use alloy_primitives::Address;
use thiserror::Error;

/// Asset ID for CALL (native token)
pub const CALL_ASSET_ID: u64 = 1;

/// Fixed EVM address used by the protocol bridge for bridgeMint operations.
pub const BRIDGE_EVM_ADDRESS: Address = Address::repeat_byte(0xFF);

/// Error type for protocol operations
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("unauthorized")]
    Unauthorized,
    #[error("nonce error: {0}")]
    NonceError(String),
    #[error("compliance check failed: {0}")]
    Compliance(String),
    #[error("invalid instruction: {0}")]
    InvalidInstruction(String),
    #[error("gas error: {0}")]
    GasError(String),
    #[error("balance error: {0}")]
    BalanceError(String),
    #[error("sponsor error: {0}")]
    SponsorError(String),
    #[error("asset error: {0}")]
    AssetError(String),
    #[error("registry error: {0}")]
    RegistryError(String),
    #[error("receipt error: {0}")]
    ReceiptError(String),
    #[error("economics error: {0}")]
    Economics(String),
    #[error("recovery error: {0}")]
    Recovery(String),
    #[error("session key error: {0}")]
    SessionKey(String),
    #[error("invalid signature: {0}")]
    InvalidSignature(String),
}

/// Result type alias for protocol operations
pub type ProtocolResult<T> = Result<T, ProtocolError>;
