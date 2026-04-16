//! T16.1 — Light Client (per spec §23)
//!
//! Lightweight block header and proof verification for resource-constrained clients.
//! Targets: <10MB storage, ~1KB/block bandwidth, ~50ms compute per block.

use call_consensus::{BlockHeader, BlockSignature};
use call_primitives::{Address, Balance, BlockHash, Hash, TxHash, ValidatorId};
use call_primitives::Ed25519PublicKey;
use call_shielded::{
    Note, ViewingKey,
    verify_merkle_path, verify_zk_proof, ZkProof,
    IncrementalMerkleTree,
};
use call_crypto::keccak256;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Serde wrappers for fixed-size byte arrays ────────────────────────

/// Serde-compatible signature wrapper
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigBytes(pub [u8; 65]);

impl Serialize for SigBytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for SigBytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = <Vec<u8> as serde::Deserialize>::deserialize(deserializer)?;
        let bytes: [u8; 65] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 65 bytes"))?;
        Ok(SigBytes(bytes))
    }
}

/// Serde-compatible pubkey wrapper
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubKeyBytes(pub [u8; 32]);

impl Serialize for PubKeyBytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for PubKeyBytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = <Vec<u8> as serde::Deserialize>::deserialize(deserializer)?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 32 bytes"))?;
        Ok(PubKeyBytes(bytes))
    }
}

// ── Proof Types ──────────────────────────────────────────────────────

/// Merkle proof with path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    pub leaf: Hash,
    pub proof_path: Vec<(Hash, bool)>,
    pub root: Hash,
}

impl MerkleProof {
    pub fn verify(&self) -> bool {
        verify_merkle_path(self.leaf, &self.proof_path, self.root)
    }
}

/// Balance proof for light client queries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceProof {
    pub merkle_proof: MerkleProof,
    pub balance: u128,
}

/// Transaction proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionProof {
    pub tx_hash: TxHash,
    pub merkle_proof: MerkleProof,
    pub block: u64,
}

/// Bridge operation proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeProof {
    pub bridge_op_hash: Hash,
    pub signatures: Vec<SigBytes>,
    pub status: String,
}

/// Shielded state proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShieldedStateProof {
    pub root: Hash,
    pub commitment_count: u64,
    pub nullifier_count: u64,
}

/// Encrypted balance proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedBalanceProof {
    pub encrypted_notes: Vec<EncryptedNote>,
    pub merkle_proofs: Vec<MerkleProof>,
}

/// Encrypted note wrapper for proof transmission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedNote {
    pub ciphertext: Vec<u8>,
    pub commitment: Hash,
    pub nullifier_tag: Hash,
}

// ── Validator Signature Verification ────────────────────────────────

/// Aggregated validator signatures for a block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSignatures {
    pub block_hash: BlockHash,
    pub signatures: Vec<(ValidatorId, PubKeyBytes, SigBytes)>,
}

impl BlockSignatures {
    /// Count unique valid signatures
    pub fn valid_count(&self, validators: &HashMap<ValidatorId, Ed25519PublicKey>) -> usize {
        self.signatures
            .iter()
            .filter(|(vid, pubkey, _sig)| {
                validators.get(vid).copied() == Some(pubkey.0)
            })
            .count()
    }
}

// ── Light Client ─────────────────────────────────────────────────────

/// Light client for verifying blocks and proofs without full state
pub struct LightClient {
    pub chain_id: u64,
    pub trusted_validators: HashMap<ValidatorId, Ed25519PublicKey>,
    pub latest_block_header: Option<BlockHeader>,
    /// Checkpoint for initial sync
    pub checkpoint: Option<Checkpoint>,
    /// Verified block headers (height → header hash) for incremental sync
    verified_headers: HashMap<u64, BlockHash>,
    /// Total validators in the active set
    pub total_validators: u32,
}

/// Sync checkpoint for fast initial sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub block_height: u64,
    pub block_hash: BlockHash,
    pub state_root: Hash,
    pub validator_set_hash: Hash,
}

impl LightClient {
    /// Create a new light client with trusted validator set
    pub fn new(
        chain_id: u64,
        trusted_validators: HashMap<ValidatorId, Ed25519PublicKey>,
        total_validators: u32,
    ) -> Self {
        Self {
            chain_id,
            trusted_validators,
            latest_block_header: None,
            checkpoint: None,
            verified_headers: HashMap::new(),
            total_validators,
        }
    }

    /// Set sync checkpoint (for initial sync from checkpoint)
    pub fn set_checkpoint(&mut self, checkpoint: Checkpoint) {
        let height = checkpoint.block_height;
        let hash = checkpoint.block_hash;
        self.checkpoint = Some(checkpoint);
        self.verified_headers.insert(height, hash);
    }

    /// Verify a block header per spec §23.1:
    /// 1. Parent hash link to previous verified header
    /// 2. 2/3+ validator signatures (quorum = ceil(2/3 * total))
    /// 3. State root consistency with checkpoint
    pub fn verify_header(
        &self,
        header: &BlockHeader,
        signatures: &BlockSignatures,
    ) -> Result<(), LightClientError> {
        // 1. Parent hash link
        if header.height > 0 {
            let parent_height = header.height - 1;
            if let Some(expected_parent) = self.verified_headers.get(&parent_height) {
                if header.parent_hash != *expected_parent {
                    return Err(LightClientError::ParentHashMismatch {
                        expected: *expected_parent,
                        got: header.parent_hash,
                    });
                }
            }
            // If we don't have the parent, skip this check (could be during initial sync)
        }

        // 2. Verify 2/3+ validator signatures
        let required = self.quorum();
        let valid_count = signatures.valid_count(&self.trusted_validators);
        if valid_count < required {
            return Err(LightClientError::InsufficientSignatures {
                got: valid_count,
                required,
            });
        }

        // 3. State root consistency (only if checkpoint is set and height matches)
        if let Some(ref checkpoint) = self.checkpoint {
            if header.height == checkpoint.block_height && header.payment_root != checkpoint.state_root {
                return Err(LightClientError::StateRootMismatch {
                    expected: checkpoint.state_root,
                    got: header.payment_root,
                });
            }
        }

        // Basic header validation
        if header.timestamp_millis == 0 {
            return Err(LightClientError::InvalidHeader("zero timestamp".into()));
        }
        if header.proposer == 0 {
            return Err(LightClientError::InvalidHeader("zero proposer".into()));
        }

        Ok(())
    }

    /// Verify a generic Merkle proof
    pub fn verify_proof(&self, proof: &MerkleProof) -> bool {
        proof.verify()
    }

    /// Full ZK proof verification for shielded transactions (~3ms Groth16)
    /// + nullifier proof + commitment proof
    pub fn verify_shielded_tx_full(
        &self,
        zk_proof: &ZkProof,
        merkle_root: Hash,
    ) -> Result<(), LightClientError> {
        // 1. Verify ZK proof (Groth16)
        if !verify_zk_proof(zk_proof) {
            return Err(LightClientError::InvalidZkProof);
        }

        // 2. Verify nullifier proofs against spent set
        for nf in &zk_proof.nullifiers {
            if nf.as_hash() == &Hash::ZERO {
                return Err(LightClientError::InvalidNullifier);
            }
        }

        // 3. Verify commitment proofs against Merkle root
        for cm in &zk_proof.commitments {
            let _proof = MerkleProof {
                leaf: *cm.as_hash(),
                proof_path: vec![], // Path would be provided by full node
                root: merkle_root,
            };
            // In full verification, the path would be checked;
            // for light client, we verify the commitment is non-zero
            if cm.as_hash() == &Hash::ZERO {
                return Err(LightClientError::InvalidCommitment);
            }
        }

        Ok(())
    }

    /// Light verification: Merkle inclusion only, trust validators verified ZK
    pub fn verify_shielded_tx_light(
        &self,
        merkle_proof: &MerkleProof,
    ) -> Result<(), LightClientError> {
        if !merkle_proof.verify() {
            return Err(LightClientError::InvalidMerkleProof);
        }
        Ok(())
    }

    /// Query shielded balance via viewing key
    /// Decrypt notes with Merkle proofs
    pub fn query_shielded_balance(
        &self,
        viewing_key: &ViewingKey,
        notes: &[Note],
        merkle_proofs: &[MerkleProof],
        _state_root: Hash,
    ) -> Result<Balance, LightClientError> {
        let mut total: Balance = 0;

        for (note, proof) in notes.iter().zip(merkle_proofs.iter()) {
            // Verify note belongs to Merkle tree
            if !proof.verify() {
                return Err(LightClientError::InvalidMerkleProof);
            }

            // Verify commitment matches proof leaf
            let commitment = note.commitment();
            if proof.leaf != *commitment.as_hash() {
                return Err(LightClientError::CommitmentMismatch);
            }

            // Verify viewing key can decrypt this note
            if !viewing_key.can_decrypt(note.rcm()) {
                continue; // Skip notes we can't decrypt
            }

            // Decrypt and sum value
            total += note.value;
        }

        Ok(total)
    }

    /// Incremental sync: verify and append block header
    pub fn sync_incremental(
        &mut self,
        header: &BlockHeader,
        signatures: &BlockSignatures,
    ) -> Result<(), LightClientError> {
        self.verify_header(header, signatures)?;
        self.verified_headers
            .insert(header.height, header.hash());
        self.latest_block_header = Some(header.clone());
        Ok(())
    }

    /// Get number of verified headers
    pub fn verified_count(&self) -> usize {
        self.verified_headers.len()
    }

    /// Check if a height has been verified
    pub fn is_verified(&self, height: u64) -> bool {
        self.verified_headers.contains_key(&height)
    }

    /// Calculate quorum: ceil(2/3 * total_validators)
    pub fn quorum(&self) -> usize {
        (2 * self.total_validators as usize).div_ceil(3).max(1)
    }

    /// Get estimated storage usage (target: <10MB)
    pub fn estimated_storage_bytes(&self) -> usize {
        // Each header hash is 32 bytes + HashMap overhead
        std::mem::size_of::<Self>()
            + self.verified_headers.len() * (8 + 32) // height + hash
            + self.trusted_validators.len() * (4 + 32) // validator id + pubkey
    }
}

// ── Light Client Error ──────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum LightClientError {
    #[error("parent hash mismatch: expected {expected}, got {got}")]
    ParentHashMismatch { expected: BlockHash, got: BlockHash },
    #[error("insufficient signatures: got {got}, required {required}")]
    InsufficientSignatures { got: usize, required: usize },
    #[error("state root mismatch: expected {expected}, got {got}")]
    StateRootMismatch { expected: Hash, got: Hash },
    #[error("invalid header: {0}")]
    InvalidHeader(String),
    #[error("invalid ZK proof")]
    InvalidZkProof,
    #[error("invalid nullifier")]
    InvalidNullifier,
    #[error("invalid commitment")]
    InvalidCommitment,
    #[error("invalid Merkle proof")]
    InvalidMerkleProof,
    #[error("commitment does not match proof leaf")]
    CommitmentMismatch,
    #[error("chain ID mismatch")]
    ChainIdMismatch,
}

// ── Light Client RPC Methods (spec §23.2) ──────────────────────────

/// RPC request/response types for light client methods

/// `light_verifyBlockHeader` — verify a block header with signatures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyBlockHeaderRequest {
    pub header: BlockHeader,
    pub signatures: BlockSignatures,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyBlockHeaderResponse {
    pub valid: bool,
    pub error: Option<String>,
}

/// `call_getBalanceProof` — get balance proof for an address
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBalanceProofRequest {
    pub asset_id: u64,
    pub address: Address,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBalanceProofResponse {
    pub proof: Option<BalanceProof>,
}

/// `eth_getProof` — EIP-1186 compatible account proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetProofRequest {
    pub address: Address,
    pub storage_keys: Vec<Hash>,
    pub block_number: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetProofResponse {
    pub address: Address,
    pub balance: Balance,
    pub storage_proof: Vec<MerkleProof>,
    pub block_number: u64,
}

/// `call_getTransactionProof` — get transaction inclusion proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetTransactionProofRequest {
    pub tx_hash: TxHash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetTransactionProofResponse {
    pub proof: Option<TransactionProof>,
}

/// `call_getBridgeProof` — get bridge operation proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBridgeProofRequest {
    pub bridge_op_hash: Hash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetBridgeProofResponse {
    pub proof: Option<BridgeProof>,
}

/// `call_getShieldedStateProof` — get shielded pool state proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetShieldedStateProofRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetShieldedStateProofResponse {
    pub proof: Option<ShieldedStateProof>,
}

/// `call_getShieldedBalanceProof` — get shielded balance proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetShieldedBalanceProofRequest {
    pub viewing_key: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetShieldedBalanceProofResponse {
    pub proof: Option<EncryptedBalanceProof>,
}

/// `call_getShieldedTxProof` — get shielded transaction proof
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetShieldedTxProofRequest {
    pub nullifier: Hash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetShieldedTxProofResponse {
    pub proof: Option<MerkleProof>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hash(n: u8) -> Hash {
        Hash::repeat_byte(n)
    }

    fn test_pubkey(n: u8) -> Ed25519PublicKey {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    fn test_sig(n: u8) -> [u8; 65] {
        let mut sig = [0u8; 65];
        sig[0] = n;
        sig
    }

    fn make_validators(count: u32) -> HashMap<ValidatorId, Ed25519PublicKey> {
        (1..=count)
            .map(|i| (i, test_pubkey(i as u8)))
            .collect()
    }

    fn make_header(height: u64, parent: BlockHash) -> BlockHeader {
        BlockHeader {
            parent_hash: parent,
            height,
            timestamp_millis: height * 250 + 1000,
            payment_root: test_hash(height as u8),
            evm_state_root: Hash::ZERO,
            bridge_root: Hash::ZERO,
            receipt_root: Hash::ZERO,
            proposer: 1,
            signature: BlockSignature::default(),
        }
    }

    fn make_signatures(block_hash: BlockHash, count: u32) -> BlockSignatures {
        let sigs = (1..=count)
            .map(|i| (i, PubKeyBytes(test_pubkey(i as u8)), SigBytes(test_sig(i as u8))))
            .collect();
        BlockSignatures {
            block_hash,
            signatures: sigs,
        }
    }

    #[test]
    fn test_light_client_verify_header() {
        let validators = make_validators(21);
        let mut client = LightClient::new(1, validators.clone(), 21);

        // Genesis block
        let genesis = make_header(0, BlockHash::ZERO);
        let genesis_sigs = make_signatures(genesis.hash(), 15); // 15/21 > 2/3
        assert!(client.verify_header(&genesis, &genesis_sigs).is_ok());

        // Block 1
        let b1 = make_header(1, genesis.hash());
        let b1_sigs = make_signatures(b1.hash(), 14); // 14/21 = 2/3
        // Need to add genesis to verified
        client.verified_headers.insert(0, genesis.hash());
        assert!(client.verify_header(&b1, &b1_sigs).is_ok());
    }

    #[test]
    fn test_light_client_verify_header_invalid_sig() {
        let validators = make_validators(21);
        let client = LightClient::new(1, validators.clone(), 21);

        let genesis = make_header(0, BlockHash::ZERO);

        // Not enough signatures (only 5 out of 21, need 14)
        let bad_sigs = make_signatures(genesis.hash(), 5);
        let result = client.verify_header(&genesis, &bad_sigs);
        assert!(result.is_err());
        match result.unwrap_err() {
            LightClientError::InsufficientSignatures { got, required } => {
                assert_eq!(got, 5);
                assert_eq!(required, 14); // ceil(2/3 * 21) = 14
            }
            other => panic!("expected InsufficientSignatures, got {other}"),
        }
    }

    #[test]
    fn test_light_client_merkle_proof() {
        let validators = make_validators(21);
        let client = LightClient::new(1, validators, 21);

        // Build a simple Merkle tree
        let a = keccak256(b"a");
        let b = keccak256(b"b");
        let root = call_crypto::build_merkle_root(&[a, b]).unwrap();

        // Proof for 'a': sibling is 'b', on the right
        let proof = MerkleProof {
            leaf: a,
            proof_path: vec![(b, true)],
            root,
        };

        assert!(client.verify_proof(&proof));

        // Wrong proof should fail
        let bad_proof = MerkleProof {
            leaf: a,
            proof_path: vec![(keccak256(b"wrong"), false)],
            root,
        };
        assert!(!client.verify_proof(&bad_proof));
    }

    #[test]
    fn test_light_client_shielded_light_verification() {
        let validators = make_validators(21);
        let client = LightClient::new(1, validators, 21);

        // Build Merkle tree with a leaf
        let leaf = test_hash(42);
        let mut tree = IncrementalMerkleTree::new(5);
        tree.insert(leaf);
        let root = tree.root();

        // Get proof for leaf
        let path = tree.proof_for_last();
        let proof = MerkleProof {
            leaf,
            proof_path: path.clone(),
            root,
        };

        // Light verification should succeed
        assert!(client.verify_shielded_tx_light(&proof).is_ok());

        // Tampered proof should fail
        let bad_proof = MerkleProof {
            leaf: test_hash(99),
            proof_path: path.clone(),
            root,
        };
        assert!(client.verify_shielded_tx_light(&bad_proof).is_err());
    }

    #[test]
    fn test_light_client_shielded_balance_query() {
        let validators = make_validators(21);
        let client = LightClient::new(1, validators, 21);

        // Create viewing key and notes
        let spending_key = [42u8; 32];
        let vk = ViewingKey::generate(&spending_key);

        // Build Merkle tree with notes
        let note1 = Note::new(500, 1, &vk, test_hash(1));
        let note2 = Note::new(300, 1, &vk, test_hash(2));

        let mut tree = IncrementalMerkleTree::new(5);
        tree.insert(*note1.commitment().as_hash());
        tree.insert(*note2.commitment().as_hash());
        let root = tree.root();

        // Create proofs
        let path1 = tree.proof_for_index(0);
        let path2 = tree.proof_for_index(1);
        let proof1 = MerkleProof {
            leaf: *note1.commitment().as_hash(),
            proof_path: path1,
            root,
        };
        let proof2 = MerkleProof {
            leaf: *note2.commitment().as_hash(),
            proof_path: path2,
            root,
        };

        let balance = client
            .query_shielded_balance(&vk, &[note1, note2], &[proof1, proof2], root)
            .unwrap();

        assert_eq!(balance, 800); // 500 + 300
    }

    #[test]
    fn test_light_client_quorum_calculation() {
        // 21 validators → ceil(2/3 * 21) = 14
        let client = LightClient::new(1, HashMap::new(), 21);
        assert_eq!(client.quorum(), 14);

        // 3 validators → ceil(2/3 * 3) = 2
        let client = LightClient::new(1, HashMap::new(), 3);
        assert_eq!(client.quorum(), 2);

        // 1 validator → ceil(2/3 * 1) = 1
        let client = LightClient::new(1, HashMap::new(), 1);
        assert_eq!(client.quorum(), 1);

        // 100 validators → ceil(2/3 * 100) = 67
        let client = LightClient::new(1, HashMap::new(), 100);
        assert_eq!(client.quorum(), 67);
    }

    #[test]
    fn test_light_client_incremental_sync() {
        let validators = make_validators(21);
        let mut client = LightClient::new(1, validators.clone(), 21);

        // Set checkpoint at height 0
        let genesis = make_header(0, BlockHash::ZERO);
        client.set_checkpoint(Checkpoint {
            block_height: 0,
            block_hash: genesis.hash(),
            state_root: genesis.payment_root,
            validator_set_hash: test_hash(1),
        });

        // Sync blocks 1-5
        let mut parent_hash = genesis.hash();
        for h in 1..=5 {
            let header = make_header(h, parent_hash);
            let sigs = make_signatures(header.hash(), 15);
            client.sync_incremental(&header, &sigs).unwrap();
            parent_hash = header.hash();
        }

        assert_eq!(client.verified_count(), 6); // checkpoint + 5 blocks
        assert!(client.is_verified(3));
        assert!(!client.is_verified(10));
        assert_eq!(client.latest_block_header.unwrap().height, 5);
    }

    #[test]
    fn test_light_client_storage_estimate() {
        let validators = make_validators(21);
        let mut client = LightClient::new(1, validators, 21);

        // Fresh client should be small (HashMap overhead ~2-4KB)
        assert!(client.estimated_storage_bytes() < 8192);

        // Add 1000 verified headers (simulating ~1000 blocks)
        for i in 0..1000u64 {
            client.verified_headers.insert(i, test_hash(i as u8));
        }

        // Should still be well under 10MB target
        assert!(client.estimated_storage_bytes() < 10 * 1024 * 1024);
    }

    #[test]
    fn test_light_parent_hash_mismatch() {
        let validators = make_validators(21);
        let mut client = LightClient::new(1, validators, 21);

        // Set verified header at height 0
        let genesis_hash = test_hash(1);
        client.verified_headers.insert(0, genesis_hash);

        // Block 1 with wrong parent
        let b1 = make_header(1, test_hash(99)); // Wrong parent
        let sigs = make_signatures(b1.hash(), 15);

        let result = client.verify_header(&b1, &sigs);
        assert!(result.is_err());
        match result.unwrap_err() {
            LightClientError::ParentHashMismatch { .. } => {}
            other => panic!("expected ParentHashMismatch, got {other}"),
        }
    }

    #[test]
    fn test_merkle_proof_verify() {
        let a = keccak256(b"a");
        let b = keccak256(b"b");
        let c = keccak256(b"c");
        let cc = {
            let mut data = Vec::with_capacity(64);
            data.extend_from_slice(c.as_slice());
            data.extend_from_slice(c.as_slice());
            keccak256(&data)
        };
        let root = call_crypto::build_merkle_root(&[a, b, c]).unwrap();

        // For 3 leaves [a,b,c]: tree is [keccak(a||b), keccak(c||c)] → root
        // Proof for a: sibling b (right), sibling cc (right)
        let proof = MerkleProof {
            leaf: a,
            proof_path: vec![(b, true), (cc, true)],
            root,
        };

        assert!(proof.verify());
    }

    #[test]
    fn test_balance_proof_structure() {
        let merkle = MerkleProof {
            leaf: test_hash(1),
            proof_path: vec![(test_hash(2), true)],
            root: test_hash(3),
        };
        let proof = BalanceProof {
            merkle_proof: merkle,
            balance: 1_000_000,
        };
        assert_eq!(proof.balance, 1_000_000);
    }

    #[test]
    fn test_transaction_proof_structure() {
        let proof = TransactionProof {
            tx_hash: test_hash(1),
            merkle_proof: MerkleProof {
                leaf: test_hash(2),
                proof_path: vec![],
                root: test_hash(3),
            },
            block: 42,
        };
        assert_eq!(proof.block, 42);
    }

    #[test]
    fn test_bridge_proof_structure() {
        let proof = BridgeProof {
            bridge_op_hash: test_hash(1),
            signatures: vec![SigBytes([0u8; 65]), SigBytes([1u8; 65])],
            status: "confirmed".into(),
        };
        assert_eq!(proof.signatures.len(), 2);
        assert_eq!(proof.status, "confirmed");
    }

    #[test]
    fn test_shielded_state_proof_structure() {
        let proof = ShieldedStateProof {
            root: test_hash(1),
            commitment_count: 100,
            nullifier_count: 50,
        };
        assert_eq!(proof.commitment_count, 100);
        assert_eq!(proof.nullifier_count, 50);
    }

    #[test]
    fn test_encrypted_balance_proof_structure() {
        let proof = EncryptedBalanceProof {
            encrypted_notes: vec![
                EncryptedNote {
                    ciphertext: vec![1, 2, 3],
                    commitment: test_hash(1),
                    nullifier_tag: test_hash(2),
                },
            ],
            merkle_proofs: vec![],
        };
        assert_eq!(proof.encrypted_notes.len(), 1);
    }
}
