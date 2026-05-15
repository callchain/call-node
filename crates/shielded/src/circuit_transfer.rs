//! ShieldedTransfer circuit — Halo2 PLONKish version over Pasta Pallas.
//!
//! Proves a user is transferring shielded funds to new shielded notes.
//! Temporarily stubbed during migration; full Halo2 implementation
//! will be added in Phase 4.

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

/// ShieldedTransfer circuit.
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

/// Compute the public input byte count for a transfer circuit with N inputs and M outputs.
pub const fn transfer_public_input_count(n_inputs: usize, n_outputs: usize) -> usize {
    n_inputs * 32   // nullifiers
        + n_outputs * 32  // commitments
        + 8               // asset_id
        + 32 // merkle_root
}
