//! Oracle System (per spec §25)
//!
//! Validator-submitted price feeds with quorum-based aggregation,
//! outlier detection, and TWAP history.

use call_crypto::ed25519_sign;
use call_crypto::ed25519_verify;
use call_primitives::{AssetId, Ed25519PublicKey};
use ed25519_dalek::SigningKey;
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
    pub asset_id: AssetId,
    pub price: u128,
    pub block_number: u64,
    pub timestamp: u64,
    pub signature: [u8; 64],
}

/// Aggregated price after quorum
#[derive(Debug, Clone)]
pub struct AggregatedPrice {
    pub asset_id: AssetId,
    pub median_price: u128,
    pub block_number: u64,
    pub timestamp: u64,
    pub submission_count: usize,
    pub outlier_count: usize,
}

/// Per-validator oracle tracking
#[derive(Debug, Clone)]
pub struct OracleValidatorInfo {
    pub validator_id: u32,
    pub public_key: Ed25519PublicKey,
    pub is_active: bool,
    pub outlier_count: u32,
    pub last_submission_block: u64,
    pub submission_count: u64,
}

/// Historical price entry for TWAP
#[derive(Debug, Clone)]
pub struct HistoricalPrice {
    pub price: u128,
    pub timestamp: u64,
    pub block_number: u64,
}

/// Oracle configuration
#[derive(Debug, Clone, Copy)]
pub struct OracleConfig {
    pub update_interval: u64,
    pub outlier_threshold_bps: u64,
    pub outlier_tolerance: u32,
    pub twap_window_secs: u64,
    pub staleness_secs: u64,
    pub min_data_sources: usize,
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
        }
    }
}

// ─── Oracle State ───────────────────────────────────────────────

/// Full oracle state manager
#[derive(Debug)]
pub struct OracleManager {
    config: OracleConfig,
    validators: HashMap<u32, OracleValidatorInfo>,
    /// Asset IDs to track for oracle submissions
    pub tracked_assets: Vec<AssetId>,
    /// Pending submissions for current period: asset_id -> (validator_id -> submission)
    pending: HashMap<AssetId, HashMap<u32, OracleSubmission>>,
    /// Current aggregated prices per asset
    aggregated: HashMap<AssetId, AggregatedPrice>,
    /// Historical prices for TWAP: asset_id -> Vec<HistoricalPrice>
    history: HashMap<AssetId, Vec<HistoricalPrice>>,
    current_block: u64,
    /// Accumulated fee pool for oracle rewards (reset each period)
    pub reward_pool: u128,
    /// Validators who contributed to the last quorum aggregation
    pub current_contributors: Vec<u32>,
    /// Last round's outlier validators (for slashing)
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
            tracked_assets: Vec::new(),
            pending: HashMap::new(),
            aggregated: HashMap::new(),
            history: HashMap::new(),
            current_block: 0,
            reward_pool: 0,
            current_contributors: Vec::new(),
            last_outliers: Vec::new(),
        }
    }

    /// Register a validator for oracle submissions
    pub fn register_validator(&mut self, validator_id: u32, public_key: Ed25519PublicKey) {
        self.validators.insert(
            validator_id,
            OracleValidatorInfo {
                validator_id,
                public_key,
                is_active: true,
                outlier_count: 0,
                last_submission_block: 0,
                submission_count: 0,
            },
        );
    }

    /// Set the list of asset IDs to track for oracle submissions
    pub fn set_tracked_assets(&mut self, asset_ids: Vec<AssetId>) {
        self.tracked_assets = asset_ids;
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
            submission.asset_id,
            submission.price,
            submission.block_number,
            submission.timestamp,
        );
        ed25519_verify(&validator.public_key, &submission.signature, &message)
            .map_err(|_| OracleError::InvalidSignature)?;

        // Accept submission
        let asset_id = submission.asset_id;
        let validator_id = submission.validator_id;
        let block_number = submission.block_number;
        self.pending
            .entry(asset_id)
            .or_default()
            .insert(validator_id, submission);

        // Update validator info
        if let Some(v) = self.validators.get_mut(&validator_id) {
            v.last_submission_block = block_number;
            v.submission_count += 1;
        }

        // Check if quorum reached
        let submissions = self.pending.get(&asset_id).unwrap();
        if submissions.len() >= oracle_quorum(self.validators.len()) {
            self.aggregate_and_publish_price(asset_id)?;
        }

        Ok(())
    }

    /// Aggregate submissions: sort, compute median, mark outliers, append to TWAP history
    fn aggregate_and_publish_price(&mut self, asset_id: AssetId) -> Result<(), OracleError> {
        let submissions = self
            .pending
            .remove(&asset_id)
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
            asset_id,
            median_price: median,
            block_number: block,
            timestamp: first_ts,
            submission_count: submissions.len(),
            outlier_count,
        };

        // Store aggregated price
        self.aggregated.insert(asset_id, aggregated);

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
            .entry(asset_id)
            .or_default()
            .push(HistoricalPrice {
                price: median,
                timestamp: first_ts,
                block_number: block,
            });

        // Prune TWAP history beyond window
        if let Some(entries) = self.history.get_mut(&asset_id) {
            if entries.len() > 1 {
                let cutoff = first_ts.saturating_sub(self.config.twap_window_secs);
                entries.retain(|e| e.timestamp >= cutoff);
            }
        }

        Ok(())
    }

    /// Get current aggregated price for an asset
    pub fn get_price(&self, asset_id: AssetId) -> Option<&AggregatedPrice> {
        self.aggregated.get(&asset_id)
    }

    /// Calculate TWAP for an asset over a period
    pub fn get_twap(&self, asset_id: AssetId, current_timestamp: u64) -> Option<u128> {
        let entries = self.history.get(&asset_id)?;
        if entries.is_empty() {
            return None;
        }
        let cutoff = current_timestamp.saturating_sub(self.config.twap_window_secs);
        let relevant: Vec<_> = entries.iter().filter(|e| e.timestamp >= cutoff).collect();
        if relevant.is_empty() {
            return None;
        }
        let sum: u128 = relevant.iter().map(|e| e.price).sum();
        Some(sum / relevant.len() as u128)
    }

    /// Check if a price is stale
    pub fn is_stale(&self, asset_id: AssetId, current_timestamp: u64) -> bool {
        match self.aggregated.get(&asset_id) {
            Some(p) => current_timestamp.saturating_sub(p.timestamp) > self.config.staleness_secs,
            None => true,
        }
    }

    /// Get validator info
    pub fn get_validator_info(&self, validator_id: u32) -> Option<&OracleValidatorInfo> {
        self.validators.get(&validator_id)
    }

    /// Advance block number (for period tracking)
    pub fn set_current_block(&mut self, block: u64) {
        self.current_block = block;
    }

    /// Get pending submission count for an asset
    pub fn pending_count(&self, asset_id: AssetId) -> usize {
        self.pending.get(&asset_id).map(|m| m.len()).unwrap_or(0)
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

    /// Simple price submission (no signature validation — used by legacy precompile)
    pub fn simple_submit_price(
        &mut self,
        asset_id: AssetId,
        price: u128,
        timestamp: u64,
        block_number: u64,
    ) {
        self.history
            .entry(asset_id)
            .or_default()
            .push(HistoricalPrice {
                price,
                timestamp,
                block_number,
            });

        // Update current aggregated price
        self.aggregated.insert(
            asset_id,
            AggregatedPrice {
                asset_id,
                median_price: price,
                block_number,
                timestamp,
                submission_count: 1,
                outlier_count: 0,
            },
        );
    }
}

// ─── Helpers ────────────────────────────────────────────────────

/// Create a canonical message hash for oracle submissions
pub fn oracle_message_hash(
    validator_id: u32,
    asset_id: AssetId,
    price: u128,
    block_number: u64,
    timestamp: u64,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(4 + 8 + 16 + 8 + 8);
    msg.extend_from_slice(&validator_id.to_le_bytes());
    msg.extend_from_slice(&asset_id.to_le_bytes());
    msg.extend_from_slice(&price.to_le_bytes());
    msg.extend_from_slice(&block_number.to_le_bytes());
    msg.extend_from_slice(&timestamp.to_le_bytes());
    msg
}

/// Sign an oracle submission message
pub fn sign_oracle_submission(
    signing_key: &SigningKey,
    validator_id: u32,
    asset_id: AssetId,
    price: u128,
    block_number: u64,
    timestamp: u64,
) -> [u8; 64] {
    let message = oracle_message_hash(validator_id, asset_id, price, block_number, timestamp);
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
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_crypto::ed25519_generate_keypair;

    fn make_manager() -> (OracleManager, Vec<(u32, Ed25519PublicKey, SigningKey)>) {
        let mut manager = OracleManager::new(OracleConfig::default());
        let mut validators = Vec::new();

        // Register 6 validators (quorum = ceil(2/3*6) = 4)
        for i in 0..6 {
            let (pubkey, signing_key) = ed25519_generate_keypair();
            let vid = i as u32;
            manager.register_validator(vid, pubkey);
            validators.push((vid, pubkey, signing_key));
        }

        (manager, validators)
    }

    fn submit_all(
        manager: &mut OracleManager,
        validators: &[(u32, Ed25519PublicKey, SigningKey)],
        asset_id: AssetId,
        block: u64,
        price: u128,
    ) {
        for (vid, _, signing_key) in validators {
            let timestamp = block * 1000;
            let sig = sign_oracle_submission(signing_key, *vid, asset_id, price, block, timestamp);
            let _ = manager.submit_price(OracleSubmission {
                validator_id: *vid,
                asset_id,
                price,
                block_number: block,
                timestamp,
                signature: sig,
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

    fn submit_price_for_asset(
        manager: &mut OracleManager,
        validators: &[(u32, Ed25519PublicKey, SigningKey)],
        asset_id: AssetId,
        block: u64,
        price: u128,
    ) {
        submit_all(manager, validators, asset_id, block, price);
    }

    #[test]
    fn test_oracle_submission_valid() {
        let (mut manager, validators) = make_manager();
        let asset_id = 1u64;
        let block = 1000u64;
        let price = 2_000_000u128;

        submit_all(&mut manager, &validators, asset_id, block, price);

        let q = quorum_for(&validators);
        let agg = manager.get_price(asset_id).unwrap();
        assert_eq!(agg.median_price, price);
        assert_eq!(agg.submission_count, q);
        assert_eq!(agg.outlier_count, 0);
    }

    #[test]
    fn test_oracle_submission_wrong_period() {
        let (mut manager, validators) = make_manager();
        let (vid, _, signing_key) = &validators[0];
        let timestamp = 500_000u64;

        // Block 500 is not on update interval (1000)
        let sig = sign_oracle_submission(signing_key, *vid, 1, 2_000_000, 500, timestamp);
        let submission = OracleSubmission {
            validator_id: *vid,
            asset_id: 1,
            price: 2_000_000,
            block_number: 500,
            timestamp,
            signature: sig,
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

        // First submission
        let sig = sign_oracle_submission(signing_key, *vid, 1, 2_000_000, block, timestamp);
        let submission = OracleSubmission {
            validator_id: *vid,
            asset_id: 1,
            price: 2_000_000,
            block_number: block,
            timestamp,
            signature: sig,
        };
        assert!(manager.submit_price(submission).is_ok());

        // Same validator, same block = duplicate
        let sig2 = sign_oracle_submission(signing_key, *vid, 1, 2_100_000, block, timestamp);
        let submission2 = OracleSubmission {
            validator_id: *vid,
            asset_id: 1,
            price: 2_100_000,
            block_number: block,
            timestamp,
            signature: sig2,
        };
        assert!(matches!(
            manager.submit_price(submission2),
            Err(OracleError::DuplicateSubmission)
        ));
    }

    #[test]
    fn test_oracle_aggregation_median() {
        let (mut manager, validators) = make_manager();
        let asset_id = 1u64;
        let block = 1000u64;
        let q = quorum_for(&validators);

        // Half submit 1_900_000, half submit 2_100_000
        let half = q / 2;
        for (i, (vid, _, signing_key)) in validators.iter().enumerate().take(q) {
            let timestamp = block * 1000;
            let price = if i < half { 1_900_000 } else { 2_100_000 };
            let sig = sign_oracle_submission(signing_key, *vid, asset_id, price, block, timestamp);
            let submission = OracleSubmission {
                validator_id: *vid,
                asset_id,
                price,
                block_number: block,
                timestamp,
                signature: sig,
            };
            manager.submit_price(submission).unwrap();
        }

        let agg = manager.get_price(asset_id).unwrap();
        // Sorted: [1.9M x half, 2.1M x (q-half)], median at index q/2
        assert_eq!(agg.median_price, 2_100_000);
    }

    #[test]
    fn test_oracle_outlier_detection() {
        let (mut manager, validators) = make_manager();
        let asset_id = 1u64;
        let block = 1000u64;
        let q = quorum_for(&validators);

        // All but one submit 2_000_000, last one submits 10_000_000 (500% deviation)
        for (i, (vid, _, signing_key)) in validators.iter().enumerate().take(q) {
            let timestamp = block * 1000;
            let price = if i == q - 1 { 10_000_000 } else { 2_000_000 };
            let sig = sign_oracle_submission(signing_key, *vid, asset_id, price, block, timestamp);
            let submission = OracleSubmission {
                validator_id: *vid,
                asset_id,
                price,
                block_number: block,
                timestamp,
                signature: sig,
            };
            manager.submit_price(submission).unwrap();
        }

        let agg = manager.get_price(asset_id).unwrap();
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
        let asset_id = 1u64;
        let q = quorum_for(&validators);
        let outlier_vid = validators[q - 1].0;

        // Submit 10 rounds where the last validator is always the outlier
        for round in 0..10 {
            let block = (round + 1) as u64 * 1000;
            for (i, (vid, _, signing_key)) in validators.iter().enumerate().take(q) {
                let timestamp = block * 1000;
                let price = if i == q - 1 { 10_000_000 } else { 2_000_000 };
                let sig =
                    sign_oracle_submission(signing_key, *vid, asset_id, price, block, timestamp);
                let submission = OracleSubmission {
                    validator_id: *vid,
                    asset_id,
                    price,
                    block_number: block,
                    timestamp,
                    signature: sig,
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
            sign_oracle_submission(signing_key, outlier_vid, asset_id, 2_000_000, block, timestamp);
        let submission = OracleSubmission {
            validator_id: outlier_vid,
            asset_id,
            price: 2_000_000,
            block_number: block,
            timestamp,
            signature: sig,
        };
        assert!(matches!(
            manager.submit_price(submission),
            Err(OracleError::ValidatorDisabled)
        ));
    }

    #[test]
    fn test_oracle_price_staleness() {
        let (mut manager, validators) = make_manager();
        let asset_id = 1u64;
        let block = 1000u64;

        submit_price_for_asset(&mut manager, &validators, asset_id, block, 2_000_000);

        // Not stale immediately
        let ts = block * 1000;
        assert!(!manager.is_stale(asset_id, ts));

        // Stale after 901 seconds
        assert!(manager.is_stale(asset_id, ts + 901));

        // Unknown asset is stale
        assert!(manager.is_stale(999, ts));
    }

    #[test]
    fn test_oracle_twap_calculation() {
        let (mut manager, validators) = make_manager();
        let asset_id = 1u64;
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
                    sign_oracle_submission(signing_key, *vid, asset_id, price, block, timestamp);
                let submission = OracleSubmission {
                    validator_id: *vid,
                    asset_id,
                    price,
                    block_number: block,
                    timestamp,
                    signature: sig,
                };
                manager.submit_price(submission).unwrap();
            }
        }

        // TWAP at ts = 1_007_200 with window 86_400 covers all 3 entries
        let current_ts = 1_007_200u64;
        let twap = manager.get_twap(asset_id, current_ts).unwrap();
        assert_eq!(twap, 2_000_000); // (1M + 2M + 3M) / 3

        // Even at current_ts + 1800, window of 86_400 still covers all entries
        // cutoff = 1_009_000 - 86_400 = 922_600, all entries >= 1_000_000 qualify
        let twap = manager.get_twap(asset_id, current_ts + 1800).unwrap();
        assert_eq!(twap, 2_000_000);
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
        let asset_id = 1u64;
        let block = 1000u64;

        submit_all(&mut manager, &validators, asset_id, block, 2_000_000);

        // After successful aggregation, contributors should be recorded
        assert!(!manager.current_contributors.is_empty());
        // Outliers should be empty (no outliers in this submission)
        assert!(manager.last_outliers.is_empty());

        manager.clear_tracking();
        assert!(manager.current_contributors.is_empty());
        assert!(manager.last_outliers.is_empty());
    }
}
