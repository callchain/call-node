//! Callchain Protocol Layer — Core protocol types, execution, and economics.

pub mod security;
pub mod oracle;
pub mod registry;
pub mod balances;
pub mod compliance;
pub mod instructions;
pub mod transaction;
pub mod smart_accounts;
pub mod fee_currency;
pub mod sponsor;
pub mod receipts;
pub mod issuer;
pub mod economics;
pub mod governance;

pub use governance::*;

pub use registry::*;
pub use balances::*;
pub use compliance::*;
pub use instructions::*;
pub use transaction::*;
pub use smart_accounts::*;
pub use fee_currency::*;
pub use sponsor::*;
pub use receipts::*;
pub use issuer::*;
pub use economics::*;
pub use oracle::*;

use thiserror::Error;

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
}

/// Result type alias for protocol operations
pub type ProtocolResult<T> = Result<T, ProtocolError>;
