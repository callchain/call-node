//! Instruction types: enum, memos, payments, compliance status.

use crate::{ProtocolError, ProtocolResult};
use call_primitives::{Address, AssetId, Balance};

/// Protocol instruction variants (per spec §3.5)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Instruction {
    Transfer {
        asset_id: AssetId,
        to: Address,
        amount: Balance,
        memo: Option<PaymentMemo>,
    },
    BatchTransfer {
        asset_id: AssetId,
        payments: Vec<PaymentEntry>,
    },
    Approve {
        asset_id: AssetId,
        spender: Address,
        amount: Balance,
    },
    TransferFrom {
        asset_id: AssetId,
        from: Address,
        to: Address,
        amount: Balance,
    },
    Mint {
        asset_id: AssetId,
        to: Address,
        amount: Balance,
    },
    Burn {
        asset_id: AssetId,
        from: Address,
        amount: Balance,
    },
    AgentPay {
        payment: AgentPayment,
    },
    AgentBatchPay {
        payments: Vec<AgentPayment>,
    },
    AgentCall {
        agent_id: u64,
        target: Address,
        data: Vec<u8>,
    },
    AgentBridgeDeposit {
        agent_id: u64,
        asset_id: AssetId,
        amount: Balance,
        target_chain: u64,
        target_address: Vec<u8>,
    },
    BridgeDeposit {
        source_chain: u64,
        target_address: Address,
        amount: Balance,
        asset_id: AssetId,
        proof: Vec<u8>,
    },
    /// Register a new protocol asset and auto-deploy an EVM wrapped token
    RegisterAsset {
        symbol: String,
        name: String,
        decimals: u8,
        max_supply: Balance,
    },
    /// Bridge protocol balance to EVM (native CALL or ERC-20 wrapped asset)
    BridgeToEvm {
        asset_id: AssetId,
        to: Address,
        amount: Balance,
    },
    /// Bridge EVM balance back to protocol (native CALL or ERC-20 wrapped asset)
    BridgeToProtocol {
        asset_id: AssetId,
        to: Address,
        amount: Balance,
    },
    /// Mint wrapped ERC-20 tokens directly on EVM (issuer only)
    EvmIssuerMint {
        asset_id: AssetId,
        to: Address,
        amount: Balance,
    },
    UpdateCompliance {
        asset_id: AssetId,
        target: Address,
        status: ComplianceStatus,
    },
    ShieldedTransfer {
        asset_id: AssetId,
        proof: Vec<u8>,
        nullifiers: Vec<call_primitives::Hash>,
        commitments: Vec<call_primitives::Hash>,
        encrypted_notes: Vec<Vec<u8>>,
    },
    ShieldedWithdraw {
        asset_id: AssetId,
        target: Address,
        amount: Balance,
        proof: Vec<u8>,
        nullifier: call_primitives::Hash,
    },
    ShieldedDeposit {
        asset_id: AssetId,
        amount: Balance,
        commitment: call_primitives::Hash,
        encrypted_note: Vec<u8>,
    },
    OracleSubmit {
        asset_id: AssetId,
        price: u128,
        block_number: u64,
        timestamp: u64,
        signature: Vec<u8>,
        sources: Vec<String>,
    },
    /// Submit a governance proposal (per spec §13.3)
    GovernanceSubmitProposal {
        proposal_type: call_governance::ProposalType,
        title: String,
        description: String,
        execution_data: Vec<u8>,
    },
    /// Cast a vote on an active governance proposal
    GovernanceVote {
        proposal_id: u64,
        vote: call_governance::Vote,
    },
    /// Queue a passed proposal for execution after timelock
    GovernanceQueue {
        proposal_id: u64,
    },
    /// Execute a queued proposal after timelock elapsed
    GovernanceExecute {
        proposal_id: u64,
    },
    /// Initiate emergency pause (requires validator threshold)
    GovernanceEmergencyPause {
        reason: String,
    },
    /// Resume from emergency pause (requires governance)
    GovernanceEmergencyResume,
    /// External bridge deposit claim with validator signatures
    ExternalBridgeDeposit {
        source_tx_hash: [u8; 32],
        source_chain: u8,
        source_block_number: u64,
        external_sender: Vec<u8>,
        recipient: Address,
        asset_id: AssetId,
        amount: Balance,
        validator_signatures: Vec<(u32, Vec<u8>)>,
    },
    /// External bridge withdrawal to an external chain address.
    /// The protocol burns the sender's balance and emits a withdrawal event
    /// for validators to sign and relay to the target chain.
    ExternalBridgeWithdraw {
        target_chain: u8,
        target_address: Vec<u8>,
        asset_id: AssetId,
        sender: Address,
        amount: Balance,
    },
    /// Challenge a pending bridge deposit during the challenge period.
    /// Permissionless — anyone can submit proof that a deposit is fraudulent
    /// (e.g. source tx was reorged, or signatures are from slashed validators).
    ChallengeBridgeDeposit {
        source_tx_hash: [u8; 32],
        proof: Vec<u8>,
    },
    /// Stake CALL to become a validator.
    /// Deducts `self_stake` from sender's balance and registers them in the validator set.
    ValidatorStake {
        ed25519_pubkey: [u8; 32],
        self_stake: u128,
    },
    /// Begin unbonding for a validator.
    /// The validator is removed from the active set immediately; stake can be
    /// claimed after the unbonding period elapsed.
    ValidatorUnstake {
        validator_id: u32,
    },
    /// Claim unbonded stake after the unbonding period has elapsed.
    /// Transfers staked CALL from the escrow back to the original staker.
    ValidatorClaimUnbonded {
        validator_id: u32,
    },
    /// Register a new agent. The transaction sender becomes the owner.
    /// Deducts `fee_params.base_fee` from the sender's CALL balance.
    RegisterAgent {
        pubkey: Vec<u8>,
        name: String,
        url: String,
    },
    /// Grant protocol balance to an agent. Only the agent owner can grant.
    GrantAgentBalance {
        agent_id: u64,
        asset_id: AssetId,
        amount: Balance,
    },
    /// Revoke all protocol balance from an agent for a given asset.
    /// Only the agent owner can revoke. Funds are not returned to owner.
    RevokeAgentBalance {
        agent_id: u64,
        asset_id: AssetId,
    },
    /// Submit an Ed25519-signed emergency rollback signature.
    /// When quorum is reached, a RollbackPlan is produced in the block result.
    SubmitRollbackSignature {
        validator_id: u32,
        target_height: u64,
        target_version_major: u16,
        target_version_minor: u16,
        target_version_patch: u16,
        nonce: u64,
        signature: Vec<u8>,
    },
}

/// Payment memo with size limits per spec §3.5
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PaymentMemo {
    pub message: String,           // max 256 bytes
    pub reference: Option<String>, // max 128 bytes
    pub metadata: Option<Vec<u8>>, // max 1024 bytes
}

pub const MAX_MEMO_MESSAGE: usize = 256;
pub const MAX_MEMO_REFERENCE: usize = 128;
pub const MAX_MEMO_METADATA: usize = 1024;

impl PaymentMemo {
    pub fn validate(&self) -> ProtocolResult<()> {
        if self.message.len() > MAX_MEMO_MESSAGE {
            return Err(ProtocolError::InvalidInstruction(
                "memo message too long".into(),
            ));
        }
        if let Some(ref r) = self.reference {
            if r.len() > MAX_MEMO_REFERENCE {
                return Err(ProtocolError::InvalidInstruction(
                    "memo reference too long".into(),
                ));
            }
        }
        if let Some(ref m) = self.metadata {
            if m.len() > MAX_MEMO_METADATA {
                return Err(ProtocolError::InvalidInstruction(
                    "memo metadata too long".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Single payment entry for batch transfers
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaymentEntry {
    pub to: Address,
    pub amount: Balance,
    pub memo: Option<PaymentMemo>,
}

/// Agent payment instruction
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentPayment {
    pub agent_id: u64,
    pub asset_id: AssetId,
    pub to: Address,
    pub amount: Balance,
}

/// Compliance status for UpdateCompliance instruction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ComplianceStatus {
    #[default]
    Clear,
    UnderReview,
    Flagged,
    Restricted,
}

/// Result of executing a single instruction
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum InstructionResult {
    Success,
    Reverted { reason: String },
}
