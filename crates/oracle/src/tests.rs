//! Oracle tests

use crate::{
    oracle_quorum, sign_oracle_submission, OracleConfig, OracleError,
    OracleManager, OracleSubmission, PricePair,
};
use call_crypto::ed25519_generate_keypair;
use call_primitives::{Address, Ed25519PublicKey};
use ed25519_dalek::SigningKey;

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
) -> Option<crate::AggregatedPrice> {
    let mut result = None;
    for (vid, _, signing_key) in validators {
        let timestamp = block * 1000;
        let sig = sign_oracle_submission(signing_key, *vid, pair, price, block, timestamp);
        if let Ok(Some(agg)) = manager.submit_price(OracleSubmission {
            validator_id: *vid,
            pair,
            price,
            block_number: block,
            timestamp,
            signature: sig,
            sources: Vec::new(),
        }) {
            result = Some(agg);
        }
    }
    result
}

fn quorum_for(validators: &[(u32, Ed25519PublicKey, SigningKey)]) -> usize {
    oracle_quorum(validators.len())
}

#[test]
fn test_oracle_quorum_function() {
    assert_eq!(oracle_quorum(1), 1); // single validator
    assert_eq!(oracle_quorum(2), 2); // 2 validators, both required
    assert_eq!(oracle_quorum(3), 2); // ceil(2/3*3) = 2
    assert_eq!(oracle_quorum(4), 3); // ceil(2/3*4) = 3
    assert_eq!(oracle_quorum(6), 4); // ceil(2/3*6) = 4
    assert_eq!(oracle_quorum(21), 14); // production subset
    assert_eq!(oracle_quorum(216), 144);
}

#[test]
fn test_oracle_submission_valid() {
    let (mut manager, validators) = make_manager();
    let pair = PricePair::new(1, 0);
    let block = 1000u64;
    let price = 2_000_000u128;

    let agg = submit_all(&mut manager, &validators, pair, block, price).unwrap();

    let q = quorum_for(&validators);
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
    let mut agg = None;
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
        if let Ok(Some(a)) = manager.submit_price(submission) {
            agg = Some(a);
        }
    }

    let agg = agg.unwrap();
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
    let mut agg = None;
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
        if let Ok(Some(a)) = manager.submit_price(submission) {
            agg = Some(a);
        }
    }

    let agg = agg.unwrap();
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
            let _ = manager.submit_price(submission);
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
    assert!(manager.last_outliers().is_empty());

    manager.clear_tracking();
    assert!(manager.current_contributors.is_empty());
    assert!(manager.last_outliers().is_empty());
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
