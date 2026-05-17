//! ZK prover for shielded transactions (per spec §3.8.4)
//!
//! Halo2 prover/verifier over Pasta Pallas with ~5-10KB proofs and ~5-10ms verification.
//! No trusted setup required — uses Params::new(k) + keygen_vk/keygen_pk.

use crate::{ShieldedCircuit, ZkProof};

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
        circuit
            .verify_constraints()
            .map_err(|e| ProverError::ConstraintViolation(format!("{:?}", e)))?;

        // Generate a mock proof (~200 bytes)
        let proof_data = vec![1u8; 200];

        Ok(ZkProof {
            proof_data,
            nullifiers: circuit.nullifiers.clone(),
            commitments: circuit.commitments.clone(),
            asset_id: circuit.asset_id,
            key_version: 0,
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
// Halo2Prover — production Halo2 prover/verifier (gated behind "halo2-prover")
// ============================================================================

#[cfg(feature = "halo2-prover")]
mod halo2_prover_impl {
    use super::ProverError;
    use crate::circuit_deposit::DepositCircuit;
    use crate::circuit_transfer::{InputNoteWitness, OutputNoteWitness, TransferCircuit};
    use crate::circuit_withdraw::WithdrawCircuit;
    use crate::poseidon::{bytes_to_fp, fp_to_bytes, poseidon_hash, poseidon_hash_tagged};
    use crate::merkle_poseidon::PoseidonMerkleTree;
    use crate::ViewingKey;

    use halo2_proofs::plonk::{
        create_proof, keygen_pk, keygen_vk, ProvingKey, SingleVerifier, VerifyingKey,
        verify_proof,
    };
    use halo2_proofs::poly::commitment::Params;
    use halo2_proofs::transcript::{Blake2bRead, Blake2bWrite, Challenge255};
    use pasta_curves::EqAffine;
    use pasta_curves::Fp;

    /// Production Halo2 prover/verifier over Pasta Pallas.
    ///
    /// Holds universal parameters (Params) and circuit-specific keys (PK/VK)
    /// for all three shielded circuit types. No trusted setup required.
    #[derive(Debug, Clone)]
    pub struct Halo2Prover {
        deposit_params: Params<EqAffine>,
        deposit_pk: ProvingKey<EqAffine>,
        deposit_vk: VerifyingKey<EqAffine>,

        withdraw_params: Params<EqAffine>,
        withdraw_pk: ProvingKey<EqAffine>,
        withdraw_vk: VerifyingKey<EqAffine>,

        transfer_params: Params<EqAffine>,
        transfer_pk: ProvingKey<EqAffine>,
        transfer_vk: VerifyingKey<EqAffine>,
    }

    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// Global registry of Halo2Prover instances by key version.
    static VERSION_REGISTRY: OnceLock<Mutex<HashMap<u32, Halo2Prover>>> = OnceLock::new();

    fn version_registry() -> &'static Mutex<HashMap<u32, Halo2Prover>> {
        VERSION_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
    }

    impl Halo2Prover {
        /// Global singleton Halo2Prover instance (version 0).
        pub fn global() -> &'static Self {
            use std::sync::OnceLock;
            static INSTANCE: OnceLock<Halo2Prover> = OnceLock::new();
            INSTANCE.get_or_init(|| Self::setup())
        }

        /// Register a Halo2Prover for a specific key version.
        ///
        /// Called by governance during `ProverKeyRotation` proposal execution.
        /// The version is bumped by governance; old versions remain accessible
        /// until explicitly sunset.
        pub fn register_version(version: u32, prover: Halo2Prover) {
            let mut registry = version_registry().lock().expect("version registry poisoned");
            registry.insert(version, prover);
        }

        /// Create a Halo2Prover for a specific key version.
        ///
        /// First checks the version registry (governance-registered keys),
        /// then falls back to the genesis singleton (version 0).
        pub fn for_version(version: u32) -> Option<Self> {
            if version == 0 {
                return Some(Self::global().clone());
            }
            let registry = version_registry().lock().expect("version registry poisoned");
            registry.get(&version).cloned()
        }

        /// Default key directory for persistent Halo2 parameters.
        pub fn default_key_dir() -> std::path::PathBuf {
            let mut path = std::env::temp_dir();
            path.push("call_halo2_keys");
            path
        }

        /// Load `Params` from disk or generate and save them.
        fn load_or_create_params(k: u32, dir: &std::path::Path, name: &str) -> Params<EqAffine> {
            use std::io::{Read, Write};
            let path = dir.join(format!("{}_params_k{}.bin", name, k));
            if let Ok(mut file) = std::fs::File::open(&path) {
                let mut buf = Vec::new();
                if file.read_to_end(&mut buf).is_ok() {
                    if let Ok(params) = Params::read(&mut buf.as_slice()) {
                        if params.k() == k {
                            return params;
                        }
                    }
                }
            }
            let params = Params::new(k);
            let _ = std::fs::create_dir_all(dir);
            if let Ok(mut file) = std::fs::File::create(&path) {
                let mut buf = Vec::new();
                let _ = params.write(&mut buf);
                let _ = file.write_all(&buf);
            }
            params
        }

        /// Generate universal parameters and circuit-specific keys for all three circuits.
        ///
        /// Uses `Params::new(k)` where k is the circuit's row count exponent:
        /// - Deposit: k=10 (1024 rows)
        /// - Withdraw: k=12 (4096 rows, 32-level Merkle path)
        /// - Transfer: k=12 (4096 rows, 2-in/2-out)
        ///
        /// Parameters are persisted to disk so subsequent calls (and node restarts)
        /// avoid the expensive multi-exponentiation setup.
        pub fn setup() -> Self {
            Self::setup_from_dir(&Self::default_key_dir())
        }

        /// Setup from a specific key directory.
        pub fn setup_from_dir(dir: &std::path::Path) -> Self {
            // Deposit circuit (k=10)
            let deposit_params = Self::load_or_create_params(10, dir, "deposit");
            let deposit_circuit = setup_deposit_circuit();
            let deposit_vk = keygen_vk(&deposit_params, &deposit_circuit)
                .expect("deposit keygen_vk");
            let deposit_pk = keygen_pk(&deposit_params, deposit_vk.clone(), &deposit_circuit)
                .expect("deposit keygen_pk");

            // Withdraw circuit (k=12)
            let withdraw_params = Self::load_or_create_params(12, dir, "withdraw");
            let withdraw_circuit = setup_withdraw_circuit();
            let withdraw_vk = keygen_vk(&withdraw_params, &withdraw_circuit)
                .expect("withdraw keygen_vk");
            let withdraw_pk = keygen_pk(&withdraw_params, withdraw_vk.clone(), &withdraw_circuit)
                .expect("withdraw keygen_pk");

            // Transfer circuit (k=12)
            let transfer_params = Self::load_or_create_params(12, dir, "transfer");
            let transfer_circuit = setup_transfer_circuit();
            let transfer_vk = keygen_vk(&transfer_params, &transfer_circuit)
                .expect("transfer keygen_vk");
            let transfer_pk = keygen_pk(&transfer_params, transfer_vk.clone(), &transfer_circuit)
                .expect("transfer keygen_pk");

            Self {
                deposit_params,
                deposit_pk,
                deposit_vk,
                withdraw_params,
                withdraw_pk,
                withdraw_vk,
                transfer_params,
                transfer_pk,
                transfer_vk,
            }
        }

        // ------------------------------------------------------------------
        // Proof generation
        // ------------------------------------------------------------------

        /// Generate a Halo2 proof for a deposit circuit.
        pub fn prove_deposit(&self, circuit: &DepositCircuit) -> Result<Vec<u8>, ProverError> {
            let commitment_fp = bytes_to_fp(&circuit.commitment);
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&circuit.asset_id.to_le_bytes());
            let asset_id_fp = bytes_to_fp(&asset_bytes);

            let instances: &[&[&[Fp]]] = &[&[&[commitment_fp, asset_id_fp]]];

            let mut transcript =
                Blake2bWrite::<_, EqAffine, Challenge255<EqAffine>>::init(vec![]);
            create_proof(
                &self.deposit_params,
                &self.deposit_pk,
                &[circuit.clone()],
                instances,
                rand::thread_rng(),
                &mut transcript,
            )
            .map_err(|e| ProverError::ProofGeneration(e.to_string()))?;

            Ok(transcript.finalize())
        }

        /// Generate a Halo2 proof for a withdraw circuit.
        pub fn prove_withdraw(&self, circuit: &WithdrawCircuit) -> Result<Vec<u8>, ProverError> {
            let nullifier_fp = bytes_to_fp(&circuit.nullifier);
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&circuit.asset_id.to_le_bytes());
            let asset_id_fp = bytes_to_fp(&asset_bytes);
            let value_fp = bytes_to_fp(&crate::poseidon::value_to_fp_bytes(circuit.value));
            let merkle_root_fp = bytes_to_fp(&circuit.merkle_root);
            let mut target_bytes = [0u8; 32];
            target_bytes[..20].copy_from_slice(&circuit.target_address);
            let target_fp = bytes_to_fp(&target_bytes);

            let instances: &[&[&[Fp]]] = &[&[&[
                nullifier_fp,
                asset_id_fp,
                value_fp,
                merkle_root_fp,
                target_fp,
            ]]];

            let mut transcript =
                Blake2bWrite::<_, EqAffine, Challenge255<EqAffine>>::init(vec![]);
            create_proof(
                &self.withdraw_params,
                &self.withdraw_pk,
                &[circuit.clone()],
                instances,
                rand::thread_rng(),
                &mut transcript,
            )
            .map_err(|e| ProverError::ProofGeneration(e.to_string()))?;

            Ok(transcript.finalize())
        }

        /// Generate a Halo2 proof for a transfer circuit.
        pub fn prove_transfer(&self, circuit: &TransferCircuit) -> Result<Vec<u8>, ProverError> {
            let nullifier0_fp = bytes_to_fp(&circuit.nullifiers[0]);
            let nullifier1_fp = bytes_to_fp(&circuit.nullifiers[1]);
            let commitment0_fp = bytes_to_fp(&circuit.commitments[0]);
            let commitment1_fp = bytes_to_fp(&circuit.commitments[1]);
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&circuit.asset_id.to_le_bytes());
            let asset_id_fp = bytes_to_fp(&asset_bytes);
            let merkle_root_fp = bytes_to_fp(&circuit.merkle_root);

            let instances: &[&[&[Fp]]] = &[&[&[
                nullifier0_fp,
                nullifier1_fp,
                commitment0_fp,
                commitment1_fp,
                asset_id_fp,
                merkle_root_fp,
            ]]];

            let mut transcript =
                Blake2bWrite::<_, EqAffine, Challenge255<EqAffine>>::init(vec![]);
            create_proof(
                &self.transfer_params,
                &self.transfer_pk,
                &[circuit.clone()],
                instances,
                rand::thread_rng(),
                &mut transcript,
            )
            .map_err(|e| ProverError::ProofGeneration(e.to_string()))?;

            Ok(transcript.finalize())
        }

        // ------------------------------------------------------------------
        // Verification
        // ------------------------------------------------------------------

        /// Verify a deposit proof.
        pub fn verify_deposit(
            &self,
            proof_data: &[u8],
            public_inputs: &[u8],
        ) -> Result<bool, ProverError> {
            let instances = Self::bytes_to_instances(public_inputs);
            let strategy = SingleVerifier::new(&self.deposit_params);
            let mut transcript =
                Blake2bRead::<_, EqAffine, Challenge255<EqAffine>>::init(proof_data);
            match verify_proof(
                &self.deposit_params,
                &self.deposit_vk,
                strategy,
                &[&[&instances]],
                &mut transcript,
            ) {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        }

        /// Verify a withdraw proof.
        pub fn verify_withdraw(
            &self,
            proof_data: &[u8],
            public_inputs: &[u8],
        ) -> Result<bool, ProverError> {
            let instances = Self::bytes_to_instances(public_inputs);
            let strategy = SingleVerifier::new(&self.withdraw_params);
            let mut transcript =
                Blake2bRead::<_, EqAffine, Challenge255<EqAffine>>::init(proof_data);
            match verify_proof(
                &self.withdraw_params,
                &self.withdraw_vk,
                strategy,
                &[&[&instances]],
                &mut transcript,
            ) {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        }

        /// Verify a transfer proof.
        pub fn verify_transfer(
            &self,
            proof_data: &[u8],
            public_inputs: &[u8],
        ) -> Result<bool, ProverError> {
            let instances = Self::bytes_to_instances(public_inputs);
            let strategy = SingleVerifier::new(&self.transfer_params);
            let mut transcript =
                Blake2bRead::<_, EqAffine, Challenge255<EqAffine>>::init(proof_data);
            match verify_proof(
                &self.transfer_params,
                &self.transfer_vk,
                strategy,
                &[&[&instances]],
                &mut transcript,
            ) {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        }

        // ------------------------------------------------------------------
        // Helpers
        // ------------------------------------------------------------------

        /// Convert raw public input bytes (32-byte chunks) to Fp elements.
        fn bytes_to_instances(bytes: &[u8]) -> Vec<Fp> {
            bytes
                .chunks(32)
                .map(|chunk| {
                    let mut buf = [0u8; 32];
                    let len = chunk.len().min(32);
                    buf[..len].copy_from_slice(&chunk[..len]);
                    bytes_to_fp(&buf)
                })
                .collect()
        }
    }

    // ---------------------------------------------------------------------------
    // Setup helpers: create valid circuits with witness data for key generation
    // ---------------------------------------------------------------------------

    use crate::circuit_deposit::DepositWitness;
    use crate::circuit_withdraw::WithdrawWitness;
    use crate::poseidon::domain;

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

    fn setup_compute_rcm(
        vk: &ViewingKey,
        value: u128,
        asset_id: u64,
        rho: &[u8; 32],
    ) -> [u8; 32] {
        let ivk_fp = bytes_to_fp(&vk.incoming_view_key);
        let value_fp = bytes_to_fp(&crate::poseidon::value_to_fp_bytes(value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fp = bytes_to_fp(&asset_bytes);
        let rho_fp = bytes_to_fp(rho);
        let rcm_fp = poseidon_hash_tagged(domain::RCM, &[ivk_fp, value_fp, asset_fp, rho_fp]);
        fp_to_bytes(&rcm_fp)
    }

    fn setup_derive_nullifier(ivk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
        let fvk_tag = bytes_to_fp(&crate::poseidon::tag_to_bytes(domain::FVK_FROM_IVK));
        let ivk_fp = bytes_to_fp(ivk);
        let rho_fp = bytes_to_fp(rho);
        let fvk = poseidon_hash(&[fvk_tag, ivk_fp]);
        let nf_fp = poseidon_hash(&[fvk, rho_fp]);
        fp_to_bytes(&nf_fp)
    }

    fn setup_compute_commitment(
        value: u128,
        asset_id: u64,
        rcm: &[u8; 32],
        rho: &[u8; 32],
    ) -> [u8; 32] {
        let value_fp = bytes_to_fp(&crate::poseidon::value_to_fp_bytes(value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fp = bytes_to_fp(&asset_bytes);
        let rcm_fp = bytes_to_fp(rcm);
        let rho_fp = bytes_to_fp(rho);
        let cm_fp = poseidon_hash(&[value_fp, asset_fp, rcm_fp, rho_fp]);
        fp_to_bytes(&cm_fp)
    }

    pub fn setup_deposit_circuit() -> DepositCircuit {
        let sk = setup_spending_key(1);
        let vk = ViewingKey::generate(&sk);
        let rho = setup_hash(1);
        let value: u128 = 1000;
        let asset_id: u64 = 1;

        let rcm = setup_compute_rcm(&vk, value, asset_id, &rho);
        let witness = DepositWitness {
            value,
            rcm,
            recipient_ivk: vk.incoming_view_key,
            rho,
        };
        let commitment = setup_compute_commitment(value, asset_id, &rcm, &rho);

        DepositCircuit::new(commitment, asset_id, witness)
    }

    pub fn setup_withdraw_circuit() -> WithdrawCircuit {
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
            note_value: value,
            rcm,
            recipient_ivk: vk.incoming_view_key,
            rho,
            spending_key: sk,
            merkle_path,
        };
        let target_address = [1u8; 20];

        WithdrawCircuit::new(nullifier, asset_id, value, target_address, merkle_root, witness)
    }

    pub fn setup_transfer_circuit() -> TransferCircuit {
        let asset_id: u64 = 1;

        let mut tree = PoseidonMerkleTree::new(32);
        let mut input_notes = Vec::with_capacity(2);
        let mut nullifiers = Vec::with_capacity(2);

        for i in 0..2 {
            let sk = setup_spending_key(1 + i as u8);
            let vk = ViewingKey::generate(&sk);
            let rho = setup_hash(10 + i as u8);
            let rcm = setup_compute_rcm(&vk, 500, asset_id, &rho);
            let cm = setup_compute_commitment(500, asset_id, &rcm, &rho);

            input_notes.push(InputNoteWitness {
                value: 500,
                rcm,
                recipient_ivk: vk.incoming_view_key,
                rho,
                spending_key: sk,
            });

            let nf = setup_derive_nullifier(&vk.incoming_view_key, &rho);
            nullifiers.push(nf);
            tree.insert(&cm);
        }

        let merkle_root = tree.root();
        let mut merkle_paths = Vec::with_capacity(2);
        for i in 0..2 {
            let proof = tree.proof_for_index(i).unwrap();
            let mut arr = [([0u8; 32], false); 32];
            for (j, p) in proof.iter().enumerate() {
                arr[j] = *p;
            }
            merkle_paths.push(arr);
        }

        let mut output_notes = Vec::with_capacity(2);
        let mut commitments = Vec::with_capacity(2);

        for j in 0..2 {
            let sk = setup_spending_key(20 + j as u8);
            let vk = ViewingKey::generate(&sk);
            let rho = setup_hash(30 + j as u8);
            let rcm = setup_compute_rcm(&vk, 500, asset_id, &rho);
            let cm = setup_compute_commitment(500, asset_id, &rcm, &rho);

            output_notes.push(OutputNoteWitness {
                value: 500,
                rcm,
                recipient_ivk: vk.incoming_view_key,
                rho,
            });
            commitments.push(cm);
        }

        TransferCircuit::new(
            [nullifiers[0], nullifiers[1]],
            [commitments[0], commitments[1]],
            asset_id,
            merkle_root,
            [input_notes[0], input_notes[1]],
            [output_notes[0], output_notes[1]],
            [merkle_paths[0], merkle_paths[1]],
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_halo2_prover_setup_succeeds() {
            let prover = Halo2Prover::setup();
            // Keys are non-empty (asserted by setup() success)
            assert_eq!(prover.deposit_params.k(), 10);
            assert_eq!(prover.withdraw_params.k(), 12);
            assert_eq!(prover.transfer_params.k(), 12);
        }

        #[test]
        fn test_halo2_prover_deposit_proof_cycle() {
            let prover = Halo2Prover::setup();
            let circuit = setup_deposit_circuit();

            let proof_data = prover
                .prove_deposit(&circuit)
                .expect("deposit prove failed");
            assert!(
                proof_data.len() > 128,
                "Halo2 proof should be larger than 128B"
            );

            // Public inputs: commitment (32B) + asset_id (32B) = 64B
            let mut public_inputs = circuit.commitment.to_vec();
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&circuit.asset_id.to_le_bytes());
            public_inputs.extend_from_slice(&asset_bytes);

            let valid = prover
                .verify_deposit(&proof_data, &public_inputs)
                .expect("deposit verify failed");
            assert!(valid, "valid deposit proof should verify");
        }

        #[test]
        fn test_halo2_prover_withdraw_proof_cycle() {
            let prover = Halo2Prover::setup();
            let circuit = setup_withdraw_circuit();

            let proof_data = prover
                .prove_withdraw(&circuit)
                .expect("withdraw prove failed");
            assert!(!proof_data.is_empty());

            // Public inputs: nullifier + asset_id + value + merkle_root + target_address (5 Fp = 160B)
            let mut public_inputs = Vec::new();
            public_inputs.extend_from_slice(&circuit.nullifier);
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&circuit.asset_id.to_le_bytes());
            public_inputs.extend_from_slice(&asset_bytes);
            public_inputs.extend_from_slice(&crate::poseidon::value_to_fp_bytes(circuit.value));
            public_inputs.extend_from_slice(&circuit.merkle_root);
            let mut target_bytes = [0u8; 32];
            target_bytes[..20].copy_from_slice(&circuit.target_address);
            public_inputs.extend_from_slice(&target_bytes);

            let valid = prover
                .verify_withdraw(&proof_data, &public_inputs)
                .expect("withdraw verify failed");
            assert!(valid, "valid withdraw proof should verify");
        }

        #[test]
        fn test_halo2_prover_transfer_proof_cycle() {
            let prover = Halo2Prover::setup();
            let circuit = setup_transfer_circuit();

            let proof_data = prover
                .prove_transfer(&circuit)
                .expect("transfer prove failed");
            assert!(!proof_data.is_empty());

            // Public inputs: nullifier0 + nullifier1 + commitment0 + commitment1 + asset_id + merkle_root
            let mut public_inputs = Vec::new();
            public_inputs.extend_from_slice(&circuit.nullifiers[0]);
            public_inputs.extend_from_slice(&circuit.nullifiers[1]);
            public_inputs.extend_from_slice(&circuit.commitments[0]);
            public_inputs.extend_from_slice(&circuit.commitments[1]);
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&circuit.asset_id.to_le_bytes());
            public_inputs.extend_from_slice(&asset_bytes);
            public_inputs.extend_from_slice(&circuit.merkle_root);

            let valid = prover
                .verify_transfer(&proof_data, &public_inputs)
                .expect("transfer verify failed");
            assert!(valid, "valid transfer proof should verify");
        }

        #[test]
        fn test_halo2_prover_rejects_corrupted_proof() {
            let prover = Halo2Prover::setup();
            let circuit = setup_deposit_circuit();

            let mut proof_data = prover
                .prove_deposit(&circuit)
                .expect("invariant: dev circuit setup succeeds");
            proof_data[10] ^= 0xFF;

            let mut public_inputs = circuit.commitment.to_vec();
            let mut asset_bytes = [0u8; 32];
            asset_bytes[..8].copy_from_slice(&circuit.asset_id.to_le_bytes());
            public_inputs.extend_from_slice(&asset_bytes);

            let result = prover.verify_deposit(&proof_data, &public_inputs);
            match result {
                Ok(valid) => assert!(!valid, "corrupted proof should not verify"),
                Err(_) => {}
            }
        }

        #[test]
        fn test_halo2_prover_wrong_public_inputs_rejected() {
            let prover = Halo2Prover::setup();
            let circuit = setup_deposit_circuit();

            let proof_data = prover
                .prove_deposit(&circuit)
                .expect("invariant: dev circuit setup succeeds");

            let public_inputs = vec![0xFFu8; 64];

            let valid = prover
                .verify_deposit(&proof_data, &public_inputs)
                .expect("invariant: dev circuit setup succeeds");
            assert!(!valid, "wrong public inputs should reject");
        }
    }
}

#[cfg(feature = "halo2-prover")]
pub use halo2_prover_impl::{Halo2Prover, setup_deposit_circuit, setup_transfer_circuit, setup_withdraw_circuit};

// ============================================================================
// MockProver tests (always compiled)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merkle_poseidon::PoseidonMerkleTree;
    use crate::test_utils::test_note;

    fn build_circuit_with_proof() -> (ShieldedCircuit, ZkProof) {
        let mut tree = PoseidonMerkleTree::new(32);
        let input = test_note(1000, 1, 1);
        let leaf: [u8; 32] = input.commitment().0.into();
        tree.insert(&leaf);
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
        )
        .with_merkle_paths(vec![proof]);

        let zk_proof = ZkProof {
            proof_data: vec![1u8; 200],
            nullifiers: vec![nf],
            commitments: vec![cm],
            asset_id: 1,
            key_version: 0,
        };

        (circuit, zk_proof)
    }

    #[test]
    fn test_mock_prover_generate_proof() {
        let (circuit, _) = build_circuit_with_proof();
        let prover = MockProver::new();
        let proof = prover
            .prove(&circuit)
            .expect("invariant: dev circuit setup succeeds");
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
            key_version: 0,
        };
        assert!(!prover.verify(&bad).unwrap());
    }

    #[test]
    fn test_mock_prover_constraint_violation() {
        let input = test_note(1000, 1, 1);
        let output = test_note(2000, 1, 2);
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
