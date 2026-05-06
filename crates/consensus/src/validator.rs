//! T6.2 — Validator Staking types (per spec §12.6)
//!
//! Validator stake data types. All validator state management now lives
//! in EVM storage via the validator precompile (0x204).

use call_primitives::{Address, Ed25519PublicKey, ValidatorId};
use serde::{Deserialize, Serialize};

// ── Constants ─────────────────────────────────────────────────────────

/// System escrow address for staked CALL tokens (Cosmos-style module account)
pub const STAKING_ESCROW: Address = Address::new([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x0A, 0xCE,
]);

// ── Types ─────────────────────────────────────────────────────────────

/// Slash event record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashEvent {
    pub reason: String,
    pub amount_slashed: u128,
    pub block: u64,
}

/// Validator stake state (per spec §12.6)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorStake {
    pub validator_id: ValidatorId,
    pub address: Address,
    pub ed25519_pubkey: Ed25519PublicKey,
    pub staked_call: u128,
    pub self_stake: u128,
    pub delegated_call: u128,
    pub rewards: u128,
    pub slash_history: Vec<SlashEvent>,
    pub unbonding_start: Option<u64>, // block height when unbonding started
    /// BLS12-381 public key for aggregated vote signatures (48 bytes compressed)
    #[serde(default = "default_bls_pubkey", with = "serde_bytes")]
    pub bls_pubkey: [u8; 48],
}

fn default_bls_pubkey() -> [u8; 48] {
    [0u8; 48]
}

/// Pending unbonding request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnbondingRequest {
    pub validator_id: ValidatorId,
    /// Original staker address — used to return tokens from escrow on claim
    pub sender_address: Address,
    pub amount: u128,
    pub requested_at_block: u64,
    pub eligible_at_block: u64,
}

/// Key rotation record — tracks pubkey transitions with grace period
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRotation {
    pub validator_id: ValidatorId,
    pub old_pubkey: Ed25519PublicKey,
    pub new_pubkey: Ed25519PublicKey,
    pub rotation_block: u64,
}

// ── Consensus Error ───────────────────────────────────────────────────

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConsensusError {
    #[error("invalid block: {0}")]
    InvalidBlock(String),
    #[error("proposer not in subset: {0}")]
    ProposerNotInSubset(ValidatorId),
    #[error("signature verification failed")]
    InvalidSignature,
    #[error("double sign detected: validator={0}")]
    DoubleSign(ValidatorId),
    #[error("validator offline: {0}")]
    ValidatorOffline(ValidatorId),
    #[error("insufficient stake")]
    InsufficientStake,
    #[error("unbonding period not elapsed")]
    UnbondingNotElapsed,
    #[error("validator not found: {0}")]
    ValidatorNotFound(ValidatorId),
    #[error("consensus error: {0}")]
    ConsensusError(String),
}
