//! Oracle System (per spec §25)
//!
//! Validator-submitted price feeds with quorum-based aggregation,
//! outlier detection, and TWAP history.

pub mod constants;
pub mod tracker;
pub mod crypto;
pub mod fetcher;
pub mod precompile;
#[cfg(test)]
pub mod tests;

pub use constants::*;
pub use tracker::OracleTracker;
pub use crypto::*;
pub use fetcher::*;

use call_primitives::{Address, Ed25519PublicKey, PricePair};
use serde::{Deserialize, Serialize};

/// A single price submission from a validator
#[derive(Debug, Clone)]
pub struct OracleSubmission {
    pub validator_id: u32,
    pub pair: PricePair,
    pub price: u128,
    pub block_number: u64,
    pub timestamp: u64,
    pub signature: [u8; 64],
    /// Data sources attesting to this price (e.g. "binance", "coinbase")
    pub sources: Vec<String>,
}

/// Aggregated price after quorum
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedPrice {
    pub pair: PricePair,
    pub median_price: u128,
    pub block_number: u64,
    pub timestamp: u64,
    pub submission_count: usize,
    pub outlier_count: usize,
}

/// Per-validator oracle tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleValidatorInfo {
    pub validator_id: u32,
    pub address: Address,
    pub public_key: Ed25519PublicKey,
    pub is_active: bool,
    pub outlier_count: u32,
    pub last_submission_block: u64,
    pub submission_count: u64,
}

/// Historical price entry for TWAP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoricalPrice {
    pub price: u128,
    pub timestamp: u64,
    pub block_number: u64,
}

/// Oracle configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleConfig {
    pub update_interval: u64,
    pub outlier_threshold_bps: u64,
    pub outlier_tolerance: u32,
    pub twap_window_secs: u64,
    pub staleness_secs: u64,
    pub min_data_sources: usize,
    /// Allowed data source names (empty = no restriction)
    pub allowed_sources: Vec<String>,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            update_interval: constants::ORACLE_UPDATE_INTERVAL,
            outlier_threshold_bps: constants::ORACLE_OUTLIER_THRESHOLD_BPS,
            outlier_tolerance: constants::ORACLE_OUTLIER_TOLERANCE,
            twap_window_secs: constants::ORACLE_TWAP_WINDOW_SECS,
            staleness_secs: constants::ORACLE_STALENESS_SECS,
            min_data_sources: constants::ORACLE_MIN_DATA_SOURCES,
            allowed_sources: Vec::new(),
        }
    }
}

/// Oracle error types
#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    #[error("validator not found")]
    ValidatorNotFound,
    #[error("validator disabled")]
    ValidatorDisabled,
    #[error("duplicate submission")]
    DuplicateSubmission,
    #[error("wrong period")]
    WrongPeriod,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("no submissions")]
    NoSubmissions,
    #[error("insufficient data sources: got {0}, need {1}")]
    InsufficientDataSources(usize, usize),
    #[error("disallowed data source: {0}")]
    DisallowedSource(String),
    #[error("validator is not disabled")]
    ValidatorNotDisabled,
}
