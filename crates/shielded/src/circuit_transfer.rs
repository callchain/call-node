//! ShieldedTransfer circuit — proves a user is transferring shielded funds to new
//! shielded notes, consuming input notes and creating output notes, without revealing
//! spending keys.
//!
//! This is the most complex ZK circuit in the shielded pool. It proves:
//! - The spender owns the input notes (via nullifier derivation)
//! - The input notes exist in the Merkle tree
//! - Output notes are properly formed commitments
//! - No value is created: sum(output values) <= sum(input values)
//! - All notes are for the same asset type
//!
//! Public inputs:
//! - nullifiers: Vec<[u8; 32]> (N inputs)
//! - commitments: Vec<[u8; 32]> (M outputs)
//! - asset_id: u64
//! - merkle_root: [u8; 32]
//!
//! Private witnesses:
//! - input_notes: Vec<InputNoteWitness> — each has {value, rcm, recipient_ivk, rho, spending_key}
//! - output_notes: Vec<OutputNoteWitness> — each has {value, rcm, recipient_ivk, rho}
//! - merkle_paths: Vec<Vec<([u8; 32], bool)>> — one per input note
//!
//! Constraints:
//!   T1. Nullifier derivation: For each input, poseidon_hash(poseidon_hash("fvk_from_ivk" || ivk), rho) == public nullifier
//!   T2. Merkle path validity: For each input, walk path with poseidon_hash, enforce root == public merkle_root
//!   T3. Spending rights: IVK derived from spending_key matches note IVK
//!   T4. Value conservation: sum(output values) <= sum(input values), enforce non-negative diff
//!   T5. Range & asset validity: Non-zero check for all notes, 128-bit range, asset_id match

use crate::poseidon::domain;
use crate::poseidon::{bytes_to_fr, poseidon_hash_tagged};
use ark_bn254::Fr;
use ark_ff::{Field, Zero};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

// ============================================================================
// Witness types
// ============================================================================

/// Private witness data for a single input (spent) note.
#[derive(Debug, Clone)]
pub struct InputNoteWitness {
    /// Note value
    pub value: u128,
    /// Random commitment material
    pub rcm: [u8; 32],
    /// Incoming viewing key of the recipient
    pub recipient_ivk: [u8; 32],
    /// Nullifier randomness (rho)
    pub rho: [u8; 32],
    /// Spending key that authorizes spending this note
    pub spending_key: [u8; 32],
}

/// Private witness data for a single output (created) note.
#[derive(Debug, Clone)]
pub struct OutputNoteWitness {
    /// Note value
    pub value: u128,
    /// Random commitment material
    pub rcm: [u8; 32],
    /// Incoming viewing key of the recipient
    pub recipient_ivk: [u8; 32],
    /// Nullifier randomness (rho) for future spends
    pub rho: [u8; 32],
}

// ============================================================================
// Circuit
// ============================================================================

/// ShieldedTransfer circuit.
///
/// Proves that the prover:
/// - Owns N input shielded notes (valid nullifier derivation from spending keys)
/// - Those input notes exist in the Merkle tree (valid Merkle paths to public root)
/// - Is creating M output shielded notes (valid commitments)
/// - Is not creating value: sum(outputs) <= sum(inputs)
/// - All notes use the same asset_id
#[derive(Debug, Clone)]
pub struct TransferCircuit {
    /// Public inputs
    pub nullifiers: Vec<[u8; 32]>,
    pub commitments: Vec<[u8; 32]>,
    pub asset_id: u64,
    pub merkle_root: [u8; 32],
    /// Private witnesses
    pub input_notes: Option<Vec<InputNoteWitness>>,
    pub output_notes: Option<Vec<OutputNoteWitness>>,
    pub merkle_paths: Option<Vec<Vec<([u8; 32], bool)>>>,
}

impl TransferCircuit {
    /// Create a new transfer circuit from public data and private witnesses.
    pub fn new(
        nullifiers: Vec<[u8; 32]>,
        commitments: Vec<[u8; 32]>,
        asset_id: u64,
        merkle_root: [u8; 32],
        input_notes: Vec<InputNoteWitness>,
        output_notes: Vec<OutputNoteWitness>,
        merkle_paths: Vec<Vec<([u8; 32], bool)>>,
    ) -> Self {
        Self {
            nullifiers,
            commitments,
            asset_id,
            merkle_root,
            input_notes: Some(input_notes),
            output_notes: Some(output_notes),
            merkle_paths: Some(merkle_paths),
        }
    }

    /// Create a circuit with only public data (for verification only).
    pub fn for_verify(
        nullifiers: Vec<[u8; 32]>,
        commitments: Vec<[u8; 32]>,
        asset_id: u64,
        merkle_root: [u8; 32],
    ) -> Self {
        Self {
            nullifiers,
            commitments,
            asset_id,
            merkle_root,
            input_notes: None,
            output_notes: None,
            merkle_paths: None,
        }
    }
}

impl ConstraintSynthesizer<Fr> for TransferCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        use crate::poseidon::gadget::poseidon_hash_gadget;
        use ark_r1cs_std::alloc::AllocVar;
        use ark_r1cs_std::boolean::Boolean;
        use ark_r1cs_std::eq::EqGadget;
        use ark_r1cs_std::fields::fp::FpVar;
        use ark_r1cs_std::prelude::ToBitsGadget;

        let inputs = self.input_notes.ok_or(SynthesisError::AssignmentMissing)?;
        let outputs = self.output_notes.ok_or(SynthesisError::AssignmentMissing)?;
        let paths = self.merkle_paths.ok_or(SynthesisError::AssignmentMissing)?;

        // Structural consistency (determines circuit topology)
        if inputs.len() != self.nullifiers.len() {
            return Err(SynthesisError::Unsatisfiable);
        }
        if outputs.len() != self.commitments.len() {
            return Err(SynthesisError::Unsatisfiable);
        }
        if inputs.len() != paths.len() {
            return Err(SynthesisError::Unsatisfiable);
        }
        if inputs.is_empty() || outputs.is_empty() {
            return Err(SynthesisError::Unsatisfiable);
        }

        // --- Public inputs ---
        let asset_id_fr = {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&self.asset_id.to_le_bytes());
            bytes_to_fr(&b)
        };
        let asset_id_var = FpVar::new_input(cs.clone(), || Ok(asset_id_fr))?;

        let merkle_root_fr = bytes_to_fr(&self.merkle_root);
        let merkle_root_var = FpVar::new_input(cs.clone(), || Ok(merkle_root_fr))?;

        let nullifier_vars: Vec<_> = self
            .nullifiers
            .iter()
            .map(|nf| {
                let nf_fr = bytes_to_fr(nf);
                FpVar::new_input(cs.clone(), || Ok(nf_fr))
            })
            .collect::<Result<_, _>>()?;

        let commitment_vars: Vec<_> = self
            .commitments
            .iter()
            .map(|cm| {
                let cm_fr = bytes_to_fr(cm);
                FpVar::new_input(cs.clone(), || Ok(cm_fr))
            })
            .collect::<Result<_, _>>()?;

        let fvk_tag_fr = domain_tag_to_fr(domain::FVK_FROM_IVK);
        let fvk_tag_var = FpVar::new_constant(cs.clone(), fvk_tag_fr)?;
        let ivk_tag_fr = domain_tag_to_fr(domain::IVK_FROM_SK);
        let ivk_tag_var = FpVar::new_constant(cs.clone(), ivk_tag_fr)?;

        // Running sums for value conservation
        let mut input_sum = FpVar::new_constant(cs.clone(), Fr::zero())?;
        let mut output_sum = FpVar::new_constant(cs.clone(), Fr::zero())?;

        // ------------------------------------------------------------------
        // Process each input note: nullifier + Merkle path + spending rights
        // ------------------------------------------------------------------
        for (i, (input, path)) in inputs.iter().zip(paths.iter()).enumerate() {
            let value_fr = bytes_to_fr(&value_to_fr_bytes(input.value));
            let value_var = FpVar::new_witness(cs.clone(), || Ok(value_fr))?;

            let rcm_fr = bytes_to_fr(&input.rcm);
            let rcm_var = FpVar::new_witness(cs.clone(), || Ok(rcm_fr))?;

            let ivk_fr = bytes_to_fr(&input.recipient_ivk);
            let ivk_var = FpVar::new_witness(cs.clone(), || Ok(ivk_fr))?;

            let rho_fr = bytes_to_fr(&input.rho);
            let rho_var = FpVar::new_witness(cs.clone(), || Ok(rho_fr))?;

            let sk_fr = bytes_to_fr(&input.spending_key);
            let sk_var = FpVar::new_witness(cs.clone(), || Ok(sk_fr))?;

            // T1: Nullifier derivation
            let fvk_from_ivk =
                poseidon_hash_gadget(cs.clone(), &[fvk_tag_var.clone(), ivk_var.clone()])?;
            let computed_nf = poseidon_hash_gadget(cs.clone(), &[fvk_from_ivk, rho_var.clone()])?;
            computed_nf.enforce_equal(&nullifier_vars[i])?;

            // T3: Spending rights — IVK derived from spending_key must match note IVK
            let derived_ivk = poseidon_hash_gadget(cs.clone(), &[ivk_tag_var.clone(), sk_var])?;
            derived_ivk.enforce_equal(&ivk_var)?;

            // T2: Merkle path validity
            let note_commitment = poseidon_hash_gadget(
                cs.clone(),
                &[
                    value_var.clone(),
                    asset_id_var.clone(),
                    rcm_var.clone(),
                    rho_var.clone(),
                ],
            )?;

            let mut current = note_commitment;
            for (sibling_hash, sibling_is_right) in path {
                let sibling_fr = bytes_to_fr(sibling_hash);
                let sibling_var = FpVar::new_witness(cs.clone(), || Ok(sibling_fr))?;
                current = if *sibling_is_right {
                    poseidon_hash_gadget(cs.clone(), &[current, sibling_var])?
                } else {
                    poseidon_hash_gadget(cs.clone(), &[sibling_var, current])?
                };
            }
            current.enforce_equal(&merkle_root_var)?;

            // T5: Range — non-zero value
            let value_inv = FpVar::new_witness(cs.clone(), || {
                if value_fr.is_zero() {
                    Err(SynthesisError::Unsatisfiable)
                } else {
                    Ok(value_fr
                        .inverse()
                        .expect("invariant: non-zero field element has inverse"))
                }
            })?;
            let one = FpVar::new_constant(cs.clone(), Fr::from(1u64))?;
            (value_var.clone() * value_inv).enforce_equal(&one)?;

            // T5: Range — 128-bit
            let bits = value_var.to_bits_le()?;
            for bit in &bits[128..] {
                bit.enforce_equal(&Boolean::constant(false))?;
            }

            input_sum = input_sum + value_var;
        }

        // ------------------------------------------------------------------
        // Process each output note: commitment validity
        // ------------------------------------------------------------------
        for (i, output) in outputs.iter().enumerate() {
            let value_fr = bytes_to_fr(&value_to_fr_bytes(output.value));
            let value_var = FpVar::new_witness(cs.clone(), || Ok(value_fr))?;

            let rcm_fr = bytes_to_fr(&output.rcm);
            let rcm_var = FpVar::new_witness(cs.clone(), || Ok(rcm_fr))?;

            let rho_fr = bytes_to_fr(&output.rho);
            let rho_var = FpVar::new_witness(cs.clone(), || Ok(rho_fr))?;

            // Recompute commitment: H(value || asset_id || rcm || rho)
            let computed_cm = poseidon_hash_gadget(
                cs.clone(),
                &[
                    value_var.clone(),
                    asset_id_var.clone(),
                    rcm_var.clone(),
                    rho_var.clone(),
                ],
            )?;
            computed_cm.enforce_equal(&commitment_vars[i])?;

            // T5: Range — non-zero value
            let value_inv = FpVar::new_witness(cs.clone(), || {
                if value_fr.is_zero() {
                    Err(SynthesisError::Unsatisfiable)
                } else {
                    Ok(value_fr
                        .inverse()
                        .expect("invariant: non-zero field element has inverse"))
                }
            })?;
            let one = FpVar::new_constant(cs.clone(), Fr::from(1u64))?;
            (value_var.clone() * value_inv).enforce_equal(&one)?;

            // T5: Range — 128-bit
            let bits = value_var.to_bits_le()?;
            for bit in &bits[128..] {
                bit.enforce_equal(&Boolean::constant(false))?;
            }

            output_sum = output_sum + value_var;
        }

        // T4: Value conservation — diff = input_sum - output_sum, enforce diff >= 0
        let diff = input_sum.clone() - output_sum.clone();
        (diff.clone() + output_sum).enforce_equal(&input_sum)?;

        let diff_bits = diff.to_bits_le()?;
        for bit in &diff_bits[128..] {
            bit.enforce_equal(&Boolean::constant(false))?;
        }

        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Convert a domain tag string to Fr.
fn domain_tag_to_fr(tag: &str) -> Fr {
    Fr::from_random_bytes(tag.as_bytes()).unwrap_or_default()
}

/// Convert a u128 value to a 32-byte Fr-compatible representation (zero-padded LE).
fn value_to_fr_bytes(value: u128) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(&value.to_le_bytes());
    bytes
}

/// Derive the incoming viewing key Fr from a spending key.
///
/// Matches ViewingKey::generate: ivk = poseidon_hash_tagged("call/shielded/ivk", [sk_fr]).
#[allow(dead_code)]
fn derive_ivk_from_spending_key(spending_key: &[u8; 32]) -> Fr {
    let sk_fr = bytes_to_fr(spending_key);
    poseidon_hash_tagged(domain::IVK_FROM_SK, &[sk_fr])
}

/// Compute the public input byte count for a transfer circuit with N inputs and M outputs.
pub const fn transfer_public_input_count(n_inputs: usize, n_outputs: usize) -> usize {
    n_inputs * 32   // nullifiers
        + n_outputs * 32  // commitments
        + 8               // asset_id
        + 32 // merkle_root
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(all(test, feature = "real-prover"))]
mod tests {
    use super::*;
    use crate::merkle_poseidon::PoseidonMerkleTree;
    use crate::poseidon::{fr_to_bytes, poseidon_hash, poseidon_hash_tagged};
    use crate::test_utils::{test_hash, test_spending_key};
    use crate::ViewingKey;

    /// Compute rcm for a note using Poseidon (matches Note::new and circuit D3).
    fn compute_rcm_plain(vk: &ViewingKey, value: u128, asset_id: u64, rho: &[u8; 32]) -> [u8; 32] {
        let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
        let value_bytes = value_to_fr_bytes(value);
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rho_fr = bytes_to_fr(rho);
        let rcm_fr = poseidon_hash_tagged(domain::RCM, &[ivk_fr, value_fr, asset_fr, rho_fr]);
        fr_to_bytes(&rcm_fr)
    }

    /// Derive nullifier from ivk and rho using the Poseidon hash method.
    fn derive_nullifier_plain(ivk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let fvk_tag = domain_tag_to_fr(domain::FVK_FROM_IVK);
        let ivk_fr = bytes_to_fr(ivk);
        let rho_fr = bytes_to_fr(rho);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
        let nf_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);
        fr_to_bytes(&nf_fr)
    }

    /// Compute a note commitment: H(value || asset_id || rcm || rho)
    fn compute_commitment_plain(
        value: u128,
        asset_id: u64,
        rcm: &[u8; 32],
        rho: &[u8; 32],
    ) -> [u8; 32] {
        let value_bytes = value_to_fr_bytes(value);
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rcm_fr = bytes_to_fr(rcm);
        let rho_fr = bytes_to_fr(rho);
        let cm_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
        fr_to_bytes(&cm_fr)
    }

    /// Build a single input note witness with all derived data.
    fn make_input_note(
        value: u128,
        asset_id: u64,
        spending_key: &[u8; 32],
        rho: [u8; 32],
    ) -> (
        InputNoteWitness,
        [u8; 32], /* nullifier */
        [u8; 32], /* commitment */
    ) {
        let vk = ViewingKey::generate(spending_key);
        let rcm = compute_rcm_plain(&vk, value, asset_id, &rho);
        let nullifier = derive_nullifier_plain(&vk.incoming_view_key, &rho);
        let commitment = compute_commitment_plain(value, asset_id, &rcm, &rho);
        let witness = InputNoteWitness {
            value,
            rcm,
            recipient_ivk: vk.incoming_view_key,
            rho,
            spending_key: *spending_key,
        };
        (witness, nullifier, commitment)
    }

    /// Build a single output note witness.
    fn make_output_note(
        value: u128,
        asset_id: u64,
        recipient_ivk: [u8; 32],
        rho: [u8; 32],
    ) -> (OutputNoteWitness, [u8; 32] /* commitment */) {
        let ivk_fr = bytes_to_fr(&recipient_ivk);
        let value_bytes = value_to_fr_bytes(value);
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rho_fr = bytes_to_fr(&rho);
        let rcm_fr = poseidon_hash_tagged(domain::RCM, &[ivk_fr, value_fr, asset_fr, rho_fr]);
        let rcm = fr_to_bytes(&rcm_fr);
        let commitment = compute_commitment_plain(value, asset_id, &rcm, &rho);
        let witness = OutputNoteWitness {
            value,
            rcm,
            recipient_ivk,
            rho,
        };
        (witness, commitment)
    }

    /// Build complete test data for a 1-input, 1-output transfer.
    fn make_1in_1out_data(input_value: u128, output_value: u128, asset_id: u64) -> TransferCircuit {
        let sk = test_spending_key(1);
        let rho_in = test_hash(10).0;
        let (input_witness, nullifier, input_cm) =
            make_input_note(input_value, asset_id, &sk, rho_in);

        // Build Merkle tree with input commitment
        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&input_cm);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        // Output to a different recipient
        let out_sk = test_spending_key(2);
        let out_vk = ViewingKey::generate(&out_sk);
        let rho_out = test_hash(20).0;
        let (output_witness, output_cm) =
            make_output_note(output_value, asset_id, out_vk.incoming_view_key, rho_out);

        TransferCircuit::new(
            vec![nullifier],
            vec![output_cm],
            asset_id,
            merkle_root,
            vec![input_witness],
            vec![output_witness],
            vec![merkle_path],
        )
    }

    #[test]
    fn test_transfer_circuit_satisfiable_1in_1out() {
        let circuit = make_1in_1out_data(1000, 900, 1);

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_transfer_circuit_satisfiable_2in_2out() {
        let asset_id: u64 = 1;

        // Create two input notes from different spending keys
        let sk1 = test_spending_key(10);
        let sk2 = test_spending_key(11);
        let rho_in1 = test_hash(30).0;
        let rho_in2 = test_hash(31).0;

        let (in1, nf1, cm1) = make_input_note(500, asset_id, &sk1, rho_in1);
        let (in2, nf2, cm2) = make_input_note(700, asset_id, &sk2, rho_in2);

        // Build Merkle tree with both commitments
        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&cm1);
        tree.insert(&cm2);
        let path1 = tree.proof_for_index(0).unwrap();
        let path2 = tree.proof_for_index(1).unwrap();
        let merkle_root = tree.root();

        // Create two output notes
        let out_vk1 = ViewingKey::generate(&test_spending_key(20));
        let out_vk2 = ViewingKey::generate(&test_spending_key(21));
        let rho_out1 = test_hash(40).0;
        let rho_out2 = test_hash(41).0;

        let (out1, out_cm1) = make_output_note(600, asset_id, out_vk1.incoming_view_key, rho_out1);
        let (out2, out_cm2) = make_output_note(500, asset_id, out_vk2.incoming_view_key, rho_out2);

        let circuit = TransferCircuit::new(
            vec![nf1, nf2],
            vec![out_cm1, out_cm2],
            asset_id,
            merkle_root,
            vec![in1, in2],
            vec![out1, out2],
            vec![path1, path2],
        );

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_transfer_circuit_nullifier_derivation() {
        let sk = test_spending_key(7);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(7).0;

        // Derive nullifier: H(H("fvk_from_ivk" || ivk), rho)
        let fvk_tag = domain_tag_to_fr(domain::FVK_FROM_IVK);
        let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
        let rho_fr = bytes_to_fr(&rho);
        let nf1 = poseidon_hash(&[fvk_from_ivk, rho_fr]);

        // Repeat: must be deterministic
        let fvk_tag2 = domain_tag_to_fr(domain::FVK_FROM_IVK);
        let ivk_fr2 = bytes_to_fr(&vk.incoming_view_key);
        let fvk_from_ivk2 = poseidon_hash(&[fvk_tag2, ivk_fr2]);
        let nf2 = poseidon_hash(&[fvk_from_ivk2, rho_fr]);

        assert_eq!(nf1, nf2, "nullifier derivation must be deterministic");
        assert_ne!(nf1, Fr::zero(), "nullifier must be non-zero");

        // Different rho -> different nullifier
        let rho2 = test_hash(8).0;
        let rho_fr2 = bytes_to_fr(&rho2);
        let nf3 = poseidon_hash(&[fvk_from_ivk, rho_fr2]);
        assert_ne!(nf1, nf3, "different rho must produce different nullifier");
    }

    #[test]
    fn test_transfer_circuit_value_conservation() {
        // Input 1000, output 800 -> OK (200 leftover for fee)
        let circuit = make_1in_1out_data(1000, 800, 1);
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());

        // Exact match: input 500, output 500 -> OK
        let circuit2 = make_1in_1out_data(500, 500, 1);
        let cs2 = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit2.generate_constraints(cs2.clone()).unwrap();
        assert!(cs2.is_satisfied().unwrap());
    }

    #[test]
    fn test_transfer_circuit_value_creation_rejected() {
        // Build a circuit where outputs > inputs
        let sk = test_spending_key(1);
        let rho_in = test_hash(10).0;
        let input_value: u128 = 100;
        let output_value: u128 = 200; // More than input

        let (input_witness, nullifier, input_cm) = make_input_note(input_value, 1, &sk, rho_in);

        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&input_cm);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        let out_vk = ViewingKey::generate(&test_spending_key(2));
        let rho_out = test_hash(20).0;
        let (output_witness, output_cm) =
            make_output_note(output_value, 1, out_vk.incoming_view_key, rho_out);

        let circuit = TransferCircuit::new(
            vec![nullifier],
            vec![output_cm],
            1,
            merkle_root,
            vec![input_witness],
            vec![output_witness],
            vec![merkle_path],
        );

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(
            !cs.is_satisfied().unwrap(),
            "circuit should reject output > input"
        );
    }

    #[test]
    fn test_transfer_circuit_range_check_nonzero() {
        // Build a circuit with zero-value input note
        let sk = test_spending_key(1);
        let rho_in = test_hash(10).0;

        let (input_witness, nullifier, input_cm) = make_input_note(0, 1, &sk, rho_in);

        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&input_cm);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        let out_vk = ViewingKey::generate(&test_spending_key(2));
        let rho_out = test_hash(20).0;
        let (output_witness, output_cm) = make_output_note(
            100, // Non-zero output
            1,
            out_vk.incoming_view_key,
            rho_out,
        );

        let circuit = TransferCircuit::new(
            vec![nullifier],
            vec![output_cm],
            1,
            merkle_root,
            vec![input_witness],
            vec![output_witness],
            vec![merkle_path],
        );

        // Should fail: zero input value
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        let result = circuit.generate_constraints(cs.clone());
        assert!(
            result.is_err() || !cs.is_satisfied().unwrap(),
            "circuit should reject zero-value input"
        );

        // Also test zero output value
        let (input_witness2, nullifier2, input_cm2) = make_input_note(100, 1, &sk, test_hash(11).0);

        let mut tree2 = PoseidonMerkleTree::new(32);
        tree2.insert(&input_cm2);
        let merkle_root2 = tree2.root();
        let merkle_path2 = tree2.proof_for_last();

        let (output_witness2, output_cm2) = make_output_note(
            0, // Zero output
            1,
            out_vk.incoming_view_key,
            test_hash(21).0,
        );

        let circuit2 = TransferCircuit::new(
            vec![nullifier2],
            vec![output_cm2],
            1,
            merkle_root2,
            vec![input_witness2],
            vec![output_witness2],
            vec![merkle_path2],
        );

        let cs2 = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        let result2 = circuit2.generate_constraints(cs2.clone());
        assert!(
            result2.is_err() || !cs2.is_satisfied().unwrap(),
            "circuit should reject zero-value output"
        );
    }

    #[test]
    fn test_transfer_circuit_asset_id_consistency() {
        // Build a circuit where input uses asset_id=1, but public asset_id=2
        let sk = test_spending_key(1);
        let rho_in = test_hash(10).0;

        // Input note created with asset_id=1
        let (input_witness, nullifier, _input_cm_wrong) = make_input_note(1000, 1, &sk, rho_in);

        // But we compute commitment with asset_id=1 (matching the note)
        let input_cm = compute_commitment_plain(1000, 1, &input_witness.rcm, &rho_in);

        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&input_cm);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        // Output note with asset_id=1
        let out_vk = ViewingKey::generate(&test_spending_key(2));
        let rho_out = test_hash(20).0;
        let (output_witness, output_cm) =
            make_output_note(900, 1, out_vk.incoming_view_key, rho_out);

        // Public asset_id=2, but notes use asset_id=1 -> should fail
        let circuit = TransferCircuit::new(
            vec![nullifier],
            vec![output_cm],
            2, // Wrong asset_id
            merkle_root,
            vec![input_witness],
            vec![output_witness],
            vec![merkle_path],
        );

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(
            !cs.is_satisfied().unwrap(),
            "circuit should reject mismatched asset_id"
        );
    }

    #[test]
    fn test_transfer_circuit_spending_rights() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);

        // Derive IVK from spending key
        let derived_ivk = derive_ivk_from_spending_key(&sk);
        let expected_ivk = bytes_to_fr(&vk.incoming_view_key);
        assert_eq!(
            derived_ivk, expected_ivk,
            "IVK must match ViewingKey derivation"
        );

        // Different spending key -> different IVK
        let sk2 = test_spending_key(43);
        let derived_ivk2 = derive_ivk_from_spending_key(&sk2);
        assert_ne!(
            derived_ivk, derived_ivk2,
            "different spending keys must produce different IVKs"
        );

        // Wrong spending key should not match
        let wrong_ivk = derive_ivk_from_spending_key(&test_spending_key(99));
        assert_ne!(
            wrong_ivk, expected_ivk,
            "wrong spending key must not match IVK"
        );

        // Deterministic derivation
        let derived_ivk_again = derive_ivk_from_spending_key(&sk);
        assert_eq!(
            derived_ivk, derived_ivk_again,
            "IVK derivation must be deterministic"
        );
    }

    #[test]
    fn test_transfer_circuit_wrong_merkle_root_rejected() {
        let circuit = make_1in_1out_data(1000, 900, 1);

        // Rebuild with corrupted merkle root
        let mut bad_circuit = circuit.clone();
        bad_circuit.merkle_root = {
            let mut r = bad_circuit.merkle_root;
            r[0] ^= 0xFF;
            r
        };

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        bad_circuit.generate_constraints(cs.clone()).unwrap();
        assert!(
            !cs.is_satisfied().unwrap(),
            "circuit should reject wrong merkle root"
        );
    }

    #[test]
    fn test_transfer_circuit_wrong_spending_key_rejected() {
        let sk = test_spending_key(1);
        let rho_in = test_hash(10).0;
        let (input_witness, nullifier, input_cm) = make_input_note(1000, 1, &sk, rho_in);

        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&input_cm);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        let out_vk = ViewingKey::generate(&test_spending_key(2));
        let rho_out = test_hash(20).0;
        let (output_witness, output_cm) =
            make_output_note(900, 1, out_vk.incoming_view_key, rho_out);

        // Corrupt the spending key in the input witness
        let mut bad_input = input_witness.clone();
        bad_input.spending_key[0] ^= 0xFF;

        let bad_circuit = TransferCircuit::new(
            vec![nullifier],
            vec![output_cm],
            1,
            merkle_root,
            vec![bad_input],
            vec![output_witness],
            vec![merkle_path],
        );

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        bad_circuit.generate_constraints(cs.clone()).unwrap();
        assert!(
            !cs.is_satisfied().unwrap(),
            "circuit should reject wrong spending key"
        );
    }

    #[test]
    fn test_transfer_circuit_constraint_count_stable() {
        let circuit = make_1in_1out_data(1000, 900, 1);

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        let num_constraints = cs.num_constraints();
        // 1-in, 1-out transfer should have a stable, reasonable constraint count
        assert!(
            num_constraints > 0 && num_constraints < 15000,
            "transfer constraint count should be stable and reasonable, got {}",
            num_constraints
        );
    }
}
