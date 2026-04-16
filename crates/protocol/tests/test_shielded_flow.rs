//! Shielded flow integration tests
mod integration;

mod test_shielded_flow_impl {
    use super::integration::*;
    use call_primitives::{Address, AssetId, Hash};
    use call_protocol::balances::BalanceState;
    use call_protocol::registry::AssetRegistry;
    use call_protocol::instructions::Instruction;
    use call_shielded::{
        ShieldedState, ShieldedTransfer, ZkProof, Note, ViewingKey,
        Nullifier, NoteCommitment, ShieldedBlockTracker,
        verify_zk_proof, verify_shielded_balance, ShieldedError,
    };
    use call_crypto::keccak256;

    // Inline test helpers (test_utils is #[cfg(test)] only)
    fn shield_hash(n: u8) -> Hash {
        Hash::repeat_byte(n)
    }

    fn shield_spending_key(n: u8) -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    fn shield_note(value: u128, asset_id: AssetId, seed: u8) -> Note {
        let sk = shield_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        Note::new(value, asset_id, &vk, shield_hash(seed))
    }

    fn shield_proof(nullifiers: u32, commitments: u32) -> ZkProof {
        ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: (0..nullifiers).map(|i| Nullifier::new(shield_hash(i as u8))).collect(),
            commitments: (0..commitments).map(|i| NoteCommitment::new(shield_hash(i as u8))).collect(),
            asset_id: 1,
        }
    }

    #[test]
    fn test_shielded_state_init() {
        let state = ShieldedState::new();
        assert_eq!(state.merkle_tree.leaf_count(), 0);
        assert_eq!(state.nullifier_set.len(), 0);
    }

    #[test]
    fn test_shielded_state_process_transfer() {
        let mut state = ShieldedState::new();
        let input_note = shield_note(1_000, 1, 1);
        let output_note = shield_note(800, 1, 2);

        let proof = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![input_note.nullifier()],
            commitments: vec![output_note.commitment()],
            asset_id: 1,
        };
        let transfer = ShieldedTransfer {
            input_notes: vec![input_note],
            output_notes: vec![output_note.clone()],
            proof,
        };
        state.process_transfer(&transfer).unwrap();
        assert_eq!(state.merkle_tree.leaf_count(), 1);
        assert!(state.nullifier_set.is_spent(&transfer.proof.nullifiers[0]));
    }

    #[test]
    fn test_shielded_state_double_spend_rejected() {
        let mut state = ShieldedState::new();
        let input_note = shield_note(1_000, 1, 1);
        let output_note = shield_note(800, 1, 2);

        let proof = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![input_note.nullifier()],
            commitments: vec![output_note.commitment()],
            asset_id: 1,
        };
        state.process_transfer(&ShieldedTransfer {
            input_notes: vec![input_note.clone()],
            output_notes: vec![output_note.clone()],
            proof: proof.clone(),
        }).unwrap();

        let output_note2 = shield_note(800, 1, 3);
        let proof2 = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![input_note.nullifier()],
            commitments: vec![output_note2.commitment()],
            asset_id: 1,
        };
        assert!(matches!(
            state.process_transfer(&ShieldedTransfer {
                input_notes: vec![input_note],
                output_notes: vec![output_note2],
                proof: proof2,
            }),
            Err(ShieldedError::DoubleSpend(_))
        ));
    }

    #[test]
    fn test_shielded_state_value_violation() {
        let mut state = ShieldedState::new();
        let input_note = shield_note(500, 1, 1);
        let output_note = shield_note(1_000, 1, 2);

        let proof = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![input_note.nullifier()],
            commitments: vec![output_note.commitment()],
            asset_id: 1,
        };
        assert!(matches!(
            state.process_transfer(&ShieldedTransfer {
                input_notes: vec![input_note],
                output_notes: vec![output_note],
                proof,
            }),
            Err(ShieldedError::ValueViolation)
        ));
    }

    #[test]
    fn test_viewing_key_deterministic() {
        let sk = shield_spending_key(42);
        let vk1 = ViewingKey::generate(&sk);
        let vk2 = ViewingKey::generate(&sk);
        assert_eq!(vk1.incoming_view_key, vk2.incoming_view_key);
    }

    #[test]
    fn test_zk_proof_valid_accepted() {
        assert!(verify_zk_proof(&shield_proof(1, 2)));
    }

    #[test]
    fn test_zk_proof_empty_data_rejected() {
        let bad = ZkProof {
            proof_data: vec![],
            nullifiers: vec![Nullifier::new(shield_hash(1))],
            commitments: vec![NoteCommitment::new(shield_hash(2))],
            asset_id: 1,
        };
        assert!(!verify_zk_proof(&bad));
    }

    #[test]
    fn test_zk_proof_duplicate_nullifiers_rejected() {
        let nf = Nullifier::new(shield_hash(1));
        let dup = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![nf.clone(), nf],
            commitments: vec![NoteCommitment::new(shield_hash(2))],
            asset_id: 1,
        };
        assert!(!verify_zk_proof(&dup));
    }

    #[test]
    fn test_zk_proof_oversized_rejected() {
        let bad = ZkProof {
            proof_data: vec![1u8; 600],
            nullifiers: vec![Nullifier::new(shield_hash(1))],
            commitments: vec![NoteCommitment::new(shield_hash(2))],
            asset_id: 1,
        };
        assert!(!verify_zk_proof(&bad));
    }

    #[test]
    fn test_per_block_limit_50_allowed() {
        let mut tracker = ShieldedBlockTracker::default();
        for i in 0..50 {
            tracker.try_add(ShieldedTransfer {
                input_notes: vec![shield_note(100, 1, i as u8)],
                output_notes: vec![shield_note(100, 1, i as u8)],
                proof: shield_proof(1, 1),
            }).unwrap();
        }
        assert_eq!(tracker.count, 50);
    }

    #[test]
    fn test_per_block_51st_rejected() {
        let mut tracker = ShieldedBlockTracker::default();
        for i in 0..50 {
            tracker.try_add(ShieldedTransfer {
                input_notes: vec![shield_note(100, 1, i as u8)],
                output_notes: vec![shield_note(100, 1, i as u8)],
                proof: shield_proof(1, 1),
            }).unwrap();
        }
        assert!(matches!(
            tracker.try_add(ShieldedTransfer {
                input_notes: vec![shield_note(100, 1, 99)],
                output_notes: vec![shield_note(100, 1, 99)],
                proof: shield_proof(1, 1),
            }),
            Err(ShieldedError::LimitExceeded(50, 50))
        ));
    }

    #[test]
    fn test_tracker_reset() {
        let mut tracker = ShieldedBlockTracker::default();
        tracker.try_add(ShieldedTransfer {
            input_notes: vec![shield_note(100, 1, 1)],
            output_notes: vec![shield_note(100, 1, 1)],
            proof: shield_proof(1, 1),
        }).unwrap();
        tracker.reset();
        assert_eq!(tracker.count, 0);
    }

    #[test]
    fn test_shielded_balance_verification() {
        assert!(verify_shielded_balance(500, 500, 1_000));
        assert!(!verify_shielded_balance(600, 500, 1_000));
    }

    #[test]
    fn test_shielded_deposit_instruction() {
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let sender = addr(1);
        let asset_id = setup_asset(&mut balances, &mut registry, "SHIELD", addr(10), sender, 5_000);

        // Create a valid encrypted note for deposit
        let note = shield_note(1_000, asset_id, 1);
        let encrypted = note.to_encrypted_bytes();
        let cm = note.commitment();

        let instructions = vec![Instruction::ShieldedDeposit { asset_id, amount: 1_000, commitment: cm.0, encrypted_note: encrypted }];
        call_protocol::instructions::execute_protocol_instructions(&instructions, &mut balances, &registry, &mut compliance, &mut shielded_state, sender).unwrap();
        assert_eq!(balances.get_balance(asset_id, &sender), 4_000);
        assert_eq!(shielded_state.merkle_tree.leaf_count(), 1);
    }

    #[test]
    fn test_shielded_withdraw_instruction() {
        let mut balances = BalanceState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let sender = addr(1);
        let receiver = addr(2);
        setup_asset(&mut balances, &mut registry, "SHIELD", addr(10), sender, 5_000);

        // Create a valid nullifier for withdraw
        let note = shield_note(500, 0, 1);
        let nullifier = note.nullifier();

        let instructions = vec![Instruction::ShieldedWithdraw { asset_id: 0, target: receiver, amount: 500, proof: vec![1u8; 200], nullifier: nullifier.0 }];
        call_protocol::instructions::execute_protocol_instructions(&instructions, &mut balances, &registry, &mut compliance, &mut shielded_state, sender).unwrap();
        assert_eq!(balances.get_balance(0, &receiver), 500);
        assert!(shielded_state.nullifier_set.is_spent(&nullifier));
    }

    #[test]
    fn test_note_commitment_nullifier_size() {
        let note = shield_note(500, 1, 7);
        assert_eq!(note.commitment().as_hash().as_slice().len(), 32);
        assert_eq!(note.nullifier().as_hash().as_slice().len(), 32);
    }

    #[test]
    fn test_note_commitment_deterministic() {
        let note = shield_note(500, 1, 7);
        assert_eq!(note.commitment(), note.commitment());
    }

    #[test]
    fn test_merkle_root_changes_on_insert() {
        let mut state = ShieldedState::new();
        let root_before = state.merkle_root();
        let note = shield_note(500, 1, 1);
        state.merkle_tree.insert(note.commitment().0);
        assert_ne!(state.merkle_root(), root_before);
    }

    #[test]
    fn test_viewing_key_balance_disclosure() {
        let sk = shield_spending_key(10);
        let vk = ViewingKey::generate(&sk);
        let note = shield_note(500, 1, 10);
        assert!(vk.can_decrypt(note.rcm()));
    }

    #[test]
    fn test_zk_proof_empty_nullifiers_rejected() {
        // A proof with empty nullifiers AND empty commitments should be rejected
        let bad = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![],
            commitments: vec![],
            asset_id: 1,
        };
        assert!(!verify_zk_proof(&bad));
    }

    #[test]
    fn test_note_nullifier_deterministic() {
        let note = shield_note(500, 1, 7);
        let nf1 = note.nullifier();
        let nf2 = note.nullifier();
        assert_eq!(nf1, nf2);
    }
}
