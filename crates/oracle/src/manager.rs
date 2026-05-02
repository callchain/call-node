//! OracleManager — full oracle state manager

use crate::{
    oracle_message_hash, oracle_quorum, AggregatedPrice, OracleConfig,
    OracleError, OracleSubmission, OracleValidatorInfo,
};
use alloy_primitives::U256;
use call_crypto::ed25519_verify;
use call_primitives::{Address, AssetId, PricePair};
use std::collections::HashMap;

/// Full oracle state manager (transient protocol-layer logic only).
/// Aggregated prices, TWAP, and timestamps live in EVM storage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    current_block: u64,
    /// Accumulated fee pool for oracle rewards (reset each period).
    /// The canonical reward pool lives in EVM storage; this in-memory
    /// field is used by the protocol-layer distributor.
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
    pub fn register_validator(&mut self, validator_id: u32, address: Address, public_key: call_primitives::Ed25519PublicKey) {
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
    /// Returns `Ok(Some(aggregated))` if quorum was reached and price aggregated,
    /// `Ok(None)` if accepted but quorum not yet reached, or `Err` if rejected.
    /// The caller should write the returned `AggregatedPrice` to EVM storage.
    pub fn submit_price(&mut self, submission: OracleSubmission) -> Result<Option<AggregatedPrice>, OracleError> {
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
            let aggregated = self.aggregate_and_publish_price(pair)?;
            return Ok(Some(aggregated));
        }

        Ok(None)
    }

    /// Aggregate submissions: sort, compute median, mark outliers.
    /// Returns the aggregated price info; caller must write to EVM storage.
    fn aggregate_and_publish_price(&mut self, pair: PricePair) -> Result<AggregatedPrice, OracleError> {
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

        Ok(aggregated)
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
    /// Clears pending submissions; aggregated prices already live in EVM storage.
    pub fn advance_period(&mut self, _block: u64) {
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
}
