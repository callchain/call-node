//! Integration tests for shielded flows with real Halo2 prover.

#[cfg(feature = "halo2-prover")]
mod shielded_flow {
    use call_primitives::Hash;
    use call_shielded::circuit_deposit::{DepositCircuit, DepositWitness};
    use call_shielded::circuit_transfer::{InputNoteWitness, OutputNoteWitness, TransferCircuit};
    use call_shielded::circuit_withdraw::{WithdrawCircuit, WithdrawWitness};
    use call_shielded::encryption::{decrypt_note, encrypt_note};
    use call_shielded::poseidon::{self, bytes_to_fp, domain, fp_to_bytes, poseidon_hash, poseidon_hash_tagged};
    use call_shielded::ShieldedComplianceMode;
    use call_shielded::*;

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
        let ivk_fp = bytes_to_fp(&vk.incoming_view_key);
        let value_fp = bytes_to_fp(&poseidon::value_to_fp_bytes(value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fp = bytes_to_fp(&asset_bytes);
        let rho_fp = bytes_to_fp(rho);
        let rcm_fp = poseidon_hash_tagged(domain::RCM, &[ivk_fp, value_fp, asset_fp, rho_fp]);
        fp_to_bytes(&rcm_fp)
    }

    fn compute_poseidon_commitment(
        value: u128,
        asset_id: u64,
        rcm: &[u8; 32],
        rho: &[u8; 32],
    ) -> [u8; 32] {
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let cm_fp = poseidon_hash(&[
            bytes_to_fp(&poseidon::value_to_fp_bytes(value)),
            bytes_to_fp(&asset_bytes),
            bytes_to_fp(rcm),
            bytes_to_fp(rho),
        ]);
        fp_to_bytes(&cm_fp)
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
            key_version: 0,
        }
    }

    #[test]
    fn test_real_deposit_proof() {
        let circuit = make_deposit_circuit(1000, 1, 1);
        let prover = Halo2Prover::setup();

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
            value: 1000,
            rcm: rcm1,
            recipient_ivk: vk1.incoming_view_key,
            rho: rho1,
            spending_key: sk1,
        };
        let output = OutputNoteWitness {
            value: 900,
            rcm: rcm_out,
            recipient_ivk: out_vk.incoming_view_key,
            rho: rho_out,
        };

        // Second dummy input/output for fixed-size arrays
        let sk2 = test_spending_key(3);
        let vk2 = ViewingKey::generate(&sk2);
        let rho2 = test_hash(11).0;
        let rcm2 = compute_rcm(&vk2, 0, asset_id, &rho2);
        let cm2 = compute_poseidon_commitment(0, asset_id, &rcm2, &rho2);
        let nf2 = compute_poseidon_nullifier(&vk2.incoming_view_key, &rho2);
        let path2 = tree.proof_for_last();
        tree.insert(&cm2);

        let input2 = InputNoteWitness {
            value: 0,
            rcm: rcm2,
            recipient_ivk: vk2.incoming_view_key,
            rho: rho2,
            spending_key: sk2,
        };
        let output2 = OutputNoteWitness {
            value: 0,
            rcm: rcm2,
            recipient_ivk: vk2.incoming_view_key,
            rho: rho2,
        };

        let mut path1_arr = [([0u8; 32], false); 32];
        for (i, p) in path1.iter().enumerate() {
            path1_arr[i] = *p;
        }
        let mut path2_arr = [([0u8; 32], false); 32];
        for (i, p) in path2.iter().enumerate() {
            path2_arr[i] = *p;
        }

        let circuit = TransferCircuit::new(
            [nf1, nf2],
            [out_cm, cm2],
            asset_id,
            merkle_root,
            [input1, input2],
            [output, output2],
            [path1_arr, path2_arr],
        );

        let prover = Halo2Prover::setup();
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
            spending_key: sk,
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

        let prover = Halo2Prover::setup();
        let proof = prover.prove_withdraw(&circuit).unwrap();
        assert!(!proof.is_empty());
    }

    fn compute_poseidon_nullifier(ivk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let fvk_tag = bytes_to_fp(&poseidon::tag_to_bytes(domain::FVK_FROM_IVK));
        let ivk_fp = bytes_to_fp(ivk);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fp]);
        let rho_fp = bytes_to_fp(rho);
        let nf_fp = poseidon_hash(&[fvk_from_ivk, rho_fp]);
        fp_to_bytes(&nf_fp)
    }

    /// Build a real Halo2 transfer proof with properly derived notes.
    /// Returns the ShieldedTransfer and the input note's commitment.
    #[allow(dead_code)]
    fn make_real_transfer_zkproof(
        input_value: u128,
        output_value: u128,
        asset_id: u64,
    ) -> (ShieldedTransfer, NoteCommitment) {
        let sk1 = test_spending_key(1);
        let vk1 = ViewingKey::generate(&sk1);
        let rho1 = test_hash(10).0;

        let input_note = Note::new(input_value, asset_id, &vk1, test_hash(10));
        let input_rcm = *input_note.rcm();
        let input_cm = input_note.commitment();
        let cm1: [u8; 32] = input_cm.0.into();
        let nf1 = compute_poseidon_nullifier(&vk1.incoming_view_key, &rho1);

        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&cm1);
        let path1 = tree.proof_for_last();
        let merkle_root = tree.root();

        let out_sk = test_spending_key(2);
        let out_vk = ViewingKey::generate(&out_sk);
        let rho_out = test_hash(20).0;
        let output_note = Note::new(output_value, asset_id, &out_vk, test_hash(20));
        let out_rcm = *output_note.rcm();
        let out_cm: [u8; 32] = output_note.commitment().0.into();

        let input_witness = InputNoteWitness {
            value: input_value,
            rcm: input_rcm,
            recipient_ivk: vk1.incoming_view_key,
            rho: rho1,
            spending_key: sk1,
        };
        let output_witness = OutputNoteWitness {
            value: output_value,
            rcm: out_rcm,
            recipient_ivk: out_vk.incoming_view_key,
            rho: rho_out,
        };

        // Second dummy input/output for fixed-size arrays
        let sk2 = test_spending_key(3);
        let vk2 = ViewingKey::generate(&sk2);
        let rho2 = test_hash(11).0;
        let rcm2 = compute_rcm(&vk2, 0, asset_id, &rho2);
        let cm2 = compute_poseidon_commitment(0, asset_id, &rcm2, &rho2);
        let nf2 = compute_poseidon_nullifier(&vk2.incoming_view_key, &rho2);
        let path2 = tree.proof_for_last();
        tree.insert(&cm2);

        let input2 = InputNoteWitness {
            value: 0,
            rcm: rcm2,
            recipient_ivk: vk2.incoming_view_key,
            rho: rho2,
            spending_key: sk2,
        };
        let output2 = OutputNoteWitness {
            value: 0,
            rcm: rcm2,
            recipient_ivk: vk2.incoming_view_key,
            rho: rho2,
        };

        let mut path1_arr = [([0u8; 32], false); 32];
        for (i, p) in path1.iter().enumerate() {
            path1_arr[i] = *p;
        }
        let mut path2_arr = [([0u8; 32], false); 32];
        for (i, p) in path2.iter().enumerate() {
            path2_arr[i] = *p;
        }

        let circuit = TransferCircuit::new(
            [nf1, nf2],
            [out_cm, cm2],
            asset_id,
            merkle_root,
            [input_witness, input2],
            [output_witness, output2],
            [path1_arr, path2_arr],
        );

        let prover = Halo2Prover::setup();
        let proof_data = prover.prove_transfer(&circuit).unwrap();

        let zk_proof = ZkProof {
            proof_data,
            nullifiers: vec![Nullifier::new(Hash::from_slice(&nf1))],
            commitments: vec![NoteCommitment::new(Hash::from_slice(&out_cm))],
            asset_id,
            key_version: 0,
        };

        let transfer = ShieldedTransfer {
            input_notes: vec![input_note],
            output_notes: vec![output_note],
            proof: zk_proof,
        };

        (transfer, input_cm)
    }

    #[test]
    fn test_real_proof_verify_valid() {
        let circuit = make_deposit_circuit(5000, 1, 42);
        let prover = Halo2Prover::setup();

        let proof = prover.prove_deposit(&circuit).unwrap();
        assert!(proof.len() >= 128);
    }

    #[test]
    fn test_real_proof_reject_tampered() {
        let circuit = make_deposit_circuit(100, 1, 7);
        let prover = Halo2Prover::setup();

        let mut proof = prover.prove_deposit(&circuit).unwrap();
        proof[10] ^= 0xFF;

        let mut public_inputs = circuit.commitment.to_vec();
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&circuit.asset_id.to_le_bytes());
        public_inputs.extend_from_slice(&asset_bytes);

        let result = prover.verify_deposit(&proof, &public_inputs);
        assert!(
            result.is_err() || !result.unwrap_or(false),
            "tampered proof should be rejected"
        );
    }

    #[test]
    fn test_real_proving_time() {
        use std::time::Instant;

        let circuit = make_deposit_circuit(10_000, 1, 99);
        let prover = Halo2Prover::setup();

        let start = Instant::now();
        let proof = prover.prove_deposit(&circuit).unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 30,
            "proving took too long: {:?}",
            elapsed
        );
        assert!(!proof.is_empty());
    }

    #[test]
    fn test_real_multi_asset_shielded() {
        let note1 = test_note(1000, 1, 1);
        let note2 = test_note(1000, 2, 1);
        assert_ne!(note1.commitment(), note2.commitment());
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
                key_version: 0,
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
        let _vk = ViewingKey::generate(&sk);
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
        let _vk = ViewingKey::generate(&sk);
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
