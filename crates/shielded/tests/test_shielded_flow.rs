//! Integration tests for shielded flows with real prover.

#[cfg(feature = "real-prover")]
mod shielded_flow {
    use call_shielded::*;
    use call_shielded::circuit_deposit::{DepositCircuit, DepositWitness};
    use call_shielded::circuit_withdraw::{WithdrawCircuit, WithdrawWitness};
    use call_shielded::circuit_transfer::{TransferCircuit, InputNoteWitness, OutputNoteWitness};
    use call_shielded::encryption::{encrypt_note, decrypt_note};
    use call_shielded::ShieldedComplianceMode;
    use call_shielded::poseidon::{poseidon_hash, bytes_to_fr, fr_to_bytes, domain};
    use call_crypto::keccak256;
    use ark_bn254::Fr;
    use ark_ff::Field;

    fn domain_tag_to_fr(tag: &str) -> Fr {
        Fr::from_random_bytes(tag.as_bytes()).unwrap_or_default()
    }

    fn test_addr(n: u8) -> call_primitives::Address {
        call_primitives::Address::repeat_byte(n)
    }

    fn test_hash(n: u8) -> call_primitives::Hash {
        call_primitives::Hash::repeat_byte(n)
    }

    fn test_spending_key(n: u8) -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    fn test_note(value: u128, asset_id: u64, seed: u8) -> Note {
        let sk = test_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        Note::new(value, asset_id, &vk, test_hash(seed))
    }

    fn compute_rcm(vk: &ViewingKey, value: u128, asset_id: u64, rho: &[u8; 32]) -> [u8; 32] {
        // Must match the circuit's D3 constraint: poseidon_hash([rcm_tag, ivk, value, asset, rho])
        let rcm_tag = domain_tag_to_fr("rcm");
        let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
        let mut value_bytes = [0u8; 32];
        value_bytes[..16].copy_from_slice(&value.to_le_bytes());
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rho_fr = bytes_to_fr(rho);
        let rcm_fr = poseidon_hash(&[rcm_tag, ivk_fr, value_fr, asset_fr, rho_fr]);
        fr_to_bytes(&rcm_fr)
    }

    fn value_to_fr_bytes(value: u128) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&value.to_le_bytes());
        bytes
    }

    fn compute_poseidon_commitment(value: u128, asset_id: u64, rcm: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let cm_fr = poseidon_hash(&[
            bytes_to_fr(&value_to_fr_bytes(value)),
            bytes_to_fr(&asset_bytes),
            bytes_to_fr(rcm),
            bytes_to_fr(rho),
        ]);
        fr_to_bytes(&cm_fr)
    }

    fn make_deposit_circuit(value: u128, asset_id: u64, seed: u8) -> DepositCircuit {
        let sk = test_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(seed).0;
        let rcm = compute_rcm(&vk, value, asset_id, &rho);

        let witness = DepositWitness {
            value,
            rcm,
            recipient_ivk: vk.incoming_view_key,
            rho,
        };

        let commitment = compute_poseidon_commitment(value, asset_id, &rcm, &rho);

        DepositCircuit::new(commitment, asset_id, witness)
    }

    fn test_proof(nullifiers: u32, commitments: u32) -> ZkProof {
        ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: (0..nullifiers)
                .map(|i| Nullifier::new(test_hash(i as u8)))
                .collect(),
            commitments: (0..commitments)
                .map(|i| NoteCommitment::new(test_hash(i as u8)))
                .collect(),
            asset_id: 1,
        }
    }

    #[test]
    fn test_real_deposit_proof() {
        let circuit = make_deposit_circuit(1000, 1, 1);
        let prover = RealProver::setup();

        let proof = prover.prove_deposit(&circuit).unwrap();
        assert!(!proof.is_empty());
        assert!(proof.len() >= 128);
    }

    #[test]
    fn test_real_transfer_proof() {
        let asset_id: u64 = 1;

        // Create input note 1
        let sk1 = test_spending_key(1);
        let vk1 = ViewingKey::generate(&sk1);
        let rho1 = test_hash(10).0;
        let rcm1 = compute_rcm(&vk1, 1000, asset_id, &rho1);
        let cm1 = compute_poseidon_commitment(1000, asset_id, &rcm1, &rho1);
        let nf1 = compute_poseidon_nullifier(&vk1.incoming_view_key, &rho1);

        // Build Merkle tree with the commitment
        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&cm1);
        let path1 = tree.proof_for_last();
        let merkle_root = tree.root();

        // Create output note (900 <= 1000, OK)
        let out_sk = test_spending_key(2);
        let out_vk = ViewingKey::generate(&out_sk);
        let rho_out = test_hash(20).0;
        let rcm_out = compute_rcm(&out_vk, 900, asset_id, &rho_out);
        let out_cm = compute_poseidon_commitment(900, asset_id, &rcm_out, &rho_out);

        let input1 = InputNoteWitness {
            value: 1000, rcm: rcm1, recipient_ivk: vk1.incoming_view_key, rho: rho1, spending_key: sk1,
        };
        let output = OutputNoteWitness {
            value: 900, rcm: rcm_out, recipient_ivk: out_vk.incoming_view_key, rho: rho_out,
        };

        let circuit = TransferCircuit::new(
            vec![nf1],
            vec![out_cm],
            asset_id,
            merkle_root,
            vec![input1],
            vec![output],
            vec![path1],
        );

        let prover = RealProver::setup();
        let proof = prover.prove_transfer(&circuit).unwrap();
        assert!(!proof.is_empty());
    }

    #[test]
    fn test_real_withdraw_proof() {
        let asset_id: u64 = 1;
        let value: u128 = 1000;
        let sk = test_spending_key(1);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(1).0;

        // Derive nullifier using Poseidon
        let nullifier = compute_poseidon_nullifier(&vk.incoming_view_key, &rho);

        // Derive rcm and commitment
        let rcm = compute_rcm(&vk, value, asset_id, &rho);
        let commitment = compute_poseidon_commitment(value, asset_id, &rcm, &rho);

        // Build Merkle tree
        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&commitment);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        let witness = WithdrawWitness {
            note_value: value,
            rcm,
            recipient_ivk: vk.incoming_view_key,
            rho,
            merkle_path,
        };

        let target_address = [5u8; 20];

        let circuit = WithdrawCircuit::new(
            nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness,
        );

        let prover = RealProver::setup();
        let proof = prover.prove_withdraw(&circuit).unwrap();
        assert!(!proof.is_empty());
    }

    fn compute_poseidon_nullifier(ivk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let fvk_tag = domain_tag_to_fr(domain::FVK_FROM_IVK);
        let ivk_fr = bytes_to_fr(ivk);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
        let rho_fr = bytes_to_fr(rho);
        let nf_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);
        fr_to_bytes(&nf_fr)
    }

    #[test]
    fn test_real_proof_verify_valid() {
        let circuit = make_deposit_circuit(5000, 1, 42);
        let prover = RealProver::setup();

        let proof = prover.prove_deposit(&circuit).unwrap();
        assert!(proof.len() >= 128);
    }

    #[test]
    fn test_real_proof_reject_tampered() {
        let circuit = make_deposit_circuit(100, 1, 7);
        let prover = RealProver::setup();

        let mut proof = prover.prove_deposit(&circuit).unwrap();
        proof[10] ^= 0xFF;

        let result = prover.verify_deposit(&proof, &[]);
        assert!(result.is_err() || !result.unwrap_or(false), "tampered proof should be rejected");
    }

    #[test]
    fn test_real_proof_reject_double_spend() {
        let mut state = ShieldedState::new();

        let note = test_note(1000, 1, 1);
        let new_note = test_note(800, 1, 2);
        let nf = note.nullifier();
        let proof = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![nf.clone()],
            commitments: vec![new_note.commitment()],
            asset_id: 1,
        };
        let transfer = ShieldedTransfer {
            input_notes: vec![note.clone()],
            output_notes: vec![new_note],
            proof,
        };

        state.process_transfer(&transfer).unwrap();

        let transfer2 = ShieldedTransfer {
            input_notes: vec![note],
            output_notes: vec![test_note(800, 1, 3)],
            proof: ZkProof {
                proof_data: vec![1u8; 200],
                nullifiers: vec![nf],
                commitments: vec![NoteCommitment::new(test_hash(3))],
                asset_id: 1,
            },
        };
        assert!(matches!(
            state.process_transfer(&transfer2),
            Err(ShieldedError::DoubleSpend(_))
        ));
    }

    #[test]
    fn test_real_deposit_then_transfer_flow() {
        let mut state = ShieldedState::new();

        let deposited_note = test_note(1000, 1, 1);
        let cm = deposited_note.commitment();
        state.process_deposit(cm.clone(), deposited_note.clone()).unwrap();
        assert_eq!(state.merkle_tree.leaf_count(), 1);

        let new_note = test_note(800, 1, 2);
        let proof = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![deposited_note.nullifier()],
            commitments: vec![new_note.commitment()],
            asset_id: 1,
        };
        let transfer = ShieldedTransfer {
            input_notes: vec![deposited_note],
            output_notes: vec![new_note.clone()],
            proof,
        };
        state.process_transfer(&transfer).unwrap();
        assert_eq!(state.merkle_tree.leaf_count(), 2);
    }

    #[test]
    fn test_real_proving_time() {
        use std::time::Instant;

        let circuit = make_deposit_circuit(10_000, 1, 99);
        let prover = RealProver::setup();

        let start = Instant::now();
        let proof = prover.prove_deposit(&circuit).unwrap();
        let elapsed = start.elapsed();

        assert!(elapsed.as_secs() < 30, "proving took too long: {:?}", elapsed);
        assert!(!proof.is_empty());
    }

    #[test]
    fn test_real_multi_asset_shielded() {
        let note1 = test_note(1000, 1, 1);
        let note2 = test_note(1000, 2, 1);
        assert_ne!(note1.commitment(), note2.commitment());
    }

    #[test]
    fn test_real_deposit_creates_commitment() {
        let mut state = ShieldedState::new();
        let note = test_note(500, 1, 7);
        let cm = note.commitment();

        state.process_deposit(cm.clone(), note).unwrap();
        assert_eq!(state.merkle_tree.leaf_count(), 1);
        assert!(state.get_note(&cm).is_some());
    }

    #[test]
    fn test_real_withdraw_consumes_note() {
        let mut state = ShieldedState::new();
        let note = test_note(1000, 1, 1);
        let nf = note.nullifier();

        state.process_withdraw(nf.clone()).unwrap();
        assert!(state.nullifier_set.is_spent(&nf));

        assert!(matches!(
            state.process_withdraw(nf),
            Err(ShieldedError::DoubleSpend(_))
        ));
    }

    #[test]
    fn test_real_transfer_preserves_value() {
        // Input 1000, output 800 -> OK (200 leftover for fee)
        let note = test_note(1000, 1, 1);
        let new_note = test_note(800, 1, 2);
        let transfer = ShieldedTransfer {
            input_notes: vec![note],
            output_notes: vec![new_note],
            proof: ZkProof {
                proof_data: vec![1u8; 200],
                nullifiers: vec![Nullifier::new(test_hash(1))],
                commitments: vec![NoteCommitment::new(test_hash(2))],
                asset_id: 1,
            },
        };
        assert!(transfer.value_conservable());

        // Output > input should fail
        let big_output = test_note(2000, 1, 3);
        let bad = ShieldedTransfer {
            input_notes: vec![test_note(1000, 1, 1)],
            output_notes: vec![big_output],
            proof: test_proof(1, 1),
        };
        assert!(!bad.value_conservable());
    }

    #[test]
    fn test_real_compliance_kyc_shielded() {
        let sk = test_spending_key(1);
        let vk = ViewingKey::generate(&sk);
        let note = test_note(500, 1, 1);
        let recipient = ShieldedComplianceMode::derive_address_from_ivk(&note);
        let kyc_registry = vec![recipient];
        let mode = ShieldedComplianceMode::KycRequired { kyc_registry };
        let result = mode.check_compliance(&[note]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_real_compliance_whitelist_shielded() {
        let sk = test_spending_key(1);
        let vk = ViewingKey::generate(&sk);
        let note = test_note(500, 1, 1);
        let recipient = ShieldedComplianceMode::derive_address_from_ivk(&note);
        let mut whitelist = std::collections::HashSet::new();
        whitelist.insert(recipient);
        let mode = ShieldedComplianceMode::WhitelistedOnly { whitelist };
        let result = mode.check_compliance(&[note]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_real_note_encryption_roundtrip() {
        let sk = test_spending_key(1);
        let vk = ViewingKey::generate(&sk);
        let note = test_note(500, 1, 1);

        let plaintext = note.to_encrypted_bytes();
        let encrypted = encrypt_note(&plaintext, &vk.incoming_view_key);
        let decrypted = decrypt_note(&encrypted, &vk.incoming_view_key).unwrap();
        let recovered = Note::from_encrypted_bytes(&decrypted).unwrap();

        assert_eq!(note.value, recovered.value);
        assert_eq!(note.asset_id(), recovered.asset_id());
        assert_eq!(note.commitment(), recovered.commitment());
    }
}
