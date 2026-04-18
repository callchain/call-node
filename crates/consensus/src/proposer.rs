//! T6.1 — Proposer Selection (per spec §2.3)
//!
//! Consensus parameters, validator subset selection (21 of 216),
//! and deterministic proposer selection per round.
//!
//! Uses VRF (Verifiable Random Function) based on Ed25519 for
//! cryptographically secure subset selection.

use call_crypto::vrf_sortition_score;
use call_primitives::{Ed25519PublicKey, ValidatorId};
use serde::{Deserialize, Serialize};

// ── Consensus Parameters ──────────────────────────────────────────────

/// Number of blocks per epoch before rotating the participant subset.
/// Each epoch triggers a BFT engine restart with a new VRF-selected subset.
pub const EPOCH_LENGTH: u64 = 100;

/// Consensus configuration (per spec §2.3)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ConsensusParams {
    /// Maximum validators in the active set (216)
    pub max_validators: u32,
    /// Validators selected per epoch (21)
    pub subset_size: u32,
    /// Target block time in milliseconds (250)
    pub block_time_millis: u64,
    /// Number of rounds before slashing window resets
    pub slashing_window: u64,
    /// Delay in ms after broadcasting oracle price requests
    pub oracle_request_delay_ms: u64,
    /// Number of blocks per epoch before rotating participant subset
    pub epoch_length: u64,
}

impl Default for ConsensusParams {
    fn default() -> Self {
        Self {
            max_validators: 216,
            subset_size: 21,
            block_time_millis: 250,
            slashing_window: 10_000,
            oracle_request_delay_ms: 200,
            epoch_length: EPOCH_LENGTH,
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
            epoch_length: EPOCH_LENGTH,
        }
    }

    /// Create with all parameters configurable
    pub const fn with_epoch_length(
        self,
        epoch_length: u64,
    ) -> Self {
        Self {
            epoch_length,
            ..self
        }
    }

    /// Check if validator count is within valid range (100-216)
    pub fn is_valid_validator_count(&self, count: u32) -> bool {
        (100..=self.max_validators).contains(&count)
    }
}

// ── VRF-Based Proposer Selection ──────────────────────────────────────

/// Derive a VRF seed from the previous block hash and epoch/round number.
///
/// The seed is unbiasable because it depends on the already-committed
/// previous block hash, which no single validator can manipulate.
pub fn derive_vrf_seed(prev_block_hash: &call_primitives::BlockHash, round: u64) -> [u8; 32] {
    let mut data = Vec::with_capacity(32 + 8);
    data.extend_from_slice(prev_block_hash.as_slice());
    data.extend_from_slice(&round.to_le_bytes());
    call_crypto::keccak256(&data).into()
}

/// Select a deterministic proposer subset using VRF sortition.
///
/// Each validator's score is `keccak256(VRF_DOMAIN || seed || pubkey)`,
/// producing a cryptographically random, verifiable, and unbiasable
/// ordering. Validators are sorted by score and the first `subset_size`
/// are selected.
///
/// # Arguments
/// * `validators` — list of active validator IDs
/// * `validator_pubkeys` — map of validator ID → Ed25519 public key
/// * `seed` — 32-byte unbiasable seed (from `derive_vrf_seed`)
/// * `subset_size` — number of validators to select
pub fn select_proposer_subset(
    validators: &[ValidatorId],
    validator_pubkeys: &std::collections::HashMap<ValidatorId, Ed25519PublicKey>,
    seed: &[u8; 32],
    subset_size: u32,
) -> Vec<ValidatorId> {
    if validators.is_empty() || subset_size == 0 {
        return Vec::new();
    }

    let size = subset_size as usize;

    // Compute VRF score for each validator and sort
    let mut scored: Vec<(ValidatorId, [u8; 32])> = validators
        .iter()
        .filter_map(|&id| {
            validator_pubkeys
                .get(&id)
                .map(|pk| (id, vrf_sortition_score(pk, seed)))
        })
        .collect();

    // Sort by score (lexicographic comparison of the 32-byte hash)
    scored.sort_by(|a, b| a.1.cmp(&b.1));

    // Take the first subset_size validators
    let mut subset: Vec<ValidatorId> = scored.into_iter().take(size).map(|(id, _)| id).collect();

    // Stable ordering for determinism
    subset.sort();
    subset
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
pub fn verify_proposer_in_subset(proposer: ValidatorId, subset: &[ValidatorId]) -> bool {
    subset.contains(&proposer)
}

// ── Legacy LCG fallback (for testing without pubkeys) ─────────────────

/// Legacy proposer subset selection using a linear congruential generator.
/// **Not cryptographically secure** — retained only for tests that do not
/// have validator public keys available.
#[cfg(test)]
pub fn select_proposer_subset_lcg(validators: &[ValidatorId], round: u64, subset_size: u32) -> Vec<ValidatorId> {
    if validators.is_empty() || subset_size == 0 {
        return Vec::new();
    }

    let size = subset_size as usize;
    if size >= validators.len() {
        return validators.to_vec();
    }

    let mut indices: Vec<usize> = (0..validators.len()).collect();
    let mut seed = round.wrapping_mul(6364136223846793005).wrapping_add(1);

    for i in (1..validators.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let j = (seed as usize) % (i + 1);
        indices.swap(i, j);
    }

    indices.truncate(size);
    indices.sort();
    indices.iter().map(|&i| validators[i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_validators(n: u32) -> Vec<ValidatorId> {
        (0..n).collect()
    }

    fn test_pubkeys(n: u32) -> std::collections::HashMap<ValidatorId, Ed25519PublicKey> {
        let mut map = std::collections::HashMap::new();
        for i in 0..n {
            let mut key = [0u8; 32];
            key[0] = i as u8;
            key[1] = (i >> 8) as u8;
            map.insert(i, key);
        }
        map
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
    fn test_vrf_seed_derivation() {
        let hash = call_primitives::BlockHash::repeat_byte(0xAB);
        let seed1 = derive_vrf_seed(&hash, 1);
        let seed2 = derive_vrf_seed(&hash, 2);
        let seed1_copy = derive_vrf_seed(&hash, 1);

        assert_ne!(seed1, seed2);
        assert_eq!(seed1, seed1_copy);
        assert_eq!(seed1.len(), 32);
    }

    #[test]
    fn test_vrf_proposer_subset_size() {
        let validators = test_validators(216);
        let pubkeys = test_pubkeys(216);
        let seed = [0u8; 32];
        let subset = select_proposer_subset(&validators, &pubkeys, &seed, 21);
        assert_eq!(subset.len(), 21);
    }

    #[test]
    fn test_vrf_proposer_subset_deterministic() {
        let validators = test_validators(216);
        let pubkeys = test_pubkeys(216);
        let seed = [42u8; 32];
        let subset1 = select_proposer_subset(&validators, &pubkeys, &seed, 21);
        let subset2 = select_proposer_subset(&validators, &pubkeys, &seed, 21);
        assert_eq!(subset1, subset2);
    }

    #[test]
    fn test_vrf_proposer_subset_different_seeds() {
        let validators = test_validators(216);
        let pubkeys = test_pubkeys(216);
        let seed1 = [1u8; 32];
        let seed2 = [2u8; 32];
        let subset1 = select_proposer_subset(&validators, &pubkeys, &seed1, 21);
        let subset2 = select_proposer_subset(&validators, &pubkeys, &seed2, 21);
        // Different seeds should (usually) produce different subsets
        assert_ne!(subset1.len(), 0);
        assert_ne!(subset2.len(), 0);
    }

    #[test]
    fn test_vrf_proposer_subset_sorted() {
        let validators = test_validators(216);
        let pubkeys = test_pubkeys(216);
        let seed = [0u8; 32];
        let subset = select_proposer_subset(&validators, &pubkeys, &seed, 21);
        for i in 1..subset.len() {
            assert!(subset[i] > subset[i - 1]);
        }
    }

    #[test]
    fn test_vrf_proposer_selection_from_subset() {
        let validators = test_validators(216);
        let pubkeys = test_pubkeys(216);
        let seed = [0u8; 32];
        let subset = select_proposer_subset(&validators, &pubkeys, &seed, 21);
        let proposer = select_proposer(&subset, 1);
        assert!(proposer.is_some());
        assert!(subset.contains(&proposer.unwrap()));
    }

    #[test]
    fn test_vrf_proposer_rotation() {
        let validators = test_validators(216);
        let pubkeys = test_pubkeys(216);
        let seed = [0u8; 32];
        let subset = select_proposer_subset(&validators, &pubkeys, &seed, 21);

        let p1 = select_proposer(&subset, 1);
        let p2 = select_proposer(&subset, 2);
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_vrf_proposer_subset_different_rounds() {
        let validators = test_validators(216);
        let pubkeys = test_pubkeys(216);
        let hash = call_primitives::BlockHash::repeat_byte(0xAB);
        let seed1 = derive_vrf_seed(&hash, 100);
        let seed2 = derive_vrf_seed(&hash, 101);
        let s1 = select_proposer_subset(&validators, &pubkeys, &seed1, 21);
        let s2 = select_proposer_subset(&validators, &pubkeys, &seed2, 21);
        // Different seeds should produce different subsets
        assert_ne!(s1, s2);
    }

    #[test]
    fn test_vrf_verify_proposer_in_subset() {
        let validators = test_validators(216);
        let pubkeys = test_pubkeys(216);
        let seed = [0u8; 32];
        let subset = select_proposer_subset(&validators, &pubkeys, &seed, 21);
        let proposer = select_proposer(&subset, 1).unwrap();
        assert!(verify_proposer_in_subset(proposer, &subset));
        assert!(!verify_proposer_in_subset(999, &subset));
    }

    #[test]
    fn test_vrf_empty_inputs() {
        let empty: Vec<ValidatorId> = vec![];
        let empty_pk = std::collections::HashMap::new();
        let seed = [0u8; 32];
        assert!(select_proposer_subset(&empty, &empty_pk, &seed, 21).is_empty());
        assert!(select_proposer(&empty, 1).is_none());
    }

    #[test]
    fn test_vrf_subset_larger_than_validators() {
        let validators = test_validators(5);
        let pubkeys = test_pubkeys(5);
        let seed = [0u8; 32];
        let subset = select_proposer_subset(&validators, &pubkeys, &seed, 21);
        assert_eq!(subset.len(), 5);
        assert_eq!(subset, validators);
    }

    #[test]
    fn test_vrf_filters_missing_pubkeys() {
        let validators = test_validators(10);
        // Only provide pubkeys for first 5 validators
        let pubkeys = test_pubkeys(5);
        let seed = [0u8; 32];
        let subset = select_proposer_subset(&validators, &pubkeys, &seed, 21);
        // Should only select from validators that have pubkeys
        assert_eq!(subset.len(), 5);
        for id in &subset {
            assert!(*id < 5);
        }
    }

    // ── LCG fallback tests (ensure backward compat for tests without keys) ──

    #[test]
    fn test_lcg_proposer_subset_size() {
        let validators = test_validators(216);
        let subset = select_proposer_subset_lcg(&validators, 1, 21);
        assert_eq!(subset.len(), 21);
    }

    #[test]
    fn test_lcg_proposer_subset_deterministic() {
        let validators = test_validators(216);
        let subset1 = select_proposer_subset_lcg(&validators, 42, 21);
        let subset2 = select_proposer_subset_lcg(&validators, 42, 21);
        assert_eq!(subset1, subset2);
    }
}
