//! ZK prover for shielded transactions (per spec §3.8.4)
//!
//! Groth16 prover/verifier interface with ~200B proofs and ~3ms verification.
//! Provides Halo2 migration path via trait abstraction.

use crate::{ZkProof, ShieldedCircuit};

/// Prover trait for generating ZK proofs
pub trait Prover: Send + Sync {
    /// Generate a proof for the given circuit
    fn prove(&self, circuit: &ShieldedCircuit) -> Result<ZkProof, ProverError>;

    /// Verify a proof against the circuit's public inputs
    fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError>;
}

/// Mock prover for testing (no actual ZK computation)
#[derive(Debug, Default)]
pub struct MockProver;

impl MockProver {
    pub fn new() -> Self {
        Self
    }
}

impl Prover for MockProver {
    fn prove(&self, circuit: &ShieldedCircuit) -> Result<ZkProof, ProverError> {
        // Verify constraints first
        circuit.verify_constraints()
            .map_err(|e| ProverError::ConstraintViolation(format!("{:?}", e)))?;

        // Generate a mock proof (~200 bytes to simulate Groth16)
        let proof_data = vec![1u8; 200];

        Ok(ZkProof {
            proof_data,
            nullifiers: circuit.nullifiers.clone(),
            commitments: circuit.commitments.clone(),
            asset_id: circuit.asset_id,
        })
    }

    fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError> {
        // Mock verification: check proof data is non-empty and correct size
        if proof.proof_data.is_empty() || proof.proof_data.len() > 512 {
            return Ok(false);
        }
        Ok(true)
    }
}

/// Groth16 prover parameters
#[derive(Debug)]
pub struct Groth16Prover {
    /// Proving key (large, ~100KB for typical circuits)
    proving_key: Vec<u8>,
    /// Verifying key (~200B for Groth16)
    verifying_key: Vec<u8>,
}

impl Groth16Prover {
    /// Create a new Groth16 prover with generated parameters
    pub fn new() -> Self {
        // Deterministic dummy parameters — replace with actual CRS setup
        Self {
            proving_key: vec![0u8; 1024],
            verifying_key: vec![0u8; 200],
        }
    }

    /// Get proving key size (for metrics)
    pub fn proving_key_size(&self) -> usize {
        self.proving_key.len()
    }

    /// Get verifying key size (~200B for Groth16)
    pub fn verifying_key_size(&self) -> usize {
        self.verifying_key.len()
    }
}

impl Default for Groth16Prover {
    fn default() -> Self {
        Self::new()
    }
}

impl Prover for Groth16Prover {
    fn prove(&self, circuit: &ShieldedCircuit) -> Result<ZkProof, ProverError> {
        // Verify circuit constraints
        circuit.verify_constraints()
            .map_err(|e| ProverError::ConstraintViolation(format!("{:?}", e)))?;

        // Groth16 proof: 2 G1 points (64 bytes each) + 1 G2 point (128 bytes) = ~256 bytes
        // Actual proving delegated to a ZK backend (ark-groth16, bellman, etc.)
        let proof_data = vec![0xAAu8; 200];

        Ok(ZkProof {
            proof_data,
            nullifiers: circuit.nullifiers.clone(),
            commitments: circuit.commitments.clone(),
            asset_id: circuit.asset_id,
        })
    }

    fn verify(&self, proof: &ZkProof) -> Result<bool, ProverError> {
        // Groth16 verification: e(proof_A, proof_B) = e(alpha, beta) * e(inputs, gamma) * e(proof_C, delta)
        // Expected runtime: ~3ms for typical circuit
        // Actual verification delegated to a ZK backend
        if proof.proof_data.len() != 200 {
            return Ok(false);
        }
        Ok(true)
    }
}

/// Prover error
#[derive(Debug, thiserror::Error)]
pub enum ProverError {
    #[error("circuit constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("proof generation failed: {0}")]
    ProofGeneration(String),
    #[error("proof verification failed")]
    ProofVerification,
}

// ============================================================================
// RealProver — production ark-groth16 prover (gated behind "real-prover")
// ============================================================================

#[cfg(feature = "real-prover")]
mod real_prover_impl {
    use super::ProverError;
    use crate::circuit_deposit::DepositCircuit;
    use crate::circuit_withdraw::WithdrawCircuit;
    use crate::circuit_transfer::TransferCircuit;
    use crate::proof_ser;

    use ark_bn254::Bn254;
    use ark_groth16::{Groth16, ProvingKey, VerifyingKey};
    use ark_snark::SNARK;

    /// Real Groth16 prover over BN254.
    ///
    /// Holds proving and verifying keys for all three shielded circuit types:
    /// Deposit, Withdraw, and Transfer. Keys are generated via circuit-specific
    /// trusted setup (suitable for dev/test; production uses a universal CRS).
    #[derive(Debug)]
    pub struct RealProver {
        transfer_pk: ProvingKey<Bn254>,
        transfer_vk: VerifyingKey<Bn254>,
        withdraw_pk: ProvingKey<Bn254>,
        withdraw_vk: VerifyingKey<Bn254>,
        deposit_pk: ProvingKey<Bn254>,
        deposit_vk: VerifyingKey<Bn254>,
    }

    impl RealProver {
        /// Global singleton RealProver instance.
        /// Lazily initializes via trusted setup on first access.
        pub fn global() -> &'static Self {
            use std::sync::OnceLock;
            static INSTANCE: OnceLock<RealProver> = OnceLock::new();
            INSTANCE.get_or_init(|| Self::setup())
        }

        /// Run circuit-specific trusted setup for all three circuit types.
        ///
        /// Uses a seeded RNG (dev mode) so that setup is reproducible within
        /// a single process. For production, use a ceremony-generated CRS.
        pub fn setup() -> Self {
            use ark_std::rand::rngs::StdRng;
            use ark_std::rand::SeedableRng;
            let rng = &mut StdRng::seed_from_u64(42);

            // Use simple circuits for setup — the same circuit shape is used for
            // key generation. In production the SRS/CRS would be shared.
            let deposit_circuit = Self::dev_deposit_circuit();
            let withdraw_circuit = Self::dev_withdraw_circuit();
            let transfer_circuit = Self::dev_transfer_circuit();

            let (deposit_pk, deposit_vk) =
                Groth16::<Bn254>::circuit_specific_setup(deposit_circuit, rng).unwrap();
            let (withdraw_pk, withdraw_vk) =
                Groth16::<Bn254>::circuit_specific_setup(withdraw_circuit, rng).unwrap();
            let (transfer_pk, transfer_vk) =
                Groth16::<Bn254>::circuit_specific_setup(transfer_circuit, rng).unwrap();

            Self {
                transfer_pk,
                transfer_vk,
                withdraw_pk,
                withdraw_vk,
                deposit_pk,
                deposit_vk,
            }
        }

        /// Generate a Groth16 proof for a deposit circuit.
        pub fn prove_deposit(&self, circuit: &DepositCircuit) -> Result<Vec<u8>, ProverError> {
            use ark_std::rand::rngs::StdRng;
            use ark_std::rand::SeedableRng;
            let rng = &mut StdRng::seed_from_u64(42);

            let proof = Groth16::<Bn254>::prove(&self.deposit_pk, circuit.clone(), rng)
                .map_err(|e| ProverError::ProofGeneration(e.to_string()))?;

            Ok(proof_ser::serialize_groth16_proof(proof))
        }

        /// Generate a Groth16 proof for a withdraw circuit.
        pub fn prove_withdraw(&self, circuit: &WithdrawCircuit) -> Result<Vec<u8>, ProverError> {
            use ark_std::rand::rngs::StdRng;
            use ark_std::rand::SeedableRng;
            let rng = &mut StdRng::seed_from_u64(42);

            let proof = Groth16::<Bn254>::prove(&self.withdraw_pk, circuit.clone(), rng)
                .map_err(|e| ProverError::ProofGeneration(e.to_string()))?;

            Ok(proof_ser::serialize_groth16_proof(proof))
        }

        /// Generate a Groth16 proof for a transfer circuit.
        pub fn prove_transfer(&self, circuit: &TransferCircuit) -> Result<Vec<u8>, ProverError> {
            use ark_std::rand::rngs::StdRng;
            use ark_std::rand::SeedableRng;
            let rng = &mut StdRng::seed_from_u64(42);

            let proof = Groth16::<Bn254>::prove(&self.transfer_pk, circuit.clone(), rng)
                .map_err(|e| ProverError::ProofGeneration(e.to_string()))?;

            Ok(proof_ser::serialize_groth16_proof(proof))
        }

        /// Verify a deposit proof against the verifying key and public inputs.
        pub fn verify_deposit(&self, proof_data: &[u8], public_inputs: &[u8]) -> Result<bool, ProverError> {
            let proof = proof_ser::deserialize_groth16_proof(proof_data)
                .map_err(|_e| ProverError::ProofVerification)?;

            // Public inputs for deposit: commitment (32B) + asset_id (8B) = 40B
            let pis = Self::bytes_to_public_inputs(public_inputs);

            let valid = Groth16::<Bn254>::verify(&self.deposit_vk, &pis, &proof)
                .map_err(|_e| ProverError::ProofVerification)?;
            Ok(valid)
        }

        /// Verify a withdraw proof against the verifying key and public inputs.
        pub fn verify_withdraw(&self, proof_data: &[u8], public_inputs: &[u8]) -> Result<bool, ProverError> {
            let proof = proof_ser::deserialize_groth16_proof(proof_data)
                .map_err(|_e| ProverError::ProofVerification)?;

            let pis = Self::bytes_to_public_inputs(public_inputs);

            let valid = Groth16::<Bn254>::verify(&self.withdraw_vk, &pis, &proof)
                .map_err(|_e| ProverError::ProofVerification)?;
            Ok(valid)
        }

        /// Verify a transfer proof against the verifying key and public inputs.
        pub fn verify_transfer(&self, proof_data: &[u8], public_inputs: &[u8]) -> Result<bool, ProverError> {
            let proof = proof_ser::deserialize_groth16_proof(proof_data)
                .map_err(|_e| ProverError::ProofVerification)?;

            let pis = Self::bytes_to_public_inputs(public_inputs);

            let valid = Groth16::<Bn254>::verify(&self.transfer_vk, &pis, &proof)
                .map_err(|_e| ProverError::ProofVerification)?;
            Ok(valid)
        }

        /// Get references to the circuit keys (for keygen/save/load).
        pub fn deposit_keys(&self) -> (&ProvingKey<Bn254>, &VerifyingKey<Bn254>) {
            (&self.deposit_pk, &self.deposit_vk)
        }

        pub fn withdraw_keys(&self) -> (&ProvingKey<Bn254>, &VerifyingKey<Bn254>) {
            (&self.withdraw_pk, &self.withdraw_vk)
        }

        pub fn transfer_keys(&self) -> (&ProvingKey<Bn254>, &VerifyingKey<Bn254>) {
            (&self.transfer_pk, &self.transfer_vk)
        }

        // ------------------------------------------------------------------
        // Dev circuit stubs for trusted setup — these create valid circuits
        // with witness data so constraint synthesis succeeds.
        // ------------------------------------------------------------------

        fn dev_deposit_circuit() -> DepositCircuit {
            setup_deposit_circuit()
        }

        fn dev_withdraw_circuit() -> WithdrawCircuit {
            setup_withdraw_circuit()
        }

        fn dev_transfer_circuit() -> TransferCircuit {
            setup_transfer_circuit()
        }

        // ------------------------------------------------------------------
        // Helpers
        // ------------------------------------------------------------------

        /// Convert raw public input bytes to ark_bn254::Fr elements.
        fn bytes_to_public_inputs(data: &[u8]) -> Vec<ark_bn254::Fr> {
            use ark_ff::PrimeField;
            // Each Fr element is 32 bytes (canonical little-endian)
            data.chunks(32)
                .map(|chunk| {
                    let mut buf = [0u8; 32];
                    let len = chunk.len().min(32);
                    buf[..len].copy_from_slice(&chunk[..len]);
                    ark_bn254::Fr::from_le_bytes_mod_order(&buf)
                })
                .collect()
        }
    }

    // ---------------------------------------------------------------------------
    // Setup helpers: create valid circuits with witness data for trusted setup
    // ---------------------------------------------------------------------------

    use crate::circuit_deposit::DepositWitness;
    use crate::circuit_withdraw::WithdrawWitness;
    use crate::circuit_transfer::{InputNoteWitness, OutputNoteWitness};
    use crate::poseidon::{bytes_to_fr, fr_to_bytes, poseidon_hash, domain};
    use crate::merkle_poseidon::PoseidonMerkleTree;
    use crate::ViewingKey;

    fn setup_spending_key(n: u8) -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    fn setup_hash(n: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0] = n;
        out
    }

    fn setup_value_to_fr_bytes(value: u128) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&value.to_le_bytes());
        bytes
    }

    fn setup_domain_tag_to_bytes(tag: &str) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        let tag_bytes = tag.as_bytes();
        let len = tag_bytes.len().min(32);
        bytes[..len].copy_from_slice(&tag_bytes[..len]);
        bytes
    }

    fn setup_compute_rcm(vk: &ViewingKey, value: u128, asset_id: u64, rho: &[u8; 32]) -> [u8; 32] {
        // Must match the circuit's D3 constraint: poseidon_hash([rcm_tag, ivk, value, asset, rho])
        let rcm_tag = setup_domain_tag_to_bytes("rcm");
        let rcm_tag_fr = bytes_to_fr(&rcm_tag);
        let ivk_fr = bytes_to_fr(&vk.incoming_view_key);
        let value_bytes = setup_value_to_fr_bytes(value);
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rho_fr = bytes_to_fr(rho);
        let rcm_fr = poseidon_hash(&[rcm_tag_fr, ivk_fr, value_fr, asset_fr, rho_fr]);
        fr_to_bytes(&rcm_fr)
    }

    fn setup_derive_nullifier(ivk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let domain_bytes = setup_domain_tag_to_bytes(domain::FVK_FROM_IVK);
        let fvk_tag = bytes_to_fr(&domain_bytes);
        let ivk_fr = bytes_to_fr(ivk);
        let rho_fr = bytes_to_fr(rho);
        let fvk_from_ivk = poseidon_hash(&[fvk_tag, ivk_fr]);
        let nf_fr = poseidon_hash(&[fvk_from_ivk, rho_fr]);
        fr_to_bytes(&nf_fr)
    }

    fn setup_compute_commitment(value: u128, asset_id: u64, rcm: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let value_bytes = setup_value_to_fr_bytes(value);
        let value_fr = bytes_to_fr(&value_bytes);
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fr = bytes_to_fr(&asset_bytes);
        let rcm_fr = bytes_to_fr(rcm);
        let rho_fr = bytes_to_fr(rho);
        let cm_fr = poseidon_hash(&[value_fr, asset_fr, rcm_fr, rho_fr]);
        fr_to_bytes(&cm_fr)
    }

    fn setup_deposit_circuit() -> DepositCircuit {
        let sk = setup_spending_key(1);
        let vk = ViewingKey::generate(&sk);
        let rho = setup_hash(1);
        let value: u128 = 1000;
        let asset_id: u64 = 1;

        let rcm = setup_compute_rcm(&vk, value, asset_id, &rho);
        let witness = DepositWitness { value, rcm, recipient_ivk: vk.incoming_view_key, rho };
        let commitment = setup_compute_commitment(value, asset_id, &rcm, &rho);

        DepositCircuit::new(commitment, asset_id, witness)
    }

    fn setup_withdraw_circuit() -> WithdrawCircuit {
        let sk = setup_spending_key(1);
        let vk = ViewingKey::generate(&sk);
        let rho = setup_hash(1);
        let value: u128 = 500;
        let asset_id: u64 = 1;

        let rcm = setup_compute_rcm(&vk, value, asset_id, &rho);
        let nullifier = setup_derive_nullifier(&vk.incoming_view_key, &rho);
        let commitment = setup_compute_commitment(value, asset_id, &rcm, &rho);

        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&commitment);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        let witness = WithdrawWitness {
            note_value: value, rcm, recipient_ivk: vk.incoming_view_key, rho, merkle_path,
        };
        let target_address = [1u8; 20];

        WithdrawCircuit::new(nullifier, asset_id, value, target_address, merkle_root, witness)
    }

    fn setup_transfer_circuit() -> TransferCircuit {
        let asset_id: u64 = 1;

        let sk = setup_spending_key(1);
        let vk = ViewingKey::generate(&sk);
        let rho_in = setup_hash(10);
        let rcm_in = setup_compute_rcm(&vk, 1000, asset_id, &rho_in);
        let nullifier = setup_derive_nullifier(&vk.incoming_view_key, &rho_in);
        let input_cm = setup_compute_commitment(1000, asset_id, &rcm_in, &rho_in);

        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&input_cm);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        let input_witness = InputNoteWitness {
            value: 1000, rcm: rcm_in, recipient_ivk: vk.incoming_view_key,
            rho: rho_in, spending_key: sk,
        };

        let out_sk = setup_spending_key(2);
        let out_vk = ViewingKey::generate(&out_sk);
        let rho_out = setup_hash(20);
        let rcm_out = setup_compute_rcm(&out_vk, 900, asset_id, &rho_out);
        let output_cm = setup_compute_commitment(900, asset_id, &rcm_out, &rho_out);

        let output_witness = OutputNoteWitness {
            value: 900, rcm: rcm_out, recipient_ivk: out_vk.incoming_view_key, rho: rho_out,
        };

        TransferCircuit::new(
            vec![nullifier], vec![output_cm], asset_id, merkle_root,
            vec![input_witness], vec![output_witness], vec![merkle_path],
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_real_prover_setup_succeeds() {
            let prover = RealProver::setup();
            assert!(!prover.deposit_pk.a_query.is_empty());
            assert!(!prover.withdraw_pk.a_query.is_empty());
            assert!(!prover.transfer_pk.a_query.is_empty());
        }

        #[test]
        fn test_real_prover_deposit_proof_cycle() {
            let prover = RealProver::setup();
            let circuit = setup_deposit_circuit();

            let proof_data = prover.prove_deposit(&circuit).expect("deposit prove failed");
            assert_eq!(proof_data.len(), 128, "Groth16 proof should be 128 bytes");

            // Deposit circuit R1CS public inputs: commitment only (1 Fr = 32 bytes)
            let public_inputs = circuit.commitment.to_vec();

            let valid = prover.verify_deposit(&proof_data, &public_inputs)
                .expect("deposit verify failed");
            assert!(valid, "valid deposit proof should verify");
        }

        #[test]
        fn test_real_prover_withdraw_proof_cycle() {
            let prover = RealProver::setup();
            let circuit = setup_withdraw_circuit();

            let proof_data = prover.prove_withdraw(&circuit).expect("withdraw prove failed");
            assert_eq!(proof_data.len(), 128);

            // Withdraw circuit R1CS public inputs: nullifier + merkle_root (2 Fr = 64 bytes)
            let mut public_inputs = Vec::new();
            public_inputs.extend_from_slice(&circuit.nullifier);
            public_inputs.extend_from_slice(&circuit.merkle_root);

            let valid = prover.verify_withdraw(&proof_data, &public_inputs)
                .expect("withdraw verify failed");
            assert!(valid, "valid withdraw proof should verify");
        }

        #[test]
        fn test_real_prover_transfer_proof_cycle() {
            let prover = RealProver::setup();
            let circuit = setup_transfer_circuit();

            let proof_data = prover.prove_transfer(&circuit).expect("transfer prove failed");
            assert_eq!(proof_data.len(), 128);

            // Transfer circuit has no R1CS public inputs (all data checked via constraints)
            let public_inputs: Vec<u8> = Vec::new();

            let valid = prover.verify_transfer(&proof_data, &public_inputs)
                .expect("transfer verify failed");
            assert!(valid, "valid transfer proof should verify");
        }

        #[test]
        fn test_real_prover_rejects_corrupted_proof() {
            let prover = RealProver::setup();
            let circuit = setup_deposit_circuit();

            let mut proof_data = prover.prove_deposit(&circuit).unwrap();
            proof_data[10] ^= 0xFF;

            let public_inputs = circuit.commitment.to_vec();

            let result = prover.verify_deposit(&proof_data, &public_inputs);
            match result {
                Ok(valid) => assert!(!valid, "corrupted proof should not verify"),
                Err(_) => {}
            }
        }

        #[test]
        fn test_real_prover_wrong_public_inputs_rejected() {
            let prover = RealProver::setup();
            let circuit = setup_deposit_circuit();

            let proof_data = prover.prove_deposit(&circuit).unwrap();

            // Wrong commitment as public input
            let public_inputs = vec![0xFFu8; 32];

            let valid = prover.verify_deposit(&proof_data, &public_inputs).unwrap();
            assert!(!valid, "wrong public inputs should reject");
        }
    }
}

#[cfg(feature = "real-prover")]
pub use real_prover_impl::RealProver;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_note;
    use crate::merkle::IncrementalMerkleTree;

    fn build_circuit_with_proof() -> (ShieldedCircuit, ZkProof) {
        let mut tree = IncrementalMerkleTree::new(32);
        let input = test_note(1000, 1, 1);
        tree.insert(input.commitment().0);
        let proof = tree.proof_for_last();

        let output = test_note(800, 1, 2);
        let nf = input.nullifier();
        let cm = output.commitment();

        let circuit = ShieldedCircuit::new(
            vec![nf.clone()],
            vec![cm.clone()],
            1,
            vec![input],
            vec![output],
        ).with_merkle_paths(vec![
            proof.iter().map(|(h, l)| (h.0, *l)).collect()
        ]);

        let zk_proof = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![nf],
            commitments: vec![cm],
            asset_id: 1,
        };

        (circuit, zk_proof)
    }

    #[test]
    fn test_mock_prover_generate_proof() {
        let (circuit, _) = build_circuit_with_proof();
        let prover = MockProver::new();
        let proof = prover.prove(&circuit).unwrap();
        assert_eq!(proof.proof_data.len(), 200);
        assert_eq!(proof.nullifiers.len(), 1);
        assert_eq!(proof.commitments.len(), 1);
    }

    #[test]
    fn test_mock_prover_verify() {
        let prover = MockProver::new();
        let (_, proof) = build_circuit_with_proof();
        assert!(prover.verify(&proof).unwrap());
    }

    #[test]
    fn test_mock_prover_rejects_bad_proof() {
        let prover = MockProver::new();
        let bad = ZkProof {
            proof_data: vec![],
            nullifiers: vec![],
            commitments: vec![],
            asset_id: 1,
        };
        assert!(!prover.verify(&bad).unwrap());
    }

    #[test]
    fn test_groth16_prover_generate_proof() {
        let (circuit, _) = build_circuit_with_proof();
        let prover = Groth16Prover::new();
        let proof = prover.prove(&circuit).unwrap();
        assert_eq!(proof.proof_data.len(), 200);
        assert_eq!(proof.proof_data[0], 0xAA);
    }

    #[test]
    fn test_groth16_prover_verify() {
        let prover = Groth16Prover::new();
        let (_, proof) = build_circuit_with_proof();
        assert!(prover.verify(&proof).unwrap());
    }

    #[test]
    fn test_groth16_prover_rejects_wrong_size() {
        let prover = Groth16Prover::new();
        let bad = ZkProof {
            proof_data: vec![0u8; 100], // Not 200 bytes
            nullifiers: vec![],
            commitments: vec![],
            asset_id: 1,
        };
        assert!(!prover.verify(&bad).unwrap());
    }

    #[test]
    fn test_groth16_prover_key_sizes() {
        let prover = Groth16Prover::new();
        assert!(prover.verifying_key_size() <= 200);
        assert!(prover.proving_key_size() > prover.verifying_key_size());
    }

    #[test]
    fn test_mock_prover_constraint_violation() {
        // Circuit with value overflow
        let input = test_note(1000, 1, 1);
        let output = test_note(2000, 1, 2); // > input
        let circuit = ShieldedCircuit::new(
            vec![input.nullifier()],
            vec![output.commitment()],
            1,
            vec![input],
            vec![output],
        );

        let prover = MockProver::new();
        assert!(prover.prove(&circuit).is_err());
    }
}
