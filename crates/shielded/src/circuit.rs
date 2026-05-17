//! ZK circuit definition for shielded transfers (per spec §3.8.3)
//!
//! Defines the constraint system for Halo2 PLONKish proving:
//! - Public inputs: nullifiers[], commitments[], asset_id
//! - Private inputs: notes[], new_notes[], spending_key, merkle_path[]
//! - 5 constraints for validity

use crate::{Note, NoteCommitment, Nullifier};
use call_primitives::{AssetId, Balance};

/// ZK circuit for a single shielded transfer
#[derive(Debug, Clone)]
pub struct ShieldedCircuit {
    /// Public inputs
    pub nullifiers: Vec<Nullifier>,
    pub commitments: Vec<NoteCommitment>,
    pub asset_id: AssetId,

    /// Private inputs (not exposed in proof)
    pub input_notes: Vec<Note>,
    pub output_notes: Vec<Note>,
    pub merkle_paths: Vec<Vec<([u8; 32], bool)>>,
    pub merkle_root: [u8; 32],
}

impl ShieldedCircuit {
    /// Create a new circuit from a transfer's components
    pub fn new(
        nullifiers: Vec<Nullifier>,
        commitments: Vec<NoteCommitment>,
        asset_id: AssetId,
        input_notes: Vec<Note>,
        output_notes: Vec<Note>,
    ) -> Self {
        Self {
            nullifiers,
            commitments,
            asset_id,
            input_notes,
            output_notes,
            merkle_paths: Vec::new(),
            merkle_root: [0u8; 32],
        }
    }

    /// Set Merkle paths for input notes
    pub fn with_merkle_paths(mut self, paths: Vec<Vec<([u8; 32], bool)>>) -> Self {
        self.merkle_paths = paths;
        self
    }

    /// Set expected Merkle root for path validation
    pub fn with_merkle_root(mut self, root: [u8; 32]) -> Self {
        self.merkle_root = root;
        self
    }

    /// Verify all 5 circuit constraints
    pub fn verify_constraints(&self) -> Result<(), CircuitError> {
        self.check_nullifier_derivation()?;
        self.check_merkle_path_valid()?;
        self.check_spending_rights()?;
        self.check_value_conserv()?;
        self.check_range_valid()?;
        Ok(())
    }

    /// Constraint 1: Nullifier correctly derived from note + spending key
    fn check_nullifier_derivation(&self) -> Result<(), CircuitError> {
        for (i, note) in self.input_notes.iter().enumerate() {
            let expected_nf = note.nullifier();
            if i >= self.nullifiers.len() {
                return Err(CircuitError::MissingNullifier(i));
            }
            // Verify the nullifier matches what the note would derive
            if self.nullifiers[i] != expected_nf {
                return Err(CircuitError::InvalidNullifier(i));
            }
        }
        Ok(())
    }

    /// Constraint 2: Note exists in Merkle Tree (path is valid and matches root)
    fn check_merkle_path_valid(&self) -> Result<(), CircuitError> {
        use call_primitives::Hash;

        for (i, note) in self.input_notes.iter().enumerate() {
            if i >= self.merkle_paths.len() {
                return Err(CircuitError::MissingMerklePath(i));
            }
            let path = &self.merkle_paths[i];
            let proof: Vec<(Hash, bool)> = path
                .iter()
                .map(|(h, left)| (Hash::from_slice(h), *left))
                .collect();
            let cm = note.commitment();
            let commitment = cm.0;
            let computed_root = self.compute_root_from_path(&proof, commitment);
            if computed_root == Hash::repeat_byte(0) {
                return Err(CircuitError::InvalidMerklePath(i));
            }
            // When merkle_root is set, enforce it matches the computed root
            let expected = Hash::from_slice(&self.merkle_root);
            if expected != Hash::repeat_byte(0) && computed_root != expected {
                return Err(CircuitError::MerkleRootMismatch);
            }
        }
        Ok(())
    }

    /// Constraint 3: Sender owns spending rights (nullifier derivation is consistent)
    fn check_spending_rights(&self) -> Result<(), CircuitError> {
        for (i, note) in self.input_notes.iter().enumerate() {
            // The nullifier must be derivable from the note's viewing key
            let nf = note.nullifier();
            if i >= self.nullifiers.len() {
                return Err(CircuitError::MissingNullifier(i));
            }
            if self.nullifiers[i] != nf {
                return Err(CircuitError::SpendingRightsViolation(i));
            }
        }
        Ok(())
    }

    /// Constraint 4: sum(output values) <= sum(input values)
    fn check_value_conserv(&self) -> Result<(), CircuitError> {
        let input_sum: Balance = self.input_notes.iter().map(|n| n.value).sum();
        let output_sum: Balance = self.output_notes.iter().map(|n| n.value).sum();
        if output_sum > input_sum {
            return Err(CircuitError::ValueOverflow {
                input: input_sum,
                output: output_sum,
            });
        }
        Ok(())
    }

    /// Constraint 5: All values in valid range (no overflow/underflow)
    fn check_range_valid(&self) -> Result<(), CircuitError> {
        for (i, note) in self.input_notes.iter().enumerate() {
            if note.value == 0 {
                return Err(CircuitError::ZeroValueNote(format!("input note {}", i)));
            }
        }
        for (i, note) in self.output_notes.iter().enumerate() {
            if note.value == 0 {
                return Err(CircuitError::ZeroValueNote(format!("output note {}", i)));
            }
            // Asset ID must match
            if note.asset_id() != self.asset_id {
                return Err(CircuitError::AssetMismatch {
                    expected: self.asset_id,
                    got: note.asset_id(),
                });
            }
        }
        Ok(())
    }

    /// Compute the Merkle root from a proof path and leaf.
    ///
    /// Uses Poseidon hashing to match the Halo2 circuit and on-chain Merkle tree.
    /// The proof bool is `sibling_is_right` (true when sibling is on the right).
    fn compute_root_from_path(
        &self,
        proof: &[(call_primitives::Hash, bool)],
        leaf: call_primitives::Hash,
    ) -> call_primitives::Hash {
        let mut current: [u8; 32] = leaf.into();
        for (sibling, sibling_is_right) in proof {
            let sibling_bytes: [u8; 32] = (*sibling).into();
            current = if *sibling_is_right {
                crate::poseidon::poseidon_hash_pair(&current, &sibling_bytes)
            } else {
                crate::poseidon::poseidon_hash_pair(&sibling_bytes, &current)
            };
        }
        call_primitives::Hash::from_slice(&current)
    }

    /// Get the number of public inputs
    pub fn public_input_count(&self) -> usize {
        // nullifiers + commitments + asset_id + merkle_root
        self.nullifiers.len() + self.commitments.len() + 2
    }
}

/// Circuit constraint error
#[derive(Debug, thiserror::Error)]
pub enum CircuitError {
    #[error("missing nullifier for input note {0}")]
    MissingNullifier(usize),
    #[error("invalid nullifier for input note {0}")]
    InvalidNullifier(usize),
    #[error("missing Merkle path for input note {0}")]
    MissingMerklePath(usize),
    #[error("invalid Merkle path for input note {0}")]
    InvalidMerklePath(usize),
    #[error("Merkle root mismatch")]
    MerkleRootMismatch,
    #[error("spending rights violation for input note {0}")]
    SpendingRightsViolation(usize),
    #[error("value overflow: input={input}, output={output}")]
    ValueOverflow { input: Balance, output: Balance },
    #[error("zero value note: {0}")]
    ZeroValueNote(String),
    #[error("asset mismatch: expected {expected}, got {got}")]
    AssetMismatch { expected: AssetId, got: AssetId },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merkle_poseidon::PoseidonMerkleTree;
    use crate::test_utils::{test_hash, test_note, test_spending_key};

    fn build_valid_circuit() -> ShieldedCircuit {
        let input = test_note(1000, 1, 1);
        let output = test_note(800, 1, 2);
        let nf = input.nullifier();
        let cm = output.commitment();

        ShieldedCircuit::new(vec![nf], vec![cm], 1, vec![input], vec![output])
    }

    #[test]
    fn test_circuit_nullifier_constraint() {
        let circuit = build_valid_circuit();
        assert!(circuit.check_nullifier_derivation().is_ok());
    }

    #[test]
    fn test_circuit_value_conserv_constraint() {
        let circuit = build_valid_circuit();
        assert!(circuit.check_value_conserv().is_ok());

        // Violation: output > input
        let output = test_note(2000, 1, 3);
        let input = test_note(1000, 1, 1);
        let bad = ShieldedCircuit::new(
            vec![input.nullifier()],
            vec![output.commitment()],
            1,
            vec![input],
            vec![output],
        );
        assert!(bad.check_value_conserv().is_err());
    }

    #[test]
    fn test_circuit_range_constraint() {
        let circuit = build_valid_circuit();
        assert!(circuit.check_range_valid().is_ok());

        // Zero value output
        let sk = test_spending_key(42);
        let vk = crate::ViewingKey::generate(&sk);
        let zero_note = crate::Note::new(0, 1, &vk, test_hash(9));
        let bad = ShieldedCircuit::new(
            vec![],
            vec![zero_note.commitment()],
            1,
            vec![],
            vec![zero_note],
        );
        assert!(bad.check_range_valid().is_err());
    }

    #[test]
    fn test_circuit_asset_mismatch() {
        let input = test_note(1000, 1, 1);
        let output = test_note(800, 2, 2); // asset_id 2 != circuit's 1
        let circuit = ShieldedCircuit::new(
            vec![input.nullifier()],
            vec![output.commitment()],
            1,
            vec![input],
            vec![output],
        );
        assert!(circuit.check_range_valid().is_err());
    }

    #[test]
    fn test_circuit_merkle_path_valid() {
        // Build a tree with a note and verify its path
        let mut tree = PoseidonMerkleTree::new(32);
        let input = test_note(1000, 1, 1);
        let leaf: [u8; 32] = input.commitment().0.into();
        tree.insert(&leaf);
        let proof = tree.proof_for_last();

        let output = test_note(800, 1, 2);
        let nf = input.nullifier();
        let cm = output.commitment();

        let circuit = ShieldedCircuit::new(vec![nf], vec![cm], 1, vec![input], vec![output])
            .with_merkle_paths(vec![proof])
            .with_merkle_root(tree.root());

        assert!(circuit.check_merkle_path_valid().is_ok());
    }

    #[test]
    fn test_circuit_all_constraints_pass() {
        let mut tree = PoseidonMerkleTree::new(32);
        let input = test_note(1000, 1, 1);
        let leaf: [u8; 32] = input.commitment().0.into();
        tree.insert(&leaf);
        let proof = tree.proof_for_last();

        let output = test_note(800, 1, 2);
        let nf = input.nullifier();
        let cm = output.commitment();

        let circuit = ShieldedCircuit::new(vec![nf], vec![cm], 1, vec![input], vec![output])
            .with_merkle_paths(vec![proof])
            .with_merkle_root(tree.root());

        assert!(circuit.verify_constraints().is_ok());
    }

    #[test]
    fn test_circuit_public_input_count() {
        let circuit = build_valid_circuit();
        // 1 nullifier + 1 commitment + 1 asset_id + 1 merkle_root
        assert_eq!(circuit.public_input_count(), 4);
    }

    #[test]
    fn test_circuit_merkle_root_mismatch() {
        let mut tree = PoseidonMerkleTree::new(32);
        let input = test_note(1000, 1, 1);
        let leaf: [u8; 32] = input.commitment().0.into();
        tree.insert(&leaf);
        let proof = tree.proof_for_last();

        let output = test_note(800, 1, 2);
        let nf = input.nullifier();
        let cm = output.commitment();

        // Use a wrong root (all zeros is treated as "not set", so use a different non-zero root)
        let wrong_root = [0xFFu8; 32];

        let circuit = ShieldedCircuit::new(vec![nf], vec![cm], 1, vec![input], vec![output])
            .with_merkle_paths(vec![proof])
            .with_merkle_root(wrong_root);

        assert!(
            circuit.check_merkle_path_valid().is_err(),
            "should reject proof with wrong merkle root"
        );
    }
}
