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
fn test_execute_timeout_expires() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"timelock", 10);
        gov.write_config_u64(b"execution_timeout", 50);

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
        // Before execution block: still rejected by timelock
        let result = gov.execute(&mut asset, id, exec_block - 1, proposer);
        assert!(result.is_err());

        // After execution block but within timeout: succeeds
        gov.execute(&mut asset, id, exec_block, proposer).unwrap();
        assert_eq!(gov.read_proposal_status(id), 3); // Executed
    });
}

#[test]
fn test_execute_past_timeout_marked_expired() {
    let mut provider = HashMapStorageProvider::with_block(10_000_000, 1, 0);
    let id = {
        let storage = &mut provider;
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"timelock", 10);
        gov.write_config_u64(b"execution_timeout", 50);

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
        id
    };

    // Advance past execution block + timeout
    provider.set_block_number(200);
    {
        let storage = &mut provider;
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        let result = gov.execute(&mut asset, id, 200, proposer);
        assert!(result.is_err(), "execute past timeout should fail");
        assert_eq!(gov.read_proposal_status(id), 6); // Expired
    }
}

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

// ── TreasurySpend ─────────────────────────────────────────────────

#[test]
fn test_treasury_spend_transfers_from_treasury() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);
        let recipient = test_addr(2);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        // Seed treasury with funds
        seed_balance(storage, crate::precompile::TREASURY_ADDRESS, 500_000);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"timelock", 1);

        // execution_data = ABI-encoded (address recipient, uint128 amount)
        let mut execution_data = vec![0u8; 64];
        execution_data[12..32].copy_from_slice(recipient.as_slice());
        let amount = 100_000u128;
        execution_data[48..64].copy_from_slice(&amount.to_be_bytes());

        let id = gov
            .submit_proposal(
                &mut asset,
                2, // TreasurySpend
                "title".into(),
                "desc".into(),
                execution_data,
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 1, test_addr(3), 1_000_000, 0).unwrap();
        gov.queue(id, 1).unwrap();
        gov.execute(&mut asset, id, 2, proposer).unwrap();

        // Recipient should receive exactly the spend amount (not more)
        let recipient_balance = asset.read_balance(call_protocol::CALL_ASSET_ID, recipient);
        assert_eq!(recipient_balance, amount);

        // Treasury should be debited
        let treasury_balance = asset.read_balance(
            call_protocol::CALL_ASSET_ID,
            crate::precompile::TREASURY_ADDRESS,
        );
        assert_eq!(treasury_balance, 400_000);
    });
}

#[test]
fn test_treasury_spend_rejects_insufficient_treasury() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);
        let recipient = test_addr(2);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        // Treasury has no funds
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"timelock", 1);

        let mut execution_data = vec![0u8; 64];
        execution_data[12..32].copy_from_slice(recipient.as_slice());
        let amount = 100_000u128;
        execution_data[48..64].copy_from_slice(&amount.to_be_bytes());

        let id = gov
            .submit_proposal(
                &mut asset,
                2, // TreasurySpend
                "title".into(),
                "desc".into(),
                execution_data,
                proposer,
                0,
            )
            .unwrap();

        gov.vote(id, 1, test_addr(3), 1_000_000, 0).unwrap();
        gov.queue(id, 1).unwrap();

        let result = gov.execute(&mut asset, id, 2, proposer);
        assert!(result.is_err(), "treasury spend with no funds should fail");
    });
}

// ── Emergency pause / resume ──────────────────────────────────────

#[test]
fn test_emergency_pause_and_resume() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));

        assert!(!gov.is_paused());

        gov.emergency_pause([1u8; 32], test_addr(1)).unwrap();
        assert!(gov.is_paused());

        gov.emergency_resume().unwrap();
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

// ── Quorum computation ────────────────────────────────────────────

#[test]
fn test_quorum_computed_with_validators() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        // Seed 3 validators
        storage
            .sstore(
                call_precompile::VALIDATOR_ADDRESS,
                call_validator::slot_validator_count(),
                u128_to_u256(3),
            )
            .unwrap();

        let id = gov
            .submit_proposal(
                &mut asset,
                0, // ParameterChange (validator-based)
                "title".into(),
                "desc".into(),
                vec![],
                proposer,
                0,
            )
            .unwrap();

        // Vote with enough power (quorum = ceil(3 * 6667 / 10000) = 3 validators)
        // Since voting_power is counted as max(1 if validator, balance),
        // and we have no validator status for test_addr(2), voting_power comes from balance.
        // But quorum is validator_quorum = 3 (count), which means total_votes must be >= 3.
        gov.vote(id, 1, test_addr(2), 3, 0).unwrap();

        gov.queue(id, 1).unwrap();
        assert_eq!(gov.read_proposal_status(id), 2); // Queued
    });
}

#[test]
fn test_quorum_not_met_with_validators() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);

        // Seed 3 validators
        storage
            .sstore(
                call_precompile::VALIDATOR_ADDRESS,
                call_validator::slot_validator_count(),
                u128_to_u256(3),
            )
            .unwrap();

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

        // Vote with insufficient power (quorum = 3, but only 1 vote)
        gov.vote(id, 1, test_addr(2), 1, 0).unwrap();

        let result = gov.queue(id, 1);
        assert!(result.is_err(), "quorum not met should fail");
        assert_eq!(gov.read_proposal_status(id), 4); // Defeated
    });
}

// ── Pause blocks governance ───────────────────────────────────────

#[test]
fn test_vote_blocked_during_pause() {
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

        // Pause the chain
        gov.emergency_pause([1u8; 32], test_addr(99)).unwrap();
        assert!(gov.is_paused());

        // Vote should be rejected
        let result = gov.vote(id, 1, voter, 500_000, 0);
        assert!(result.is_err(), "vote during pause should fail");
    });
}

#[test]
fn test_queue_blocked_during_pause() {
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

        gov.vote(id, 1, test_addr(2), 1_000_000, 0).unwrap();

        // Pause the chain
        gov.emergency_pause([1u8; 32], test_addr(99)).unwrap();

        // Queue should be rejected
        let result = gov.queue(id, 1);
        assert!(result.is_err(), "queue during pause should fail");
    });
}

#[test]
fn test_execute_blocked_during_pause_except_emergency() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
        gov.write_config_u64(b"review_period", 0);
        gov.write_config_u64(b"voting_period", 100);
        gov.write_config_u64(b"timelock", 1);

        // Normal proposal
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

        // Pause the chain
        gov.emergency_pause([1u8; 32], test_addr(99)).unwrap();

        // Execute normal proposal during pause should fail
        let result = gov.execute(&mut asset, id, 2, proposer);
        assert!(result.is_err(), "execute normal proposal during pause should fail");
    });
}

// ── Cancel proposal ───────────────────────────────────────────────

#[test]
fn test_cancel_proposal_by_proposer_during_review() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
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

        assert_eq!(gov.read_proposal_status(id), 0); // Pending
        let balance_before = asset.read_balance(call_protocol::CALL_ASSET_ID, proposer);

        // Proposer cancels during review period
        gov.cancel_proposal(&mut asset, id, proposer).unwrap();

        assert_eq!(gov.read_proposal_status(id), 4); // Defeated
        let balance_after = asset.read_balance(call_protocol::CALL_ASSET_ID, proposer);
        assert_eq!(balance_after, balance_before + PROPOSAL_DEPOSIT); // Deposit refunded
    });
}

#[test]
fn test_cancel_proposal_rejected_after_review() {
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

        // Proposal is immediately active (review_period=0)
        assert_eq!(gov.read_proposal_status(id), 1); // Active

        // Cancel should fail
        let result = gov.cancel_proposal(&mut asset, id, proposer);
        assert!(result.is_err(), "cancel after review period should fail");
    });
}

#[test]
fn test_cancel_proposal_rejected_for_non_proposer() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));
        let mut asset = AssetStorage::new(StorageRef::new(&mut *storage));
        let proposer = test_addr(1);
        let rando = test_addr(2);

        seed_balance(storage, proposer, PROPOSAL_DEPOSIT * 2);
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

        let result = gov.cancel_proposal(&mut asset, id, rando);
        assert!(result.is_err(), "cancel by non-proposer should fail");
    });
}

// ── Emergency pause idempotent ────────────────────────────────────

#[test]
fn test_emergency_pause_rejects_double_pause() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));

        gov.emergency_pause([1u8; 32], test_addr(1)).unwrap();
        assert!(gov.is_paused());

        let result = gov.emergency_pause([2u8; 32], test_addr(1));
        assert!(result.is_err(), "double pause should fail");
    });
}

#[test]
fn test_emergency_resume_rejects_when_not_paused() {
    with_storage(0, |storage| {
        let mut gov = GovernanceStorage::new(StorageRef::new(&mut *storage));

        assert!(!gov.is_paused());

        let result = gov.emergency_resume();
        assert!(result.is_err(), "resume when not paused should fail");
    });
}
