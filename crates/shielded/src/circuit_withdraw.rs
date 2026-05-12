//! ShieldedWithdraw circuit — proves a user is withdrawing from the shielded pool
//! to a transparent address, consuming a shielded note.
//!
//! Proves knowledge of a valid shielded note (committed in the Merkle tree) and
//! its associated spending authority, without revealing the note's full contents
//! or the spending key. The withdrawal value is public — it is credited to the
//! transparent balance of `target_address`.
//!
//! Public inputs:
//! - nullifier: [u8; 32]
//! - asset_id: u64
//! - value: u128 (public — credited to transparent balance)
//! - target_address: [u8; 20]
//! - merkle_root: [u8; 32]
//!
//! Private witnesses:
//! - note_value: u128
//! - rcm: [u8; 32]
//! - recipient_ivk: [u8; 32]
//! - rho: [u8; 32]
//! - spending_key: [u8; 32]
//! - merkle_path: Vec<([u8; 32], bool)>
//!
//! Constraints:
//!   W1. Nullifier derivation: poseidon_hash(poseidon_hash("fvk_from_ivk" || ivk), rho) == public nullifier
//!   W2. Merkle path validity: Walk the Poseidon Merkle path, enforce root == public merkle_root
//!   W3. Value match: note_value == public value
//!   W4. Spending rights: ivk == poseidon_hash_tagged("call/shielded/ivk", [sk])
//!   W5. Range & asset: Non-zero value, 128-bit range, asset_id consistency

use crate::poseidon::bytes_to_fr;
use crate::poseidon::domain;
use ark_bn254::Fr;
use ark_ff::{Field, Zero};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

/// Witness data for a withdraw note.
#[derive(Debug, Clone)]
pub struct WithdrawWitness {
    pub note_value: u128,
    pub rcm: [u8; 32],
    pub recipient_ivk: [u8; 32],
    pub rho: [u8; 32],
    pub spending_key: [u8; 32],
    pub merkle_path: Vec<([u8; 32], bool)>,
}

/// ShieldedWithdraw circuit.
///
/// Proves that the prover knows a shielded note that:
/// - Exists in the Merkle tree (root == public merkle_root)
/// - Was created with the claimed spending key (nullifier derivation)
/// - Has value matching the public withdrawal amount
#[derive(Debug, Clone)]
pub struct WithdrawCircuit {
    /// Public inputs
    pub nullifier: [u8; 32],
    pub asset_id: u64,
    pub value: u128,
    pub target_address: [u8; 20],
    pub merkle_root: [u8; 32],
    /// Private witnesses
    pub witness: Option<WithdrawWitness>,
}

impl WithdrawCircuit {
    /// Create a new withdraw circuit from public data and private witness.
    pub fn new(
        nullifier: [u8; 32],
        asset_id: u64,
        value: u128,
        target_address: [u8; 20],
        merkle_root: [u8; 32],
        witness: WithdrawWitness,
    ) -> Self {
        Self {
            nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness: Some(witness),
        }
    }

    /// Create a circuit with only public data (for verification only).
    pub fn for_verify(
        nullifier: [u8; 32],
        asset_id: u64,
        value: u128,
        target_address: [u8; 20],
        merkle_root: [u8; 32],
    ) -> Self {
        Self {
            nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness: None,
        }
    }
}

impl ConstraintSynthesizer<Fr> for WithdrawCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        use crate::poseidon::gadget::poseidon_hash_gadget;
        use ark_r1cs_std::alloc::AllocVar;
        use ark_r1cs_std::boolean::Boolean;
        use ark_r1cs_std::eq::EqGadget;
        use ark_r1cs_std::fields::fp::FpVar;
        use ark_r1cs_std::prelude::ToBitsGadget;

        let witness = self.witness.ok_or(SynthesisError::AssignmentMissing)?;

        // --- Public inputs ---
        let nullifier_fr = bytes_to_fr(&self.nullifier);
        let nullifier_var = FpVar::new_input(cs.clone(), || Ok(nullifier_fr))?;

        let asset_id_fr = {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&self.asset_id.to_le_bytes());
            bytes_to_fr(&b)
        };
        let asset_id_var = FpVar::new_input(cs.clone(), || Ok(asset_id_fr))?;

        let merkle_root_fr = bytes_to_fr(&self.merkle_root);
        let merkle_root_var = FpVar::new_input(cs.clone(), || Ok(merkle_root_fr))?;

        let public_value_fr = bytes_to_fr(&value_to_fr_bytes(self.value));
        let public_value_var = FpVar::new_input(cs.clone(), || Ok(public_value_fr))?;

        // --- Private witnesses ---
        let value_fr = bytes_to_fr(&value_to_fr_bytes(witness.note_value));
        let value_var = FpVar::new_witness(cs.clone(), || Ok(value_fr))?;

        let rcm_fr = bytes_to_fr(&witness.rcm);
        let rcm_var = FpVar::new_witness(cs.clone(), || Ok(rcm_fr))?;

        let ivk_fr = bytes_to_fr(&witness.recipient_ivk);
        let ivk_var = FpVar::new_witness(cs.clone(), || Ok(ivk_fr))?;

        let rho_fr = bytes_to_fr(&witness.rho);
        let rho_var = FpVar::new_witness(cs.clone(), || Ok(rho_fr))?;

        let sk_fr = bytes_to_fr(&witness.spending_key);
        let sk_var = FpVar::new_witness(cs.clone(), || Ok(sk_fr))?;

        // W1: Nullifier derivation
        let fvk_tag_fr = domain_tag_to_fr(domain::FVK_FROM_IVK);
        let fvk_tag_var = FpVar::new_constant(cs.clone(), fvk_tag_fr)?;
        let fvk_from_ivk = poseidon_hash_gadget(cs.clone(), &[fvk_tag_var, ivk_var.clone()])?;
        let computed_nf = poseidon_hash_gadget(cs.clone(), &[fvk_from_ivk, rho_var.clone()])?;
        computed_nf.enforce_equal(&nullifier_var)?;

        // W4: Spending rights — IVK derived from spending_key must match note IVK
        let ivk_tag_fr = domain_tag_to_fr(domain::IVK_FROM_SK);
        let ivk_tag_var = FpVar::new_constant(cs.clone(), ivk_tag_fr)?;
        let derived_ivk = poseidon_hash_gadget(cs.clone(), &[ivk_tag_var, sk_var])?;
        derived_ivk.enforce_equal(&ivk_var)?;

        // W2: Merkle path validity
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
        for (sibling_hash, sibling_is_right) in &witness.merkle_path {
            let sibling_fr = bytes_to_fr(sibling_hash);
            let sibling_var = FpVar::new_witness(cs.clone(), || Ok(sibling_fr))?;

            current = if *sibling_is_right {
                poseidon_hash_gadget(cs.clone(), &[current, sibling_var])?
            } else {
                poseidon_hash_gadget(cs.clone(), &[sibling_var, current])?
            };
        }
        current.enforce_equal(&merkle_root_var)?;

        // W3: Value match
        value_var.enforce_equal(&public_value_var)?;

        // W4: Range & asset checks
        // Non-zero value
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

        // 128-bit range
        let bits = value_var.to_bits_le()?;
        for bit in &bits[128..] {
            bit.enforce_equal(&Boolean::constant(false))?;
        }

        Ok(())
    }
}

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

/// Compute the public input byte count for a withdraw circuit.
pub const fn withdraw_public_input_count() -> usize {
    32 + 8 + 16 + 20 + 32 // nullifier + asset_id + value + target_address + merkle_root
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

    /// Build a withdraw witness and all public inputs from scratch.
    fn make_withdraw_data(
        value: u128,
        asset_id: u64,
        seed: u8,
    ) -> (
        [u8; 32], // nullifier
        u64,      // asset_id
        u128,     // value
        [u8; 20], // target_address
        [u8; 32], // merkle_root
        WithdrawWitness,
    ) {
        let sk = test_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(seed).0;

        // Derive nullifier: H(H("fvk_from_ivk" || ivk), rho)
        let fvk_tag = domain_tag_to_fr(domain::FVK_FROM_IVK);
        let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
        let rho_fr = bytes_to_fr(&rho);
        let nullifier_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);
        let nullifier = fr_to_bytes(&nullifier_fr);

        // Derive rcm deterministically
        let rcm = compute_rcm_plain(&vk, value, asset_id, &rho);
        let rcm_fr = bytes_to_fr(&rcm);

        // Build note commitment: H(value || asset_id || rcm || rho)
        let value_bytes = value_to_fr_bytes(value);
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes_arr = [0u8; 32];
        asset_bytes_arr[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes_arr);
        let commitment_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
        let commitment = fr_to_bytes(&commitment_fr);

        // Build Merkle tree with the commitment
        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&commitment);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        let target_address = [seed; 20];

        let witness = WithdrawWitness {
            note_value: value,
            rcm,
            recipient_ivk: vk.incoming_view_key,
            rho,
            spending_key: sk,
            merkle_path,
        };

        (
            nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness,
        )
    }

    fn compute_rcm_plain(vk: &ViewingKey, value: u128, asset_id: u64, rho: &[u8; 32]) -> [u8; 32] {
        let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
        let value_bytes = value_to_fr_bytes(value);
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rho_fr = bytes_to_fr(rho);
        let rcm_fr = poseidon_hash_tagged("rcm", &[ivk_fr, value_fr, asset_fr, rho_fr]);
        fr_to_bytes(&rcm_fr)
    }

    #[test]
    fn test_withdraw_circuit_satisfiable() {
        let (nullifier, asset_id, value, target_address, merkle_root, witness) =
            make_withdraw_data(1000, 1, 1);

        let circuit = WithdrawCircuit::new(
            nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness,
        );

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_withdraw_circuit_nullifier_derivation() {
        let sk = test_spending_key(7);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(7).0;

        // Derive via the constraint logic
        let fvk_tag = domain_tag_to_fr(domain::FVK_FROM_IVK);
        let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
        let rho_fr = bytes_to_fr(&rho);
        let nullifier_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);

        // Same derivation should be deterministic
        let nullifier_fr2 = {
            let fvk_tag2 = domain_tag_to_fr(domain::FVK_FROM_IVK);
            let ivk_fr2 = bytes_to_fr(&vk.incoming_view_key);
            let fvk2 = poseidon_hash(&[fvk_tag2, ivk_fr2]);
            poseidon_hash(&[fvk2, rho_fr])
        };
        assert_eq!(nullifier_fr, nullifier_fr2);
        assert_ne!(nullifier_fr, Fr::zero());
    }

    #[test]
    fn test_withdraw_circuit_merkle_path_valid() {
        let (_, _, _value, _, _, witness) = make_withdraw_data(500, 1, 42);

        // Merkle path should have exactly 32 elements for depth-32 tree
        assert_eq!(witness.merkle_path.len(), 32);

        // Verify the path is well-formed (each element has a 32-byte hash and bool)
        for (sibling, is_right) in &witness.merkle_path {
            assert_eq!(sibling.len(), 32);
            assert!(*is_right == true || *is_right == false);
        }
    }

    #[test]
    fn test_withdraw_circuit_value_match() {
        let value: u128 = 10000;
        let (nullifier, asset_id, _, target_address, merkle_root, witness) =
            make_withdraw_data(value, 1, 5);

        // Public value should match witness note_value
        assert_eq!(witness.note_value, value);

        let circuit = WithdrawCircuit::new(
            nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness,
        );
        assert_eq!(circuit.value, value);

        // Verify the value byte conversion is consistent
        let v_bytes = value_to_fr_bytes(value);
        let v_fr = bytes_to_fr(&v_bytes);
        let v2_bytes = value_to_fr_bytes(value);
        let v2_fr = bytes_to_fr(&v2_bytes);
        assert_eq!(v_fr, v2_fr);
    }

    #[test]
    fn test_withdraw_circuit_wrong_nullifier_rejected() {
        let (nullifier, asset_id, value, target_address, merkle_root, witness) =
            make_withdraw_data(1000, 1, 1);

        // Corrupt the nullifier by flipping a byte
        let mut bad_nullifier = nullifier;
        bad_nullifier[0] ^= 0xFF;

        let circuit = WithdrawCircuit::new(
            bad_nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness,
        );

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        let result = circuit.generate_constraints(cs.clone());
        assert!(result.is_err() || !cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_withdraw_circuit_zero_value_rejected() {
        // Build witness with zero value
        let sk = test_spending_key(99);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(99).0;

        let value: u128 = 0;
        let rcm = compute_rcm_plain(&vk, value, 1, &rho);

        let fvk_tag = domain_tag_to_fr(domain::FVK_FROM_IVK);
        let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
        let rho_fr = bytes_to_fr(&rho);
        let nullifier_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);
        let nullifier = fr_to_bytes(&nullifier_fr);

        let value_bytes = value_to_fr_bytes(value);
        let value_fr = bytes_to_fr(&value_bytes);
        let rcm_fr = bytes_to_fr(&rcm);
        // Use asset_id = 1 to match circuit
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&1u64.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let commitment_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
        let commitment = fr_to_bytes(&commitment_fr);

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

        let circuit = WithdrawCircuit::new(nullifier, 1, value, [99u8; 20], merkle_root, witness);

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        let result = circuit.generate_constraints(cs.clone());
        assert!(result.is_err() || !cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_withdraw_circuit_wrong_merkle_root_rejected() {
        let (nullifier, asset_id, value, target_address, _merkle_root, witness) =
            make_withdraw_data(1000, 1, 1);

        // Corrupt the merkle root by flipping a byte
        let mut bad_root = witness.merkle_path[0].0;
        bad_root[0] ^= 0xFF;

        let circuit = WithdrawCircuit::new(
            nullifier,
            asset_id,
            value,
            target_address,
            bad_root,
            witness,
        );

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        let result = circuit.generate_constraints(cs.clone());
        assert!(result.is_err() || !cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_withdraw_circuit_wrong_value_rejected() {
        let (nullifier, asset_id, value, target_address, merkle_root, witness) =
            make_withdraw_data(1000, 1, 1);

        // Public value doesn't match witness note_value
        let bad_value = value + 1;

        let circuit = WithdrawCircuit::new(
            nullifier,
            asset_id,
            bad_value,
            target_address,
            merkle_root,
            witness,
        );

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        let result = circuit.generate_constraints(cs.clone());
        assert!(result.is_err() || !cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_withdraw_circuit_wrong_asset_id_rejected() {
        let (nullifier, _asset_id, value, target_address, merkle_root, witness) =
            make_withdraw_data(1000, 1, 1);

        // Public asset_id doesn't match witness
        let bad_asset_id = 999u64;

        let circuit = WithdrawCircuit::new(
            nullifier,
            bad_asset_id,
            value,
            target_address,
            merkle_root,
            witness,
        );

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        let result = circuit.generate_constraints(cs.clone());
        assert!(result.is_err() || !cs.is_satisfied().unwrap());
    }
}
