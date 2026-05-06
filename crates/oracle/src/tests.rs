//! Oracle tests

use crate::{
    oracle_quorum, sign_oracle_submission, OracleConfig, OracleError, OracleSubmission,
    OracleTracker, OracleValidatorInfo, PricePair,
};
use call_crypto::ed25519_generate_keypair;
use call_primitives::{Address, Ed25519PublicKey};
use ed25519_dalek::SigningKey;
use std::collections::HashMap;

fn make_tracker() -> (
    OracleTracker,
    Vec<(u32, Ed25519PublicKey, SigningKey)>,
    OracleConfig,
    HashMap<u32, OracleValidatorInfo>,
) {
    let mut config = OracleConfig::default();
    config.min_data_sources = 0; // tests don't need real data sources
    let tracker = OracleTracker::default();
    let mut validators = Vec::new();
    let mut validator_map = HashMap::new();

    // Register 6 validators (quorum = ceil(2/3*6) = 4)
    for i in 0..6 {
        let (pubkey, signing_key) = ed25519_generate_keypair();
        let vid = i as u32;
        let address = Address::repeat_byte(i as u8);
        validator_map.insert(
            vid,
            OracleValidatorInfo {
                validator_id: vid,
                address,
                public_key: pubkey,
                is_active: true,
                outlier_count: 0,
                last_submission_block: 0,
                submission_count: 0,
            },
        );
        validators.push((vid, pubkey, signing_key));
    }

    (tracker, validators, config, validator_map)
}

fn submit_all(
    tracker: &mut OracleTracker,
    validators: &[(u32, Ed25519PublicKey, SigningKey)],
    validator_map: &HashMap<u32, OracleValidatorInfo>,
    config: &OracleConfig,
    pair: PricePair,
    block: u64,
    price: u128,
) -> Option<crate::AggregatedPrice> {
    let mut result = None;
    for (vid, _, signing_key) in validators {
        let timestamp = block * 1000;
        let sig = sign_oracle_submission(signing_key, *vid, pair, price, block, timestamp);
        if let Ok(Some(agg)) = tracker.submit_price(
            OracleSubmission {
                validator_id: *vid,
                pair,
                price,
                block_number: block,
                timestamp,
                signature: sig,
                sources: Vec::new(),
            },
            config,
            validator_map,
        ) {
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
    let (mut tracker, validators, config, validator_map) = make_tracker();
    let pair = PricePair::new(1, 0);
    let block = 1000u64;
    let price = 2_000_000u128;

    let agg = submit_all(
        &mut tracker,
        &validators,
        &validator_map,
        &config,
        pair,
        block,
        price,
    )
    .unwrap();

    let q = quorum_for(&validators);
    assert_eq!(agg.median_price, price);
    assert_eq!(agg.submission_count, q);
    assert_eq!(agg.outlier_count, 0);
}

#[test]
fn test_oracle_submission_wrong_period() {
    let (mut tracker, validators, config, validator_map) = make_tracker();
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
        tracker.submit_price(submission, &config, &validator_map),
        Err(OracleError::WrongPeriod)
    ));
}

#[test]
fn test_oracle_submission_duplicate() {
    let (mut tracker, validators, config, validator_map) = make_tracker();
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
    assert!(tracker
        .submit_price(submission, &config, &validator_map)
        .is_ok());

    // Same validator, same block = duplicate (overwrites, not error)
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
    // Tracker allows overwriting the same validator's submission
    assert!(tracker
        .submit_price(submission2, &config, &validator_map)
        .is_ok());
}

#[test]
fn test_oracle_aggregation_median() {
    let (mut tracker, validators, config, validator_map) = make_tracker();
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
        if let Ok(Some(a)) = tracker.submit_price(submission, &config, &validator_map) {
            agg = Some(a);
        }
    }

    let agg = agg.unwrap();
    // Sorted: [1.9M x half, 2.1M x (q-half)], median at index q/2
    assert_eq!(agg.median_price, 2_100_000);
}

#[test]
fn test_oracle_outlier_detection() {
    let (mut tracker, validators, config, validator_map) = make_tracker();
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
        if let Ok(Some(a)) = tracker.submit_price(submission, &config, &validator_map) {
            agg = Some(a);
        }
    }

    let agg = agg.unwrap();
    assert_eq!(agg.outlier_count, 1);

    // The outlier validator should be in last_outliers
    let outlier_vid = validators[q - 1].0;
    assert!(tracker.last_outliers().contains(&outlier_vid));
}

#[test]
fn test_oracle_disabled_validator_rejected() {
    let (mut tracker, validators, config, mut validator_map) = make_tracker();
    let pair = PricePair::new(1, 0);
    let block = 1000u64;
    let disabled_vid = validators[0].0;

    // Disable first validator
    if let Some(v) = validator_map.get_mut(&disabled_vid) {
        v.is_active = false;
    }

    let (_, _, signing_key) = &validators[0];
    let timestamp = block * 1000;
    let sig = sign_oracle_submission(signing_key, disabled_vid, pair, 2_000_000, block, timestamp);
    let submission = OracleSubmission {
        validator_id: disabled_vid,
        pair,
        price: 2_000_000,
        block_number: block,
        timestamp,
        signature: sig,
        sources: Vec::new(),
    };
    assert!(matches!(
        tracker.submit_price(submission, &config, &validator_map),
        Err(OracleError::ValidatorDisabled)
    ));
}

#[test]
fn test_oracle_distribute_rewards() {
    let (mut tracker, validators, config, validator_map) = make_tracker();
    let pair = PricePair::new(1, 0);
    let block = 1000u64;

    submit_all(
        &mut tracker,
        &validators,
        &validator_map,
        &config,
        pair,
        block,
        2_000_000,
    );

    // After successful aggregation, contributors should be recorded
    assert!(!tracker.current_contributors.is_empty());
    // Outliers should be empty (no outliers in this submission)
    assert!(tracker.last_outliers().is_empty());

    let rewards = tracker.distribute_rewards(1_000_000);
    assert!(!rewards.is_empty());
    let total_distributed: u128 = rewards.iter().map(|(_, amount)| amount).sum();
    assert_eq!(total_distributed, 1_000_000);

    tracker.clear_tracking();
    assert!(tracker.current_contributors.is_empty());
    assert!(tracker.last_outliers().is_empty());
}

#[test]
fn test_oracle_clear_pending() {
    let (mut tracker, validators, config, validator_map) = make_tracker();
    let pair = PricePair::new(1, 0);
    let block = 1000u64;

    // Submit but don't reach quorum
    let (vid, _, signing_key) = &validators[0];
    let timestamp = block * 1000;
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
    tracker
        .submit_price(submission, &config, &validator_map)
        .unwrap();

    tracker.clear_pending();
    // After clearing, a new submission should work (pending is empty)
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
    assert!(tracker
        .submit_price(submission2, &config, &validator_map)
        .is_ok());
}
