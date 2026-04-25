//! Oracle System (per spec §25)
//!
//! Validator-submitted price feeds with quorum-based aggregation,
//! outlier detection, and TWAP history.

use alloy_primitives::U256;
use call_crypto::ed25519_sign;
use call_crypto::ed25519_verify;
use call_primitives::{Address, AssetId, Ed25519PublicKey, PricePair};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Constants (per spec §25.5) ─────────────────────────────────

/// Blocks between oracle price updates
pub const ORACLE_UPDATE_INTERVAL: u64 = 1000;
/// Oracle period in seconds (used for submission validation)
pub const ORACLE_PERIOD_SECS: u64 = 240;
/// Outlier threshold: 5% deviation from median
pub const ORACLE_OUTLIER_THRESHOLD_BPS: u64 = 500;
/// Strikes before validator is disabled
pub const ORACLE_OUTLIER_TOLERANCE: u32 = 10;
/// TWAP window max: 24 hours
pub const ORACLE_TWAP_WINDOW_SECS: u64 = 86_400;
/// Price staleness threshold: 900 seconds (15 min)
pub const ORACLE_STALENESS_SECS: u64 = 900;
/// Minimum independent data sources per validator
pub const ORACLE_MIN_DATA_SOURCES: usize = 2;

/// Compute the oracle quorum from the number of active validators.
/// Returns ceil(2/3 * n), minimum 2, capped at n.
pub fn oracle_quorum(active_count: usize) -> usize {
    if active_count == 0 {
        return 0;
    }
    ((2 * active_count + 2) / 3).min(active_count).max(1)
}

// ─── Types ──────────────────────────────────────────────────────

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
            update_interval: ORACLE_UPDATE_INTERVAL,
            outlier_threshold_bps: ORACLE_OUTLIER_THRESHOLD_BPS,
            outlier_tolerance: ORACLE_OUTLIER_TOLERANCE,
            twap_window_secs: ORACLE_TWAP_WINDOW_SECS,
            staleness_secs: ORACLE_STALENESS_SECS,
            min_data_sources: ORACLE_MIN_DATA_SOURCES,
            allowed_sources: Vec::new(),
        }
    }
}

// ─── Oracle State ───────────────────────────────────────────────

/// Full oracle state manager
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleManager {
    pub config: OracleConfig,
    validators: HashMap<u32, OracleValidatorInfo>,
    /// Reverse lookup: validator address -> validator_id
    validator_ids_by_address: HashMap<Address, u32>,
    /// Price pairs to track for oracle submissions
    pub tracked_pairs: Vec<PricePair>,
    /// Pending submissions for current period: pair -> (validator_id -> submission)
    #[serde(skip)]
    pending: HashMap<PricePair, HashMap<u32, OracleSubmission>>,
    /// Current aggregated prices per pair
    aggregated: HashMap<PricePair, AggregatedPrice>,
    /// Historical prices for TWAP: pair -> Vec<HistoricalPrice>
    history: HashMap<PricePair, Vec<HistoricalPrice>>,
    current_block: u64,
    /// Accumulated fee pool for oracle rewards (reset each period)
    pub reward_pool: u128,
    /// Validators who contributed to the last quorum aggregation
    #[serde(skip)]
    pub current_contributors: Vec<u32>,
    /// Last round's outlier validators (for slashing)
    #[serde(skip)]
    last_outliers: Vec<u32>,
}

impl Default for OracleManager {
    fn default() -> Self {
        Self::new(OracleConfig::default())
    }
}

impl OracleManager {
    pub fn new(config: OracleConfig) -> Self {
        Self {
            config,
            validators: HashMap::new(),
            validator_ids_by_address: HashMap::new(),
            tracked_pairs: Vec::new(),
            pending: HashMap::new(),
            aggregated: HashMap::new(),
            history: HashMap::new(),
            current_block: 0,
            reward_pool: 0,
            current_contributors: Vec::new(),
            last_outliers: Vec::new(),
        }
    }

    /// Update the oracle configuration
    pub fn update_config(&mut self, config: OracleConfig) {
        self.config = config;
    }

    /// Register a validator for oracle submissions
    pub fn register_validator(&mut self, validator_id: u32, address: Address, public_key: Ed25519PublicKey) {
        self.validators.insert(
            validator_id,
            OracleValidatorInfo {
                validator_id,
                address,
                public_key,
                is_active: true,
                outlier_count: 0,
                last_submission_block: 0,
                submission_count: 0,
            },
        );
        self.validator_ids_by_address.insert(address, validator_id);
    }

    /// Look up validator_id by address
    pub fn validator_id_by_address(&self, address: Address) -> Option<u32> {
        self.validator_ids_by_address.get(&address).copied()
    }

    /// Set the list of price pairs to track for oracle submissions
    pub fn set_tracked_pairs(&mut self, pairs: Vec<PricePair>) {
        self.tracked_pairs = pairs;
    }

    /// Legacy compatibility: set tracked assets, all quoted in USD (asset_id 0)
    pub fn set_tracked_assets(&mut self, asset_ids: Vec<AssetId>) {
        self.tracked_pairs = asset_ids
            .into_iter()
            .map(|id| PricePair::new(id, 0))
            .collect();
    }

    /// Submit a price from a validator.
    /// Returns Ok(()) if accepted, Err if rejected.
    pub fn submit_price(&mut self, submission: OracleSubmission) -> Result<(), OracleError> {
        let validator = self
            .validators
            .get(&submission.validator_id)
            .ok_or(OracleError::ValidatorNotFound)?;

        if !validator.is_active {
            return Err(OracleError::ValidatorDisabled);
        }

        // Dedup: same validator, same block
        if validator.last_submission_block == submission.block_number {
            return Err(OracleError::DuplicateSubmission);
        }

        // Period check: only accept at update interval boundaries
        if !submission.block_number.is_multiple_of(self.config.update_interval) {
            return Err(OracleError::WrongPeriod);
        }

        // Verify Ed25519 signature
        let message = oracle_message_hash(
            submission.validator_id,
            submission.pair,
            submission.price,
            submission.block_number,
            submission.timestamp,
        );
        ed25519_verify(&validator.public_key, &submission.signature, &message)
            .map_err(|_| OracleError::InvalidSignature)?;

        // Data source attestation: require minimum sources and allowlist check
        if submission.sources.len() < self.config.min_data_sources {
            return Err(OracleError::InsufficientDataSources(
                submission.sources.len(),
                self.config.min_data_sources,
            ));
        }
        if !self.config.allowed_sources.is_empty() {
            for source in &submission.sources {
                if !self.config.allowed_sources.contains(source) {
                    return Err(OracleError::DisallowedSource(source.clone()));
                }
            }
        }

        // Accept submission
        let pair = submission.pair;
        let validator_id = submission.validator_id;
        let block_number = submission.block_number;
        self.pending
            .entry(pair)
            .or_default()
            .insert(validator_id, submission);

        // Update validator info
        if let Some(v) = self.validators.get_mut(&validator_id) {
            v.last_submission_block = block_number;
            v.submission_count += 1;
        }

        // Check if quorum reached
        let submissions = self.pending.get(&pair).unwrap();
        if submissions.len() >= oracle_quorum(self.validators.len()) {
            self.aggregate_and_publish_price(pair)?;
        }

        Ok(())
    }

    /// Aggregate submissions: sort, compute median, mark outliers, append to TWAP history
    fn aggregate_and_publish_price(&mut self, pair: PricePair) -> Result<(), OracleError> {
        let submissions = self
            .pending
            .remove(&pair)
            .ok_or(OracleError::NoSubmissions)?;

        if submissions.is_empty() {
            return Err(OracleError::NoSubmissions);
        }

        let mut prices: Vec<(u32, u128)> = submissions
            .values()
            .map(|s| (s.validator_id, s.price))
            .collect();
        prices.sort_by_key(|&(_, p)| p);

        let median = prices[prices.len() / 2].1;

        // Detect outliers (>5% deviation from median)
        let threshold_bps = self.config.outlier_threshold_bps;
        let mut outlier_count = 0;
        let outlier_validators: Vec<u32> = prices
            .iter()
            .filter_map(|&(vid, price)| {
                if median == 0 {
                    return None;
                }
                let deviation_bps = if price > median {
                    ((price - median) * 10_000) / median
                } else {
                    ((median - price) * 10_000) / median
                };
                if deviation_bps > threshold_bps as u128 {
                    outlier_count += 1;
                    Some(vid)
                } else {
                    None
                }
            })
            .collect();

        // Strike outlier validators
        for vid in &outlier_validators {
            if let Some(v) = self.validators.get_mut(vid) {
                v.outlier_count += 1;
                if v.outlier_count >= self.config.outlier_tolerance {
                    v.is_active = false;
                }
            }
        }

        let first_ts = submissions.values().map(|s| s.timestamp).min().unwrap_or(0);
        let block = submissions.values().map(|s| s.block_number).next().unwrap_or(0);

        let aggregated = AggregatedPrice {
            pair,
            median_price: median,
            block_number: block,
            timestamp: first_ts,
            submission_count: submissions.len(),
            outlier_count,
        };

        // Store aggregated price
        self.aggregated.insert(pair, aggregated);

        // Record contributors (non-outlier validators who contributed to quorum)
        self.current_contributors = prices
            .iter()
            .filter_map(|&(vid, _)| {
                if !outlier_validators.contains(&vid) {
                    Some(vid)
                } else {
                    None
                }
            })
            .collect();

        // Record outliers for external slashing
        self.last_outliers = outlier_validators;

        // Append to TWAP history
        self.history
            .entry(pair)
            .or_default()
            .push(HistoricalPrice {
                price: median,
                timestamp: first_ts,
                block_number: block,
            });

        // Prune TWAP history beyond window
        if let Some(entries) = self.history.get_mut(&pair) {
            if entries.len() > 1 {
                let cutoff = first_ts.saturating_sub(self.config.twap_window_secs);
                entries.retain(|e| e.timestamp >= cutoff);
            }
        }

        Ok(())
    }

    /// Get current aggregated price for a pair
    pub fn get_price(&self, pair: PricePair) -> Option<&AggregatedPrice> {
        self.aggregated.get(&pair)
    }

    /// Legacy compatibility: get price by asset_id, implicitly quoted in USD
    pub fn get_price_by_asset(&self, asset_id: AssetId) -> Option<&AggregatedPrice> {
        self.get_price(PricePair::new(asset_id, 0))
    }

    /// Calculate time-weighted average price over the configured window.
    ///
    /// Each historical price is weighted by the duration it was valid
    /// (time until the next price update, or until `current_timestamp`
    /// for the most recent entry).
    pub fn get_twap(&self, pair: PricePair, current_timestamp: u64) -> Option<u128> {
        let entries = self.history.get(&pair)?;
        if entries.is_empty() {
            return None;
        }
        let cutoff = current_timestamp.saturating_sub(self.config.twap_window_secs);
        let relevant: Vec<_> = entries.iter().filter(|e| e.timestamp >= cutoff).collect();
        if relevant.is_empty() {
            return None;
        }
        if relevant.len() == 1 {
            return Some(relevant[0].price);
        }

        let mut weighted_sum = U256::ZERO;
        let mut total_duration: u64 = 0;

        for i in 0..relevant.len() {
            let start = relevant[i].timestamp;
            let end = if i + 1 < relevant.len() {
                relevant[i + 1].timestamp
            } else {
                current_timestamp.min(start + self.config.twap_window_secs)
            };
            let duration = end.saturating_sub(start);
            if duration > 0 {
                weighted_sum += U256::from(relevant[i].price) * U256::from(duration);
                total_duration += duration;
            }
        }

        if total_duration == 0 {
            return relevant.first().map(|e| e.price);
        }

        let result = weighted_sum / U256::from(total_duration);
        u128::try_from(&result).ok()
    }

    /// Legacy compatibility: get TWAP by asset_id, implicitly quoted in USD
    pub fn get_twap_by_asset(&self, asset_id: AssetId, current_timestamp: u64) -> Option<u128> {
        self.get_twap(PricePair::new(asset_id, 0), current_timestamp)
    }

    /// Check if a price is stale
    pub fn is_stale(&self, pair: PricePair, current_timestamp: u64) -> bool {
        match self.aggregated.get(&pair) {
            Some(p) => current_timestamp.saturating_sub(p.timestamp) > self.config.staleness_secs,
            None => true,
        }
    }

    /// Legacy compatibility: check staleness by asset_id, implicitly quoted in USD
    pub fn is_stale_by_asset(&self, asset_id: AssetId, current_timestamp: u64) -> bool {
        self.is_stale(PricePair::new(asset_id, 0), current_timestamp)
    }

    /// Get validator info
    pub fn get_validator_info(&self, validator_id: u32) -> Option<&OracleValidatorInfo> {
        self.validators.get(&validator_id)
    }

    /// Advance block number (for period tracking)
    pub fn set_current_block(&mut self, block: u64) {
        self.current_block = block;
    }

    /// Get pending submission count for a pair
    pub fn pending_count(&self, pair: PricePair) -> usize {
        self.pending.get(&pair).map(|m| m.len()).unwrap_or(0)
    }

    /// Add to the oracle reward pool
    pub fn add_reward(&mut self, amount: u128) {
        self.reward_pool += amount;
    }

    /// Get the last round's outlier validator IDs (for slashing)
    pub fn last_outliers(&self) -> &[u32] {
        &self.last_outliers
    }

    /// Clear contributor/outlier tracking (called at start of new period)
    pub fn clear_tracking(&mut self) {
        self.current_contributors.clear();
        self.last_outliers.clear();
    }

    /// Advance the oracle period for all tracked pairs.
    ///
    /// Called by the block proposer at each `ORACLE_UPDATE_INTERVAL` boundary.
    /// For pairs where quorum was not reached, the last known price is carried
    /// forward (graceful degradation) so the oracle never stalls.
    pub fn advance_period(&mut self, block: u64) {
        for pair in &self.tracked_pairs.clone() {
            let has_quorum = self
                .pending
                .get(pair)
                .map(|p| p.len() >= oracle_quorum(self.validators.len()))
                .unwrap_or(false);

            if !has_quorum {
                // Carry forward the last known price for this pair
                if let Some(last) = self.aggregated.get(pair).cloned() {
                    self.history
                        .entry(*pair)
                        .or_default()
                        .push(HistoricalPrice {
                            price: last.median_price,
                            timestamp: last.timestamp,
                            block_number: block,
                        });
                }
            }
            // If quorum was reached, aggregate_and_publish_price was already
            // called during submission — just prune history
            if let Some(entries) = self.history.get_mut(pair) {
                if entries.len() > 1 {
                    let cutoff_ts = entries
                        .last()
                        .map(|e| e.timestamp)
                        .unwrap_or(0)
                        .saturating_sub(self.config.twap_window_secs);
                    entries.retain(|e| e.timestamp >= cutoff_ts);
                }
            }
        }
        // Clear pending for all pairs
        self.pending.clear();
    }

    /// Distribute the oracle reward pool proportionally to validators who
    /// contributed to the last quorum aggregation.
    ///
    /// Returns a list of (validator_id, reward_amount) and resets the pool to 0.
    pub fn distribute_rewards(&mut self) -> Vec<(u32, u128)> {
        let total = self.reward_pool;
        if total == 0 || self.current_contributors.is_empty() {
            self.reward_pool = 0;
            return Vec::new();
        }

        let count = self.current_contributors.len() as u128;
        let per_validator = total / count;
        let remainder = total % count;

        let rewards: Vec<(u32, u128)> = self
            .current_contributors
            .iter()
            .enumerate()
            .map(|(i, vid)| {
                // First validator gets the remainder to avoid losing dust
                let amount = if i == 0 {
                    per_validator + remainder
                } else {
                    per_validator
                };
                (*vid, amount)
            })
            .collect();

        self.reward_pool = 0;
        rewards
    }

    /// Reset a disabled validator's oracle status, allowing it to participate again.
    ///
    /// Can only be called when the validator is currently disabled (`is_active == false`).
    /// Resets outlier_count to 0 and re-enables the validator.
    pub fn reset_validator(&mut self, validator_id: u32) -> Result<(), OracleError> {
        let validator = self
            .validators
            .get_mut(&validator_id)
            .ok_or(OracleError::ValidatorNotFound)?;

        if validator.is_active {
            return Err(OracleError::ValidatorNotDisabled);
        }

        validator.outlier_count = 0;
        validator.is_active = true;
        Ok(())
    }

    /// Simple price submission — internal/testing only. Bypasses the full validation
    /// pipeline (signatures, period checks, source validation). Performs minimal
    /// sanity checks to prevent obviously invalid data.
    ///
    /// Only available in test builds. Production code must use `submit_price`.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub fn simple_submit_price(
        &mut self,
        pair: PricePair,
        price: u128,
        timestamp: u64,
        block_number: u64,
    ) {
        if price == 0 || block_number == 0 || timestamp == 0 {
            return;
        }
        self.history
            .entry(pair)
            .or_default()
            .push(HistoricalPrice {
                price,
                timestamp,
                block_number,
            });

        // Update current aggregated price
        self.aggregated.insert(
            pair,
            AggregatedPrice {
                pair,
                median_price: price,
                block_number,
                timestamp,
                submission_count: 1,
                outlier_count: 0,
            },
        );
    }

    /// Legacy compatibility: simple_submit_price by asset_id, implicitly quoted in USD
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub fn simple_submit_price_by_asset(
        &mut self,
        asset_id: AssetId,
        price: u128,
        timestamp: u64,
        block_number: u64,
    ) {
        self.simple_submit_price(PricePair::new(asset_id, 0), price, timestamp, block_number);
    }

    /// Directly record a price in history and aggregated state.
    /// Used by the precompiles crate for legacy integrations.
    /// Bypasses the full validation pipeline — use with caution.
    pub fn record_direct_price(
        &mut self,
        pair: PricePair,
        price: u128,
        timestamp: u64,
        block_number: u64,
    ) {
        if price == 0 || block_number == 0 || timestamp == 0 {
            return;
        }
        self.history
            .entry(pair)
            .or_default()
            .push(HistoricalPrice {
                price,
                timestamp,
                block_number,
            });
        self.aggregated.insert(
            pair,
            AggregatedPrice {
                pair,
                median_price: price,
                block_number,
                timestamp,
                submission_count: 1,
                outlier_count: 0,
            },
        );
    }

    /// Legacy compatibility: record_direct_price by asset_id, implicitly quoted in USD
    pub fn record_direct_price_by_asset(
        &mut self,
        asset_id: AssetId,
        price: u128,
        timestamp: u64,
        block_number: u64,
    ) {
        self.record_direct_price(PricePair::new(asset_id, 0), price, timestamp, block_number);
    }
}

// ─── Helpers ────────────────────────────────────────────────────

/// Create a canonical message hash for oracle submissions
pub fn oracle_message_hash(
    validator_id: u32,
    pair: PricePair,
    price: u128,
    block_number: u64,
    timestamp: u64,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(4 + 16 + 16 + 8 + 8);
    msg.extend_from_slice(&validator_id.to_le_bytes());
    msg.extend_from_slice(&pair.base.to_le_bytes());
    msg.extend_from_slice(&pair.quote.to_le_bytes());
    msg.extend_from_slice(&price.to_le_bytes());
    msg.extend_from_slice(&block_number.to_le_bytes());
    msg.extend_from_slice(&timestamp.to_le_bytes());
    msg
}

/// Sign an oracle submission message
pub fn sign_oracle_submission(
    signing_key: &SigningKey,
    validator_id: u32,
    pair: PricePair,
    price: u128,
    block_number: u64,
    timestamp: u64,
) -> [u8; 64] {
    let message = oracle_message_hash(validator_id, pair, price, block_number, timestamp);
    ed25519_sign(signing_key, &message)
}

// ─── Errors ─────────────────────────────────────────────────────

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

// ─── Price Fetcher Trait ────────────────────────────────────────────

/// Trait for fetching prices from external data sources.
/// Validators implement this to provide real-time price data
/// for oracle submissions.
pub trait PriceFetcher: Send + Sync {
    /// Fetch the current price for a pair. Returns price in smallest units.
    fn fetch_price(&self, pair: PricePair) -> Option<u128>;
    /// Data source names this fetcher uses (e.g., ["binance", "coinbase"])
    fn sources(&self) -> Vec<String>;
}

/// No-op price fetcher — used in devnet or when no external API is configured.
pub struct NoOpPriceFetcher;

impl PriceFetcher for NoOpPriceFetcher {
    fn fetch_price(&self, _pair: PricePair) -> Option<u128> {
        None
    }

    fn sources(&self) -> Vec<String> {
        Vec::new()
    }
}

/// HTTP-based price fetcher for production use.
/// Maps asset IDs to API endpoint URLs and queries them on demand.
///
/// Requires the `http-fetcher` feature flag.
#[cfg(feature = "http-fetcher")]
pub struct HttpPriceFetcher {
    endpoints: std::collections::HashMap<AssetId, String>,
    client: reqwest::Client,
}

#[cfg(feature = "http-fetcher")]
impl HttpPriceFetcher {
    /// Create a new HttpPriceFetcher with configured endpoints.
    /// Each asset_id maps to an HTTP URL that returns a JSON object
    /// with a `"price"` field containing a numeric string or float.
    pub fn new(endpoints: std::collections::HashMap<AssetId, String>) -> Self {
        Self {
            endpoints,
            client: reqwest::Client::new(),
        }
    }

    /// Add an endpoint for a specific asset.
    pub fn with_endpoint(mut self, asset_id: AssetId, url: String) -> Self {
        self.endpoints.insert(asset_id, url);
        self
    }
}

#[cfg(feature = "http-fetcher")]
impl PriceFetcher for HttpPriceFetcher {
    fn fetch_price(&self, pair: PricePair) -> Option<u128> {
        let url = self.endpoints.get(&pair.base)?;

        // Blocking call — acceptable in the oracle context where we
        // already have a configurable delay window.
        let rt = tokio::runtime::Handle::try_current().ok()?;
        let future = async {
            let resp = self.client.get(url).send().await.ok()?;
            let body: serde_json::Value = resp.json().await.ok()?;
            // Support common formats: {"price": "123.45"}, {"lastPrice": "123.45"},
            // or a plain number
            let price_str = body.get("price")
                .or_else(|| body.get("lastPrice"))
                .or_else(|| body.get("last"))?
                .as_str()?;
            // Parse as f64 then convert to u128 (price in smallest units)
            price_str.parse::<f64>().ok().map(|f| f as u128)
        };

        tokio::task::block_in_place(|| rt.block_on(future))
    }

    fn sources(&self) -> Vec<String> {
        vec!["http".into()]
    }
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_crypto::ed25519_generate_keypair;

    fn make_manager() -> (OracleManager, Vec<(u32, Ed25519PublicKey, SigningKey)>) {
        let mut config = OracleConfig::default();
        config.min_data_sources = 0; // tests don't need real data sources
        let mut manager = OracleManager::new(config);
        let mut validators = Vec::new();

        // Register 6 validators (quorum = ceil(2/3*6) = 4)
        for i in 0..6 {
            let (pubkey, signing_key) = ed25519_generate_keypair();
            let vid = i as u32;
            let address = Address::repeat_byte(i as u8);
            manager.register_validator(vid, address, pubkey);
            validators.push((vid, pubkey, signing_key));
        }

        (manager, validators)
    }

    fn submit_all(
        manager: &mut OracleManager,
        validators: &[(u32, Ed25519PublicKey, SigningKey)],
        pair: PricePair,
        block: u64,
        price: u128,
    ) {
        for (vid, _, signing_key) in validators {
            let timestamp = block * 1000;
            let sig = sign_oracle_submission(signing_key, *vid, pair, price, block, timestamp);
            let _ = manager.submit_price(OracleSubmission {
                validator_id: *vid,
                pair,
                price,
                block_number: block,
                timestamp,
                signature: sig,
                sources: Vec::new(),
            });
        }
    }

    fn quorum_for(validators: &[(u32, Ed25519PublicKey, SigningKey)]) -> usize {
        oracle_quorum(validators.len())
    }

    #[test]
    fn test_oracle_quorum_function() {
        assert_eq!(oracle_quorum(1), 1);  // single validator
        assert_eq!(oracle_quorum(2), 2);  // 2 validators, both required
        assert_eq!(oracle_quorum(3), 2);  // ceil(2/3*3) = 2
        assert_eq!(oracle_quorum(4), 3);  // ceil(2/3*4) = 3
        assert_eq!(oracle_quorum(6), 4);  // ceil(2/3*6) = 4
        assert_eq!(oracle_quorum(21), 14); // production subset
        assert_eq!(oracle_quorum(216), 144);
    }

    fn submit_price_for_pair(
        manager: &mut OracleManager,
        validators: &[(u32, Ed25519PublicKey, SigningKey)],
        pair: PricePair,
        block: u64,
        price: u128,
    ) {
        submit_all(manager, validators, pair, block, price);
    }

    #[test]
    fn test_oracle_submission_valid() {
        let (mut manager, validators) = make_manager();
        let pair = PricePair::new(1, 0);
        let block = 1000u64;
        let price = 2_000_000u128;

        submit_all(&mut manager, &validators, pair, block, price);

        let q = quorum_for(&validators);
        let agg = manager.get_price(pair).unwrap();
        assert_eq!(agg.median_price, price);
        assert_eq!(agg.submission_count, q);
        assert_eq!(agg.outlier_count, 0);
    }

    #[test]
    fn test_oracle_submission_wrong_period() {
        let (mut manager, validators) = make_manager();
        let (vid, _, signing_key) = &validators[0];
        let timestamp = 500_000u64;
        let pair = PricePair::new(1, 0);

        // Block 500 is not on update interval (1000)
        let sig = sign_oracle_submission(signing_key, *vid, pair, 2_000_000, 500, timestamp);
        let submission = OracleSubmission {
            validator_id: *vid,
            pair,
            price: 2_000_000,
            block_number: 500,
            timestamp,
            signature: sig,
            sources: Vec::new(),
        };
        assert!(matches!(
            manager.submit_price(submission),
            Err(OracleError::WrongPeriod)
        ));
    }

    #[test]
    fn test_oracle_submission_duplicate() {
        let (mut manager, validators) = make_manager();
        let (vid, _, signing_key) = &validators[0];
        let block = 1000u64;
        let timestamp = block * 1000;
        let pair = PricePair::new(1, 0);

        // First submission
        let sig = sign_oracle_submission(signing_key, *vid, pair, 2_000_000, block, timestamp);
        let submission = OracleSubmission {
            validator_id: *vid,
            pair,
            price: 2_000_000,
            block_number: block,
            timestamp,
            signature: sig,
            sources: Vec::new(),
        };
        assert!(manager.submit_price(submission).is_ok());

        // Same validator, same block = duplicate
        let sig2 = sign_oracle_submission(signing_key, *vid, pair, 2_100_000, block, timestamp);
        let submission2 = OracleSubmission {
            validator_id: *vid,
            pair,
            price: 2_100_000,
            block_number: block,
            timestamp,
            signature: sig2,
            sources: Vec::new(),
        };
        assert!(matches!(
            manager.submit_price(submission2),
            Err(OracleError::DuplicateSubmission)
        ));
    }

    #[test]
    fn test_oracle_aggregation_median() {
        let (mut manager, validators) = make_manager();
        let pair = PricePair::new(1, 0);
        let block = 1000u64;
        let q = quorum_for(&validators);

        // Half submit 1_900_000, half submit 2_100_000
        let half = q / 2;
        for (i, (vid, _, signing_key)) in validators.iter().enumerate().take(q) {
            let timestamp = block * 1000;
            let price = if i < half { 1_900_000 } else { 2_100_000 };
            let sig = sign_oracle_submission(signing_key, *vid, pair, price, block, timestamp);
            let submission = OracleSubmission {
                validator_id: *vid,
                pair,
                price,
                block_number: block,
                timestamp,
                signature: sig,
                sources: Vec::new(),
            };
            manager.submit_price(submission).unwrap();
        }

        let agg = manager.get_price(pair).unwrap();
        // Sorted: [1.9M x half, 2.1M x (q-half)], median at index q/2
        assert_eq!(agg.median_price, 2_100_000);
    }

    #[test]
    fn test_oracle_outlier_detection() {
        let (mut manager, validators) = make_manager();
        let pair = PricePair::new(1, 0);
        let block = 1000u64;
        let q = quorum_for(&validators);

        // All but one submit 2_000_000, last one submits 10_000_000 (500% deviation)
        for (i, (vid, _, signing_key)) in validators.iter().enumerate().take(q) {
            let timestamp = block * 1000;
            let price = if i == q - 1 { 10_000_000 } else { 2_000_000 };
            let sig = sign_oracle_submission(signing_key, *vid, pair, price, block, timestamp);
            let submission = OracleSubmission {
                validator_id: *vid,
                pair,
                price,
                block_number: block,
                timestamp,
                signature: sig,
                sources: Vec::new(),
            };
            manager.submit_price(submission).unwrap();
        }

        let agg = manager.get_price(pair).unwrap();
        assert_eq!(agg.outlier_count, 1);

        // The outlier validator should have 1 strike
        let outlier_vid = validators[q - 1].0;
        let info = manager.get_validator_info(outlier_vid).unwrap();
        assert_eq!(info.outlier_count, 1);
        assert!(info.is_active);
    }

    #[test]
    fn test_oracle_outlier_disabled_after_10() {
        let (mut manager, validators) = make_manager();
        let pair = PricePair::new(1, 0);
        let q = quorum_for(&validators);
        let outlier_vid = validators[q - 1].0;

        // Submit 10 rounds where the last validator is always the outlier
        for round in 0..10 {
            let block = (round + 1) as u64 * 1000;
            for (i, (vid, _, signing_key)) in validators.iter().enumerate().take(q) {
                let timestamp = block * 1000;
                let price = if i == q - 1 { 10_000_000 } else { 2_000_000 };
                let sig =
                    sign_oracle_submission(signing_key, *vid, pair, price, block, timestamp);
                let submission = OracleSubmission {
                    validator_id: *vid,
                    pair,
                    price,
                    block_number: block,
                    timestamp,
                    signature: sig,
                    sources: Vec::new(),
                };
                manager.submit_price(submission).unwrap();
            }
        }

        // After 10 strikes, the outlier validator should be disabled
        let info = manager.get_validator_info(outlier_vid).unwrap();
        assert!(!info.is_active);
        assert_eq!(info.outlier_count, 10);

        // Disabled validator cannot submit
        let block = 11_000u64;
        let timestamp = block * 1000;
        let (_, _, signing_key) = &validators[q - 1];
        let sig =
            sign_oracle_submission(signing_key, outlier_vid, pair, 2_000_000, block, timestamp);
        let submission = OracleSubmission {
            validator_id: outlier_vid,
            pair,
            price: 2_000_000,
            block_number: block,
            timestamp,
            signature: sig,
            sources: Vec::new(),
        };
        assert!(matches!(
            manager.submit_price(submission),
            Err(OracleError::ValidatorDisabled)
        ));
    }

    #[test]
    fn test_oracle_price_staleness() {
        let (mut manager, validators) = make_manager();
        let pair = PricePair::new(1, 0);
        let block = 1000u64;

        submit_price_for_pair(&mut manager, &validators, pair, block, 2_000_000);

        // Not stale immediately
        let ts = block * 1000;
        assert!(!manager.is_stale(pair, ts));

        // Stale after 901 seconds
        assert!(manager.is_stale(pair, ts + 901));

        // Unknown pair is stale
        assert!(manager.is_stale(PricePair::new(999, 0), ts));
    }

    #[test]
    fn test_oracle_twap_calculation() {
        let (mut manager, validators) = make_manager();
        let pair = PricePair::new(1, 0);
        let q = quorum_for(&validators);

        // Submit prices at different timestamps within a 24h window
        // Blocks must be multiples of ORACLE_UPDATE_INTERVAL (1000)
        let rounds = [
            (1_000_000u128, 1000u64, 1_000_000u64),
            (2_000_000u128, 2000u64, 1_003_600u64),   // +1h
            (3_000_000u128, 3000u64, 1_007_200u64),    // +2h
        ];

        for (price, block, timestamp) in rounds {
            for (vid, _, signing_key) in validators.iter().take(q) {
                let sig =
                    sign_oracle_submission(signing_key, *vid, pair, price, block, timestamp);
                let submission = OracleSubmission {
                    validator_id: *vid,
                    pair,
                    price,
                    block_number: block,
                    timestamp,
                    signature: sig,
                    sources: Vec::new(),
                };
                manager.submit_price(submission).unwrap();
            }
        }

        // TWAP at ts = 1_007_200 with window 86_400 covers all 3 entries
        // Time-weighted: 1M*3600 + 2M*3600 + 3M*0 = 10_800_000_000 / 7200 = 1_500_000
        let current_ts = 1_007_200u64;
        let twap = manager.get_twap(pair, current_ts).unwrap();
        assert_eq!(twap, 1_500_000);

        // At current_ts + 1800, last entry gets duration 1800
        // 1M*3600 + 2M*3600 + 3M*1800 = 16_200_000_000 / 9000 = 1_800_000
        let twap = manager.get_twap(pair, current_ts + 1800).unwrap();
        assert_eq!(twap, 1_800_000);
    }

    #[test]
    fn test_oracle_reward_pool() {
        let mut manager = OracleManager::new(OracleConfig::default());
        assert_eq!(manager.reward_pool, 0);

        manager.add_reward(500_000);
        assert_eq!(manager.reward_pool, 500_000);

        manager.add_reward(250_000);
        assert_eq!(manager.reward_pool, 750_000);
    }

    #[test]
    fn test_oracle_contributor_tracking() {
        let (mut manager, validators) = make_manager();
        let pair = PricePair::new(1, 0);
        let block = 1000u64;

        submit_all(&mut manager, &validators, pair, block, 2_000_000);

        // After successful aggregation, contributors should be recorded
        assert!(!manager.current_contributors.is_empty());
        // Outliers should be empty (no outliers in this submission)
        assert!(manager.last_outliers.is_empty());

        manager.clear_tracking();
        assert!(manager.current_contributors.is_empty());
        assert!(manager.last_outliers.is_empty());
    }

    #[test]
    fn test_legacy_get_price_by_asset() {
        let mut manager = OracleManager::new(OracleConfig::default());
        manager.record_direct_price_by_asset(1, 2_000_000, 1000, 100);
        assert_eq!(manager.get_price_by_asset(1).map(|p| p.median_price), Some(2_000_000));
        assert!(manager.get_price_by_asset(999).is_none());
    }

    #[test]
    fn test_legacy_set_tracked_assets() {
        let mut manager = OracleManager::new(OracleConfig::default());
        manager.set_tracked_assets(vec![1, 2, 3]);
        assert_eq!(manager.tracked_pairs.len(), 3);
        assert_eq!(manager.tracked_pairs[0], PricePair::new(1, 0));
        assert_eq!(manager.tracked_pairs[1], PricePair::new(2, 0));
        assert_eq!(manager.tracked_pairs[2], PricePair::new(3, 0));
    }
}
