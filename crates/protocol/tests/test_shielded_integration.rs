//! Shielded integration tests (Phase 12)

mod integration;

mod test_shielded_integration_impl {
    use super::integration::*;
    use call_primitives::Address;
    use call_protocol::balances::BalanceState;
    use call_protocol::compliance::ComplianceEngine;
    use call_protocol::registry::AssetRegistry;
    use call_protocol::instructions::Instruction;
    use call_protocol::transaction::{FeeParams, GasConfig};
    use call_shielded::{
        ShieldedState, ShieldedTransfer, ZkProof, Note, ViewingKey, Nullifier, NoteCommitment,
        ShieldedBlockTracker, ShieldedError,
    };

    fn shield_hash(n: u8) -> call_primitives::Hash {
        call_primitives::Hash::repeat_byte(n)
    }

    fn shield_spending_key(n: u8) -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    fn shield_note(value: u128, asset_id: u64, seed: u8) -> Note {
        let sk = shield_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        Note::new(value, asset_id, &vk, shield_hash(seed))
    }

    fn shield_proof(nullifiers: u32, commitments: u32) -> ZkProof {
        ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: (0..nullifiers)
                .map(|i| Nullifier::new(shield_hash(i as u8)))
                .collect(),
            commitments: (0..commitments)
                .map(|i| NoteCommitment::new(shield_hash(i as u8)))
                .collect(),
            asset_id: 1,
        }
    }

    // ── ShieldedDeposit Instruction ──────────────────────────────────────

    #[test]
    fn test_shielded_deposit_deducts_transparent_balance() {
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let sender = addr(1);
        let asset_id = setup_asset(&mut balances, &mut registry, "SHIELD", addr(10), sender, 5_000);

        let note = shield_note(1_000, asset_id, 1);
        let encrypted = note.to_encrypted_bytes();
        let cm = note.commitment();

        let instructions = vec![Instruction::ShieldedDeposit {
            asset_id,
            amount: 1_000,
            commitment: cm.0,
            encrypted_note: encrypted,
        }];
        let mut shielded_state2 = ShieldedState::new();
        call_protocol::instructions::execute_protocol_instructions(
            &instructions, &mut balances, &registry, &mut compliance, &mut shielded_state2, sender, None, &mut None, None,
        ).unwrap();

        // Transparent balance should be deducted
        assert_eq!(balances.get_balance(asset_id, &sender), 4_000);
        // Merkle tree should have grown
        assert_eq!(shielded_state2.merkle_tree.leaf_count(), 1);
    }

    // ── ShieldedWithdraw Instruction ─────────────────────────────────────

    #[test]
    fn test_shielded_withdraw_credits_transparent_balance() {
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let sender = addr(1);
        let receiver = addr(2);
        setup_asset(&mut balances, &mut registry, "SHIELD", addr(10), sender, 5_000);

        let note = shield_note(500, 0, 1);
        let nullifier = note.nullifier();

        let instructions = vec![Instruction::ShieldedWithdraw {
            asset_id: 0,
            target: receiver,
            amount: 500,
            proof: vec![1u8; 200],
            nullifier: nullifier.0,
        }];
        call_protocol::instructions::execute_protocol_instructions(
            &instructions, &mut balances, &registry, &mut compliance, &mut shielded_state, sender, None, &mut None, None,
        ).unwrap();

        // Transparent balance should be credited
        assert_eq!(balances.get_balance(0, &receiver), 500);
        // Nullifier should be consumed
        assert!(shielded_state.nullifier_set.is_spent(&nullifier));
    }

    // ── ShieldedTransfer Instruction ─────────────────────────────────────

    #[test]
    fn test_shielded_transfer_updates_nullifier_set() {
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let sender = addr(1);
        setup_asset(&mut balances, &mut registry, "SHIELD", addr(10), sender, 5_000);

        // Create input/output notes for a valid transfer
        let input_note = shield_note(1_000, 1, 1);
        let output_note = shield_note(800, 1, 2);
        let nullifier = input_note.nullifier();
        let commitment = output_note.commitment();

        let proof = shield_proof(1, 1);
        let instructions = vec![Instruction::ShieldedTransfer {
            asset_id: 1,
            proof: proof.proof_data,
            nullifiers: vec![nullifier.0],
            commitments: vec![commitment.0],
            encrypted_notes: vec![output_note.to_encrypted_bytes()],
        }];
        call_protocol::instructions::execute_protocol_instructions(
            &instructions, &mut balances, &registry, &mut compliance, &mut shielded_state, sender, None, &mut None, None,
        ).unwrap();

        // Nullifier should be marked spent
        assert!(shielded_state.nullifier_set.is_spent(&nullifier));
        // Merkle tree should have the new commitment
        assert_eq!(shielded_state.merkle_tree.leaf_count(), 1);
    }

    #[test]
    fn test_shielded_transfer_double_spend_rejected() {
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let sender = addr(1);
        setup_asset(&mut balances, &mut registry, "SHIELD", addr(10), sender, 5_000);

        let input_note = shield_note(1_000, 1, 1);
        let output_note = shield_note(800, 1, 2);
        let nullifier = input_note.nullifier();
        let commitment = output_note.commitment();

        let proof = shield_proof(1, 1);
        let instructions = vec![Instruction::ShieldedTransfer {
            asset_id: 1,
            proof: proof.proof_data.clone(),
            nullifiers: vec![nullifier.0],
            commitments: vec![commitment.0],
            encrypted_notes: vec![output_note.to_encrypted_bytes()],
        }];
        // First transfer should succeed
        call_protocol::instructions::execute_protocol_instructions(
            &instructions, &mut balances, &registry, &mut compliance, &mut shielded_state, sender, None, &mut None, None,
        ).unwrap();

        // Second transfer with same nullifier should fail
        let output_note2 = shield_note(800, 1, 3);
        let instructions2 = vec![Instruction::ShieldedTransfer {
            asset_id: 1,
            proof: proof.proof_data,
            nullifiers: vec![nullifier.0],
            commitments: vec![NoteCommitment::new(shield_hash(4)).0],
            encrypted_notes: vec![output_note2.to_encrypted_bytes()],
        }];
        let result = call_protocol::instructions::execute_protocol_instructions(
            &instructions2, &mut balances, &registry, &mut compliance, &mut shielded_state, sender, None, &mut None, None,
        );
        assert!(result.is_err());
    }

    // ── Per-Block Shielded Limit ─────────────────────────────────────────

    #[test]
    fn test_per_block_shielded_limit_tracker() {
        let mut tracker = ShieldedBlockTracker::default();
        for i in 0..50 {
            let note = shield_note(100, 1, i as u8);
            let transfer = ShieldedTransfer {
                input_notes: vec![note.clone()],
                output_notes: vec![note],
                proof: shield_proof(1, 1),
            };
            tracker.try_add(transfer).unwrap();
        }
        assert_eq!(tracker.count, 50);

        // 51st should fail
        let note = shield_note(100, 1, 99);
        let transfer = ShieldedTransfer {
            input_notes: vec![note.clone()],
            output_notes: vec![note],
            proof: shield_proof(1, 1),
        };
        assert!(matches!(
            tracker.try_add(transfer),
            Err(ShieldedError::LimitExceeded(50, 50))
        ));
    }

    #[test]
    fn test_per_block_tracker_reset() {
        let mut tracker = ShieldedBlockTracker::default();
        let note = shield_note(100, 1, 1);
        let transfer = ShieldedTransfer {
            input_notes: vec![note.clone()],
            output_notes: vec![note],
            proof: shield_proof(1, 1),
        };
        tracker.try_add(transfer).unwrap();
        assert_eq!(tracker.count, 1);

        tracker.reset();
        assert_eq!(tracker.count, 0);
        assert!(tracker.pending.is_empty());
    }

    // ── Shielded State Machine ───────────────────────────────────────────

    #[test]
    fn test_shielded_state_process_deposit() {
        let mut state = ShieldedState::new();
        let note = shield_note(1_000, 1, 1);
        let cm = note.commitment();

        state.process_deposit(cm.clone(), note.clone()).unwrap();
        assert_eq!(state.merkle_tree.leaf_count(), 1);
        assert!(state.get_note(&cm).is_some());
    }

    #[test]
    fn test_shielded_state_process_withdraw() {
        let mut state = ShieldedState::new();
        let note = shield_note(1_000, 1, 1);
        let nf = note.nullifier();

        state.process_withdraw(nf.clone()).unwrap();
        assert!(state.nullifier_set.is_spent(&nf));

        // Double withdraw should fail
        assert!(matches!(
            state.process_withdraw(nf),
            Err(ShieldedError::DoubleSpend(_))
        ));
    }

    #[test]
    fn test_shielded_state_merkle_root_changes_on_insert() {
        let mut state = ShieldedState::new();
        let root_before = state.merkle_root();
        let note = shield_note(500, 1, 1);
        state.merkle_tree.insert(note.commitment().0);
        assert_ne!(state.merkle_root(), root_before);
    }

    // ── Shielded Gas Cost ────────────────────────────────────────────────

    #[test]
    fn test_shielded_gas_calculation() {
        let gas_units = call_protocol::transaction::calculate_gas_units(&[
            Instruction::ShieldedDeposit {
                asset_id: 1,
                amount: 1_000,
                commitment: shield_hash(1),
                encrypted_note: vec![0u8; 100],
            },
        ]);
        // Shielded instructions should have gas cost
        assert!(gas_units > 0);
    }

    // ── Multi-Asset Shielded ─────────────────────────────────────────────

    #[test]
    fn test_multi_asset_shielded_commitments_differ() {
        let note1 = shield_note(1_000, 1, 1);
        let note2 = shield_note(1_000, 2, 1);
        // Same value, same seed, different asset_id -> different commitment
        assert_ne!(note1.commitment(), note2.commitment());
    }

    // ── Compliance on Shielded Flows ─────────────────────────────────────

    #[test]
    fn test_shielded_compliance_kyc_mode() {
        use call_shielded::ShieldedComplianceMode;

        let note = shield_note(500, 1, 1);
        let recipient = ShieldedComplianceMode::derive_address_from_ivk(&note);
        let kyc_registry = vec![recipient];
        let mode = ShieldedComplianceMode::KycRequired { kyc_registry };
        let result = mode.check_compliance(&[note]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_shielded_compliance_whitelist_mode() {
        use call_shielded::ShieldedComplianceMode;
        use std::collections::HashSet;

        let note = shield_note(500, 1, 1);
        let recipient = ShieldedComplianceMode::derive_address_from_ivk(&note);
        let mut whitelist = HashSet::new();
        whitelist.insert(recipient);
        let mode = ShieldedComplianceMode::WhitelistedOnly { whitelist };
        let result = mode.check_compliance(&[note]);
        assert!(result.is_ok());
    }
}
