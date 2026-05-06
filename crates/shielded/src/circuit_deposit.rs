//! ShieldedDeposit circuit — the simplest ZK circuit.
//!
//! Proves that a user deposited transparent funds into the shielded pool
//! by creating a valid note commitment, without revealing the note's
//! spending key or nullifier rho.
//!
//! Public inputs: commitment (32B), asset_id (u64)
//! Private witnesses: value (u128), rcm (32B), recipient_ivk (32B), rho (32B)
//!
//! Constraints:
//!   D1. Commitment validity: Recompute H(value || asset_id || rcm || rho) == public
//!   D2. Value range: Non-zero, 128-bit range
//!   D3. RCM determinism: H("rcm" || ivk || value || asset_id || rho) == rcm

use crate::poseidon::bytes_to_fr;
use ark_bn254::Fr;
use ark_ff::{Field, Zero};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

/// Witness data for a deposit note.
#[derive(Debug, Clone)]
pub struct DepositWitness {
    pub value: u128,
    pub rcm: [u8; 32],
    pub recipient_ivk: [u8; 32],
    pub rho: [u8; 32],
}

/// ShieldedDeposit circuit.
#[derive(Debug, Clone)]
pub struct DepositCircuit {
    /// Public inputs
    pub commitment: [u8; 32],
    pub asset_id: u64,
    /// Private witnesses
    pub witness: Option<DepositWitness>,
}

impl DepositCircuit {
    /// Create a new deposit circuit from public data and private witness.
    pub fn new(commitment: [u8; 32], asset_id: u64, witness: DepositWitness) -> Self {
        Self {
            commitment,
            asset_id,
            witness: Some(witness),
        }
    }

    /// Create a circuit with only public data (for verification only).
    pub fn for_verify(commitment: [u8; 32], asset_id: u64) -> Self {
        Self {
            commitment,
            asset_id,
            witness: None,
        }
    }
}

impl ConstraintSynthesizer<Fr> for DepositCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        use crate::poseidon::gadget::poseidon_hash_gadget;
        use ark_r1cs_std::alloc::AllocVar;
        use ark_r1cs_std::boolean::Boolean;
        use ark_r1cs_std::eq::EqGadget;
        use ark_r1cs_std::fields::fp::FpVar;
        use ark_r1cs_std::prelude::ToBitsGadget;

        let witness = self.witness.ok_or(SynthesisError::AssignmentMissing)?;

        // --- Public inputs ---
        let commitment_fr = bytes_to_fr(&self.commitment);
        let commitment_var = FpVar::new_input(cs.clone(), || Ok(commitment_fr))?;

        let asset_id_fr = {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&self.asset_id.to_le_bytes());
            bytes_to_fr(&b)
        };
        let asset_id_var = FpVar::new_input(cs.clone(), || Ok(asset_id_fr))?;

        // --- Private witnesses ---
        let value_fr = bytes_to_fr(&value_to_fr_bytes(witness.value));
        let value_var = FpVar::new_witness(cs.clone(), || Ok(value_fr))?;

        let rcm_fr = bytes_to_fr(&witness.rcm);
        let rcm_var = FpVar::new_witness(cs.clone(), || Ok(rcm_fr))?;

        let ivk_fr = bytes_to_fr(&witness.recipient_ivk);
        let ivk_var = FpVar::new_witness(cs.clone(), || Ok(ivk_fr))?;

        let rho_fr = bytes_to_fr(&witness.rho);
        let rho_var = FpVar::new_witness(cs.clone(), || Ok(rho_fr))?;

        // D1: Commitment validity
        // H(value || asset_id || rcm || rho) == public commitment
        let computed_cm = poseidon_hash_gadget(
            cs.clone(),
            &[
                value_var.clone(),
                asset_id_var.clone(),
                rcm_var.clone(),
                rho_var.clone(),
            ],
        )?;
        computed_cm.enforce_equal(&commitment_var)?;

        // D2: Value range — non-zero (inverse witness trick)
        let value_inv = FpVar::new_witness(cs.clone(), || {
            if value_fr.is_zero() {
                Err(SynthesisError::Unsatisfiable)
            } else {
                Ok(value_fr.inverse().unwrap())
            }
        })?;
        let one = FpVar::new_constant(cs.clone(), Fr::from(1u64))?;
        (value_var.clone() * value_inv).enforce_equal(&one)?;

        // 128-bit range: decompose to bits and enforce bits 128..=253 are zero
        let bits = value_var.to_bits_le()?;
        for bit in &bits[128..] {
            bit.enforce_equal(&Boolean::constant(false))?;
        }

        // D3: RCM determinism
        // H("rcm" || ivk || value || asset_id || rho) == rcm
        let rcm_tag_fr = domain_tag_to_fr("rcm");
        let rcm_tag_var = FpVar::new_constant(cs.clone(), rcm_tag_fr)?;
        let computed_rcm = poseidon_hash_gadget(
            cs.clone(),
            &[rcm_tag_var, ivk_var, value_var, asset_id_var, rho_var],
        )?;
        computed_rcm.enforce_equal(&rcm_var)?;

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

/// Compute the public input byte count for a deposit circuit.
pub const fn deposit_public_input_count() -> usize {
    32 + 8 // commitment (32 bytes) + asset_id (8 bytes)
}

#[cfg(all(test, feature = "real-prover"))]
mod tests {
    use super::*;
    use crate::poseidon::{bytes_to_fr, fr_to_bytes, poseidon_hash, poseidon_hash_tagged};
    use crate::test_utils::{test_hash, test_spending_key};
    use crate::ViewingKey;

    fn make_deposit_witness(value: u128, seed: u8) -> DepositWitness {
        let sk = test_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(seed).0;

        DepositWitness {
            value,
            rcm: compute_rcm_plain(&vk, value, 1, &rho),
            recipient_ivk: vk.incoming_view_key,
            rho,
        }
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

    fn make_commitment_plain(witness: &DepositWitness, asset_id: u64) -> [u8; 32] {
        let value_bytes = value_to_fr_bytes(witness.value);
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rcm_fr = bytes_to_fr(&witness.rcm);
        let rho_fr = bytes_to_fr(&witness.rho);
        let cm_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
        fr_to_bytes(&cm_fr)
    }

    #[test]
    fn test_deposit_circuit_satisfiable() {
        let witness = make_deposit_witness(1000, 1);
        let commitment = make_commitment_plain(&witness, 1);
        let circuit = DepositCircuit::new(commitment, 1, witness);

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_deposit_circuit_witness_complete() {
        let witness = make_deposit_witness(500, 42);
        assert_eq!(witness.value, 500);
        assert_eq!(witness.rcm.len(), 32);
        assert_eq!(witness.recipient_ivk.len(), 32);
        assert_eq!(witness.rho.len(), 32);
    }

    #[test]
    fn test_deposit_circuit_deterministic() {
        let w1 = make_deposit_witness(1000, 7);
        let w2 = make_deposit_witness(1000, 7);
        assert_eq!(w1.value, w2.value);
        assert_eq!(w1.rcm, w2.rcm);
        assert_eq!(w1.recipient_ivk, w2.recipient_ivk);
        assert_eq!(w1.rho, w2.rho);
    }

    #[test]
    fn test_deposit_circuit_different_values() {
        let w1 = make_deposit_witness(1000, 7);
        let w2 = make_deposit_witness(2000, 7);
        assert_ne!(w1.value, w2.value);
        assert_ne!(w1.rcm, w2.rcm);
    }

    #[test]
    fn test_deposit_circuit_zero_value_rejected() {
        let witness = make_deposit_witness(0, 1);
        let commitment = make_commitment_plain(&witness, 1);
        let circuit = DepositCircuit::new(commitment, 1, witness);

        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        let result = circuit.generate_constraints(cs.clone());
        assert!(result.is_err() || !cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_deposit_circuit_large_value() {
        let max_value = u128::MAX;
        let witness = DepositWitness {
            value: max_value,
            rcm: [1u8; 32],
            recipient_ivk: [2u8; 32],
            rho: [3u8; 32],
        };
        assert_eq!(witness.value, max_value);
    }

    #[test]
    fn test_deposit_public_input_count() {
        assert_eq!(deposit_public_input_count(), 40);
    }

    #[test]
    fn test_deposit_circuit_for_verify() {
        let circuit = DepositCircuit::for_verify([1u8; 32], 1);
        assert!(circuit.witness.is_none());
        assert_eq!(circuit.commitment.len(), 32);
        assert_eq!(circuit.asset_id, 1);
    }

    #[test]
    fn test_deposit_circuit_wrong_commitment_rejected() {
        let witness = make_deposit_witness(1000, 1);
        let mut bad_commitment = make_commitment_plain(&witness, 1);
        bad_commitment[0] ^= 0xFF;

        let circuit = DepositCircuit::new(bad_commitment, 1, witness);
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        let result = circuit.generate_constraints(cs.clone());
        assert!(result.is_err() || !cs.is_satisfied().unwrap());
    }
}
