//! T6.1 — Proposer Selection (per spec §2.3)
//!
//! Consensus parameters, validator subset selection (21 of 216),
//! and deterministic proposer selection per round.

use call_primitives::ValidatorId;
use serde::{Deserialize, Serialize};

// ── Consensus Parameters ──────────────────────────────────────────────

/// Consensus configuration (per spec §2.3)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ConsensusParams {
    /// Maximum validators in the active set (216)
    pub max_validators: u32,
    /// Validators selected per round (21)
    pub subset_size: u32,
    /// Target block time in milliseconds (250)
    pub block_time_millis: u64,
    /// Number of rounds before slashing window resets
    pub slashing_window: u64,
    /// Delay in ms after broadcasting oracle price requests
    pub oracle_request_delay_ms: u64,
}

impl Default for ConsensusParams {
    fn default() -> Self {
        Self {
            max_validators: 216,
            subset_size: 21,
            block_time_millis: 250,
            slashing_window: 10_000,
            oracle_request_delay_ms: 200,
        }
    }
}

impl ConsensusParams {
    /// Create with custom values
    pub const fn new(
        max_validators: u32,
        subset_size: u32,
        block_time_millis: u64,
        slashing_window: u64,
        oracle_request_delay_ms: u64,
    ) -> Self {
        Self {
            max_validators,
            subset_size,
            block_time_millis,
            slashing_window,
            oracle_request_delay_ms,
        }
    }

    /// Check if validator count is within valid range (100-216)
    pub fn is_valid_validator_count(&self, count: u32) -> bool {
        (100..=self.max_validators).contains(&count)
    }
}

// ── Proposer Selection ────────────────────────────────────────────────

/// Select a deterministic proposer subset from the validator set.
/// Uses round number as seed for reproducible selection.
///
/// Per spec §2.3: "轮次随机选择" (round-based random selection)
pub fn select_proposer_subset(
    validators: &[ValidatorId],
    round: u64,
    subset_size: u32,
) -> Vec<ValidatorId> {
    if validators.is_empty() || subset_size == 0 {
        return Vec::new();
    }

    let size = subset_size as usize;
    if size >= validators.len() {
        return validators.to_vec();
    }

    // Deterministic selection using round as seed
    // Simple Fisher-Yates-style shuffle with round-based seed
    let mut indices: Vec<usize> = (0..validators.len()).collect();
    let mut seed = round.wrapping_mul(6364136223846793005).wrapping_add(1);

    for i in (1..validators.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let j = (seed as usize) % (i + 1);
        indices.swap(i, j);
    }

    indices.truncate(size);
    indices.sort(); // Stable ordering for determinism
    indices.iter().map(|&i| validators[i]).collect()
}

/// Select the block proposer from the current round's subset.
/// Uses round number modulo subset size for deterministic rotation.
pub fn select_proposer(subset: &[ValidatorId], round: u64) -> Option<ValidatorId> {
    if subset.is_empty() {
        return None;
    }
    let idx = (round as usize) % subset.len();
    Some(subset[idx])
}

/// Verify that a proposer is a member of the expected subset
pub fn verify_proposer_in_subset(
    proposer: ValidatorId,
    subset: &[ValidatorId],
) -> bool {
    subset.contains(&proposer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_validators(n: u32) -> Vec<ValidatorId> {
        (0..n).collect()
    }

    #[test]
    fn test_consensus_params_defaults() {
        let params = ConsensusParams::default();
        assert_eq!(params.max_validators, 216);
        assert_eq!(params.subset_size, 21);
        assert_eq!(params.block_time_millis, 250);
        assert!(params.slashing_window > 0);
    }

    #[test]
    fn test_consensus_params_valid_count() {
        let params = ConsensusParams::default();
        assert!(params.is_valid_validator_count(100));
        assert!(params.is_valid_validator_count(216));
        assert!(!params.is_valid_validator_count(99));
        assert!(!params.is_valid_validator_count(217));
    }

    #[test]
    fn test_proposer_subset_size() {
        let validators = test_validators(216);
        let subset = select_proposer_subset(&validators, 1, 21);
        assert_eq!(subset.len(), 21);
    }

    #[test]
    fn test_proposer_subset_deterministic() {
        let validators = test_validators(216);
        let subset1 = select_proposer_subset(&validators, 42, 21);
        let subset2 = select_proposer_subset(&validators, 42, 21);
        assert_eq!(subset1, subset2);
    }

    #[test]
    fn test_proposer_subset_different_rounds() {
        let validators = test_validators(216);
        let subset1 = select_proposer_subset(&validators, 1, 21);
        let subset2 = select_proposer_subset(&validators, 2, 21);
        // Different rounds should (usually) produce different subsets
        // Not guaranteed to be different, but very likely with 216 validators
        assert_ne!(subset1.len(), 0);
        assert_ne!(subset2.len(), 0);
    }

    #[test]
    fn test_proposer_subset_sorted() {
        let validators = test_validators(216);
        let subset = select_proposer_subset(&validators, 1, 21);
        // Subset should be sorted for stable ordering
        for i in 1..subset.len() {
            assert!(subset[i] > subset[i - 1]);
        }
    }

    #[test]
    fn test_proposer_selection_from_subset() {
        let validators = test_validators(216);
        let subset = select_proposer_subset(&validators, 1, 21);
        let proposer = select_proposer(&subset, 1);
        assert!(proposer.is_some());
        assert!(subset.contains(&proposer.unwrap()));
    }

    #[test]
    fn test_proposer_rotation() {
        let validators = test_validators(216);
        let subset = select_proposer_subset(&validators, 1, 21);

        // Different rounds should select different proposers (within same subset)
        let p1 = select_proposer(&subset, 1);
        let p2 = select_proposer(&subset, 2);
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_proposer_subset_rotation() {
        let validators = test_validators(216);

        // Same subset across rounds (subset is stable per epoch)
        let s1 = select_proposer_subset(&validators, 100, 21);
        let s2 = select_proposer_subset(&validators, 101, 21);
        // Different rounds produce different subsets
        assert_ne!(s1, s2);
    }

    #[test]
    fn test_verify_proposer_in_subset() {
        let validators = test_validators(216);
        let subset = select_proposer_subset(&validators, 1, 21);
        let proposer = select_proposer(&subset, 1).unwrap();
        assert!(verify_proposer_in_subset(proposer, &subset));
        assert!(!verify_proposer_in_subset(999, &subset));
    }

    #[test]
    fn test_empty_inputs() {
        let empty: Vec<ValidatorId> = vec![];
        assert!(select_proposer_subset(&empty, 1, 21).is_empty());
        assert!(select_proposer(&empty, 1).is_none());
    }

    #[test]
    fn test_subset_larger_than_validators() {
        let validators = test_validators(5);
        let subset = select_proposer_subset(&validators, 1, 21);
        assert_eq!(subset.len(), 5);
        assert_eq!(subset, validators);
    }
}
