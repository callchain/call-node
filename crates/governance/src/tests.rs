use call_asset::AssetStorage;
use call_precompile::{
    slot_balance,
    storage::{HashMapStorageProvider, StorageProvider},
    u128_to_u256, StorageRef,
};
use call_primitives::Address;

use crate::precompile::{GovernanceStorage, PROPOSAL_DEPOSIT};

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn with_storage<R>(block_number: u64, f: impl FnOnce(&mut dyn StorageProvider) -> R) -> R {
    let provider = HashMapStorageProvider::with_block(10_000_000, 1, block_number);
    let mut provider = provider;
    f(&mut provider)
}

fn seed_balance(storage: &mut dyn StorageProvider, addr: Address, amount: u128) {
    let _ = storage.sstore(
        call_precompile::ASSET_ADDRESS,
        slot_balance(call_protocol::CALL_ASSET_ID, addr),
        u128_to_u256(amount),
    );
}

// ── Submit ────────────────────────────────────────────────────────

#[test]
fn test_submit_proposal_success() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        // review_period=0 so proposal is active immediately
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0, // ParameterChange
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        assert_eq!(id, 1);
        assert_eq!(gov.read_proposal_count(), 1);
        assert_eq!(gov.read_proposal_status(id), 1); // Active
        assert_eq!(gov.read_proposal_u8(id, b"proposal_type"), 0);
        assert_eq!(gov.read_proposal_proposer(id), proposer);

        // Deposit deducted
        let balance = asset.read_balance(call_protocol::CALL_ASSET_ID, proposer);
        assert_eq!(balance, PROPOSAL_DEPOSIT * 2 - PROPOSAL_DEPOSIT);
    });
}

#[test]
fn test_submit_proposal_insufficient_balance() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT - 1);

        let result = gov.submit_proposal(
            &mut asset,
            0,
            "title".into(),
            "desc".into(),
            vec![],
            proposer,
            0,
        );
        assert!(result.is_err());
    });
}

#[test]
fn test_rate_limiting() {
    with_storage(100, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 10);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"proposal_cooldown", 50);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                100,
            )
            .unwrap();
        assert_eq!(id, 1);

        // Same block (100): rate limited
        let result = gov.submit_proposal(
            &mut asset,
            0,
            "title".into(),
            "desc".into(),
            vec![],
            proposer,
            100,
        );
        assert!(result.is_err());
    });
}

#[test]
fn test_rate_limiting_expires_after_cooldown() {
    let mut provider = HashMapStorageProvider::with_block(10_000_000, 1, 0);
    {
        let storage = &mut provider;
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 10);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"proposal_cooldown", 50);

        gov.submit_proposal(
            &mut asset,
            0,
            "title".into(),
            "desc".into(),
            vec![],
            proposer,
            0,
        )
        .unwrap();
    }

    // Advance past cooldown (0 + 50 = 50, so 51 is past)
    provider.set_block_number(51);
    {
        let storage = &mut provider;
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 10);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"proposal_cooldown", 50);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                51,
            )
            .unwrap();
        assert_eq!(id, 2);
    }
}

// ── Vote ──────────────────────────────────────────────────────────

#[test]
fn test_vote_yes_and_tally() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);
        let voter = test_addr(2);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        seed_balance(storage, voter, 500_000);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 1, voter, 500_000, 0).unwrap();

        assert_eq!(gov.read_vote_tally(id, b"votes_for"), 500_000);
        assert_eq!(gov.read_vote_tally(id, b"votes_against"), 0);
        assert_eq!(gov.read_vote_tally(id, b"votes_abstain"), 0);
    });
}

#[test]
fn test_vote_before_start_rejected() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);
        let voter = test_addr(2);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        seed_balance(storage, voter, 500_000);
        // review_period=10, so start_block=10
        gov.write_config_u64(b"review_period", 10);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        // Block 0 < start_block 10: voting not started
        let result = gov.vote(id, 1, voter, 500_000, 0);
        assert!(result.is_err());
    });
}

#[test]
fn test_vote_after_end_rejected() {
    let mut provider = HashMapStorageProvider::with_block(10_000_000, 1, 0);
    let id = {
        let storage = &mut provider;
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();
        id
    };

    // Advance past voting end (start=0, end=100, so vote at 101 should fail)
    provider.set_block_number(101);
    {
        let storage = &mut provider;
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let voter = test_addr(2);
        seed_balance(storage, voter, 500_000);

        let result = gov.vote(id, 1, voter, 500_000, 101);
        assert!(result.is_err());
    }
}

#[test]
fn test_duplicate_vote_rejected() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);
        let voter = test_addr(2);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        seed_balance(storage, voter, 500_000);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 1, voter, 500_000, 0).unwrap();
        let result = gov.vote(id, 1, voter, 500_000, 0);
        assert!(result.is_err());

        assert_eq!(gov.read_vote_tally(id, b"votes_for"), 500_000);
    });
}

#[test]
fn test_vote_no_and_abstain() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);
        let voter_a = test_addr(2);
        let voter_b = test_addr(3);
        let voter_c = test_addr(4);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        seed_balance(storage, voter_a, 100_000);
        seed_balance(storage, voter_b, 200_000);
        seed_balance(storage, voter_c, 300_000);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 1, voter_a, 100_000, 0).unwrap(); // Yes
        gov.vote(id, 2, voter_b, 200_000, 0).unwrap(); // No
        gov.vote(id, 3, voter_c, 300_000, 0).unwrap(); // Abstain

        assert_eq!(gov.read_vote_tally(id, b"votes_for"), 100_000);
        assert_eq!(gov.read_vote_tally(id, b"votes_against"), 200_000);
        assert_eq!(gov.read_vote_tally(id, b"votes_abstain"), 300_000);
    });
}

// ── Queue ─────────────────────────────────────────────────────────

#[test]
fn test_queue_with_quorum() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        // Cast votes exceeding quorum
        gov.vote(id, 1, test_addr(2), 1_000_000, 0).unwrap();

        gov.queue(id, 101).unwrap();

        assert_eq!(gov.read_proposal_status(id), 2); // Queued
        let exec_block = gov.read_proposal_u64(id, b"execution_block");
        assert!(exec_block > 101); // timelock applied
    });
}

#[test]
fn test_queue_without_quorum_defeated() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        // No votes cast
        let result = gov.queue(id, 101);
        assert!(result.is_err());

        assert_eq!(gov.read_proposal_status(id), 4); // Defeated
                                                     // Deposit confiscated
        assert_eq!(gov.read_proposal_u128(id, b"deposit"), 0);
    });
}

#[test]
fn test_queue_more_against_than_for_defeated() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 2, test_addr(2), 500_000, 0).unwrap(); // No
        gov.vote(id, 1, test_addr(3), 100_000, 0).unwrap(); // Yes

        let result = gov.queue(id, 101);
        assert!(result.is_err());
        assert_eq!(gov.read_proposal_status(id), 4); // Defeated
    });
}

#[test]
fn test_queue_emergency_pause_skips_timelock() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                5, // EmergencyPause
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 1, test_addr(2), 1_000_000, 0).unwrap();

        let current_block = 101;
        gov.queue(id, current_block).unwrap();

        assert_eq!(gov.read_proposal_status(id), 2); // Queued
        let exec_block = gov.read_proposal_u64(id, b"execution_block");
        assert_eq!(exec_block, current_block); // No timelock
    });
}

// ── Execute ───────────────────────────────────────────────────────

#[test]
fn test_execute_after_timelock() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"timelock", 50);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 1, test_addr(2), 1_000_000, 0).unwrap();
        gov.queue(id, 101).unwrap();

        let exec_block = gov.read_proposal_u64(id, b"execution_block");

        // Before timelock: rejected
        let result = gov.execute(&mut asset, id, exec_block - 1, proposer);
        assert!(result.is_err());

        // After timelock: succeeds
        gov.execute(&mut asset, id, exec_block, proposer).unwrap();

        assert_eq!(gov.read_proposal_status(id), 3); // Executed

        // Deposit refunded
        let balance = asset.read_balance(call_protocol::CALL_ASSET_ID, proposer);
        assert_eq!(
            balance,
            PROPOSAL_DEPOSIT * 2 - PROPOSAL_DEPOSIT + PROPOSAL_DEPOSIT
        );
    });
}

#[test]
fn test_execute_deposit_refunded() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"timelock", 1);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 1, test_addr(2), 1_000_000, 0).unwrap();
        gov.queue(id, 1).unwrap();
        // execution_block = 1 + timelock(1) = 2
        gov.execute(&mut asset, id, 2, proposer).unwrap();

        // Deposit returned
        let balance = asset.read_balance(call_protocol::CALL_ASSET_ID, proposer);
        assert_eq!(balance, PROPOSAL_DEPOSIT * 2);
    });
}

#[test]
fn test_execute_emergency_pause_sets_paused() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                5, // EmergencyPause
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 1, test_addr(2), 1_000_000, 0).unwrap();
        gov.queue(id, 1).unwrap();

        assert!(!gov.is_paused());
        gov.execute(&mut asset, id, 1, proposer).unwrap();
        assert!(gov.is_paused());
    });
}

// ── Emergency pause / resume ──────────────────────────────────────

#[test]
fn test_emergency_pause_and_resume() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));

        assert!(!gov.is_paused());

        gov.emergency_pause([1u8; 32], test_addr(1));
        assert!(gov.is_paused());

        gov.emergency_resume();
        assert!(!gov.is_paused());
    });
}

// ── Proposal types ────────────────────────────────────────────────

#[test]
fn test_proposal_types_stored_correctly() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 20);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        for proposal_type in 0u8..=9 {
            let id = gov
                .submit_proposal(
                    &mut asset,
                    proposal_type,
                    "title".into(),
                    "desc".into(),
                    vec![],
                    proposer,
                    0,
                )
                .unwrap();
            let read = gov.read_proposal_u8(id, b"proposal_type");
            assert_eq!(read, proposal_type);
        }
    });
}

// ── Config / periods ──────────────────────────────────────────────

#[test]
fn test_custom_review_and_voting_periods() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 25);
        gov.write_config_u64(b"voting_period", 75);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        assert_eq!(gov.read_proposal_status(id), 0); // Pending
        let start_block = gov.read_proposal_u64(id, b"start_block");
        let end_block = gov.read_proposal_u64(id, b"end_block");
        assert_eq!(start_block, 25);
        assert_eq!(end_block, 100);
    });
}

// ── Deposit confiscation ──────────────────────────────────────────

#[test]
fn test_deposit_confiscated_on_defeat() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        assert_eq!(gov.read_proposal_u128(id, b"deposit"), PROPOSAL_DEPOSIT);

        // Queue without quorum -> defeat
        let _ = gov.queue(id, 101);
        assert_eq!(gov.read_proposal_status(id), 4); // Defeated
        assert_eq!(gov.read_proposal_u128(id, b"deposit"), 0); // Confiscated
    });
}

// ── Full lifecycle ────────────────────────────────────────────────

#[test]
fn test_full_lifecycle_submit_vote_queue_execute() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"timelock", 10);

        // Submit
        let id = gov
            .submit_proposal(
                &mut asset,
                0,
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();
        assert_eq!(gov.read_proposal_status(id), 1); // Active

        // Vote
        gov.vote(id, 1, test_addr(2), 1_000_000, 0).unwrap();
        assert_eq!(gov.read_vote_tally(id, b"votes_for"), 1_000_000);

        // Queue
        gov.queue(id, 1).unwrap();
        assert_eq!(gov.read_proposal_status(id), 2); // Queued

        // Execute
        let exec_block = gov.read_proposal_u64(id, b"execution_block");
        gov.execute(&mut asset, id, exec_block, proposer).unwrap();
        assert_eq!(gov.read_proposal_status(id), 3); // Executed
    });
}

// ── Config quorum calculations ────────────────────────────────────

#[test]
fn test_config_quorum_calculations() {
    let config = crate::config::GovernanceConfig::default();

    assert_eq!(config.validator_quorum(3), 3); // ceil(3 * 6667 / 10000) = 3
    assert_eq!(config.validator_quorum(10), 7); // ceil(10 * 6667 / 10000) = 7

    assert_eq!(config.supply_quorum(), crate::config::TOTAL_SUPPLY / 5); // 20%
    assert_eq!(config.treasury_quorum(), crate::config::TOTAL_SUPPLY / 5); // 20%

    assert_eq!(config.simple_majority(3), 2); // ceil(3 * 5001 / 10000) = 2
    assert_eq!(config.emergency_pause_threshold(3), 3); // ceil(3 * 6667 / 10000) = 3
}
