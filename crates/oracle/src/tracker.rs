//! OracleTracker — transient price-submission buffer and aggregation.
//!
//! Replaces the persisted `OracleManager`.  All canonical price/TWAP data
//! lives in EVM storage; this struct only holds pending submissions and
//! per-period contributor/outlier tracking (both lost on restart).

use crate::{
    oracle_message_hash, oracle_quorum, AggregatedPrice, OracleConfig,
    OracleError, OracleSubmission, OracleValidatorInfo, PricePair,
};
use call_crypto::ed25519_verify;
use std::collections::HashMap;

/// Transient oracle coordinator.
#[derive(Debug, Default)]
pub struct OracleTracker {
    /// Pending submissions for current period: pair -> (validator_id -> submission)
    pub(crate) pending: HashMap<PricePair, HashMap<u32, OracleSubmission>>,
    /// Validators who contributed to the last quorum aggregation
    pub(crate) current_contributors: Vec<u32>,
    /// Last round's outlier validators (for slashing)
    pub(crate) last_outliers: Vec<u32>,
}

impl OracleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Submit a price from a validator.
    /// Returns `Ok(Some(aggregated))` if quorum was reached, `Ok(None)` if
    /// accepted but quorum not yet reached, or `Err` if rejected.
    pub fn submit_price(
        &mut self,
        submission: OracleSubmission,
        config: &OracleConfig,
        validators: &HashMap<u32, OracleValidatorInfo>,
    ) -> Result<Option<AggregatedPrice>, OracleError> {
        let validator = validators
            .get(&submission.validator_id)
            .ok_or(OracleError::ValidatorNotFound)?;

        if !validator.is_active {
            return Err(OracleError::ValidatorDisabled);
        }

        // Period check: only accept at update interval boundaries
        if !submission.block_number.is_multiple_of(config.update_interval) {
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

        // Data source attestation
        if submission.sources.len() < config.min_data_sources {
            return Err(OracleError::InsufficientDataSources(
                submission.sources.len(),
                config.min_data_sources,
            ));
        }
        if !config.allowed_sources.is_empty() {
            for source in &submission.sources {
                if !config.allowed_sources.contains(source) {
                    return Err(OracleError::DisallowedSource(source.clone()));
                }
            }
        }

        // Accept submission
        let pair = submission.pair;
        let validator_id = submission.validator_id;
        self.pending
            .entry(pair)
            .or_default()
            .insert(validator_id, submission);

        // Check if quorum reached
        let submissions = self.pending.get(&pair).unwrap();
        let active_count = validators.values().filter(|v| v.is_active).count();
        if submissions.len() >= oracle_quorum(active_count) {
            let (aggregated, outliers, contributors) =
                aggregate_submissions(self.pending.remove(&pair).unwrap(), config);
            self.last_outliers = outliers;
            self.current_contributors = contributors;
            return Ok(Some(aggregated));
        }

        Ok(None)
    }

    /// Get the last round's outlier validator IDs (for slashing)
    pub fn last_outliers(&self) -> &[u32] {
        &self.last_outliers
    }

    /// Distribute the oracle reward pool proportionally to contributors.
    /// Returns a list of (validator_id, reward_amount).
    pub fn distribute_rewards(&mut self, reward_pool: u128) -> Vec<(u32, u128)> {
        let total = reward_pool;
        if total == 0 || self.current_contributors.is_empty() {
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
                let amount = if i == 0 {
                    per_validator + remainder
                } else {
                    per_validator
                };
                (*vid, amount)
            })
            .collect();

        rewards
    }

    /// Clear contributor/outlier tracking and pending submissions
    /// (called at start of new period).
    pub fn clear_tracking(&mut self) {
        self.current_contributors.clear();
        self.last_outliers.clear();
    }

    /// Clear pending submissions (called at period boundary).
    pub fn clear_pending(&mut self) {
        self.pending.clear();
    }
}

/// Aggregate submissions: sort, compute median, detect outliers.
/// Returns `(AggregatedPrice, outlier_ids, contributor_ids)`.
pub fn aggregate_submissions(
    submissions: HashMap<u32, OracleSubmission>,
    config: &OracleConfig,
) -> (AggregatedPrice, Vec<u32>, Vec<u32>) {
    let mut prices: Vec<(u32, u128)> = submissions
        .values()
        .map(|s| (s.validator_id, s.price))
        .collect();
    prices.sort_by_key(|&(_, p)| p);

    let median = prices[prices.len() / 2].1;

    let threshold_bps = config.outlier_threshold_bps;
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

    let first_ts = submissions.values().map(|s| s.timestamp).min().unwrap_or(0);
    let block = submissions.values().map(|s| s.block_number).next().unwrap_or(0);

    let pair = submissions.values().next().map(|s| s.pair).unwrap_or(PricePair::new(0, 0));

    let aggregated = AggregatedPrice {
        pair,
        median_price: median,
        block_number: block,
        timestamp: first_ts,
        submission_count: submissions.len(),
        outlier_count,
    };

    let contributors: Vec<u32> = prices
        .iter()
        .filter_map(|&(vid, _)| {
            if !outlier_validators.contains(&vid) {
                Some(vid)
            } else {
                None
            }
        })
        .collect();

    (aggregated, outlier_validators, contributors)
}
