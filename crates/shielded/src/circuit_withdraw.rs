//! ShieldedWithdraw circuit — Halo2 PLONKish version over Pasta Pallas.
//!
//! Proves that a user is withdrawing from the shielded pool to a transparent
//! address, consuming a shielded note. Temporarily stubbed during migration;
//! full Halo2 implementation will be added in Phase 3.

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

/// Number of public inputs for the withdraw circuit.
pub const fn withdraw_public_input_count() -> usize {
    32 + 8 + 16 + 20 + 32 // nullifier + asset_id + value + target_address + merkle_root
}
