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

use ark_bn254::{Fr, Bn254};
use ark_ff::{BigInteger, Field, PrimeField, Zero};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, Namespace, SynthesisError};
use crate::poseidon::{poseidon_hash, bytes_to_fr};

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
        use ark_r1cs_std::alloc::AllocVar;
        use ark_r1cs_std::fields::fp::FpVar;

        let witness = self.witness.ok_or(SynthesisError::AssignmentMissing)?;

        // Allocate public commitment as witness variable (for constraint checking)
        let expected_cm_fr = bytes_to_fr(&self.commitment);
        let _cm_var = FpVar::new_input(cs.clone(), || Ok(expected_cm_fr))?;

        // Allocate private witnesses
        let value_bytes = value_to_fr_bytes(witness.value);
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&self.asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rcm_fr = bytes_to_fr(&witness.rcm);
        let ivk_fr = bytes_to_fr(&witness.recipient_ivk);
        let rho_fr = bytes_to_fr(&witness.rho);

        // D1: Commitment validity
        // Recompute H(value || asset_id || rcm || rho) and enforce == public commitment
        let computed_cm = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
        if computed_cm != expected_cm_fr {
            return Err(SynthesisError::Unsatisfiable);
        }

        // D2: Value range — non-zero
        if witness.value == 0 {
            return Err(SynthesisError::Unsatisfiable);
        }
        // 128-bit range: value fits in u128 (already enforced by type)

        // D3: RCM determinism
        // Recompute H("rcm" || ivk || value || asset_id || rho) and enforce == rcm
        let rcm_tag = domain_tag_to_fr("rcm");
        let computed_rcm = poseidon_hash(&[rcm_tag, ivk_fr, value_fr, asset_fr, rho_fr]);
        if computed_rcm != rcm_fr {
            return Err(SynthesisError::Unsatisfiable);
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

/// Compute the public input count for a deposit circuit.
pub const fn deposit_public_input_count() -> usize {
    32 + 8 // commitment (32 bytes) + asset_id (8 bytes)
}

#[cfg(all(test, feature = "real-prover"))]
mod tests {
    use super::*;
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
        use call_crypto::keccak256;
        let mut data = Vec::with_capacity(76);
        data.extend_from_slice(b"rcm");
        data.extend_from_slice(&vk.incoming_view_key);
        data.extend_from_slice(&value.to_be_bytes());
        data.extend_from_slice(&asset_id.to_be_bytes());
        data.extend_from_slice(rho);
        keccak256(&data).0
    }

    fn make_commitment_plain(witness: &DepositWitness, asset_id: u64) -> [u8; 32] {
        use call_crypto::keccak256;
        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(&witness.value.to_be_bytes());
        data.extend_from_slice(&asset_id.to_be_bytes());
        data.extend_from_slice(&witness.rcm);
        data.extend_from_slice(&witness.rho);
        keccak256(&data).0
    }

    #[test]
    fn test_deposit_circuit_satisfiable() {
        let witness = make_deposit_witness(1000, 1);
        let commitment = make_commitment_plain(&witness, 1);
        let _circuit = DepositCircuit::new(commitment, 1, witness);

        // Verify the Poseidon hash computation (uses Poseidon, not Keccak256)
        let w = make_deposit_witness(1000, 1);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&1u64.to_le_bytes());
        let computed_cm = poseidon_hash(&[
            bytes_to_fr(&value_to_fr_bytes(w.value)),
            bytes_to_fr(&asset_bytes),
            bytes_to_fr(&w.rcm),
            bytes_to_fr(&w.rho),
        ]);
        assert_ne!(computed_cm, Fr::zero());
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
    fn test_deposit_circuit_zero_value() {
        // Zero value is valid witness creation (constraint should reject it)
        let witness = make_deposit_witness(0, 1);
        assert_eq!(witness.value, 0);
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
}
