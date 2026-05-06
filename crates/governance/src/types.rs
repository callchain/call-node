use call_primitives::{Address, AssetId, Balance, ValidatorId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Proposal type variants (per spec §13.3)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProposalType {
    ParameterChange {
        param_id: String,
        new_value: String,
    },
    ProtocolUpgrade {
        activation_block: u64,
        changelog: String,
    },
    TreasurySpend {
        recipient: Address,
        amount: Balance,
        asset_id: AssetId,
    },
    ValidatorSlash {
        validator_id: ValidatorId,
        reason: String,
    },
    ComplianceUpdate {
        asset_id: AssetId,
        new_policy: u8,
    },
    EmergencyPause {
        reason: String,
    },
    FeeCurrencyAdd {
        asset_id: AssetId,
        name: String,
        oracle_price_key: String,
    },
    FeeCurrencyRemove {
        asset_id: AssetId,
        grace_period_blocks: u64,
    },
    FeeCurrencyCap {
        new_cap_bps: u32,
    },
    /// Rotate a validator's signing key (per spec §12.7)
    /// The old key must sign the rotation request to prove ownership.
    ValidatorKeyRotation {
        validator_id: ValidatorId,
        old_pubkey: [u8; 32],
        new_pubkey: [u8; 32],
        /// secp256k1 signature from old key: sign(hash(old_pubkey || new_pubkey))
        signature: Vec<u8>,
    },
}

impl ProposalType {
    /// Returns true if this proposal type requires validator voting (1=1)
    pub fn is_validator_proposal(&self) -> bool {
        matches!(
            self,
            ProposalType::ParameterChange { .. }
                | ProposalType::ProtocolUpgrade { .. }
                | ProposalType::ValidatorSlash { .. }
                | ProposalType::EmergencyPause { .. }
                | ProposalType::ValidatorKeyRotation { .. }
        )
    }

    /// Returns true if this proposal type uses CALL balance voting
    pub fn is_balance_weighted(&self) -> bool {
        matches!(self, ProposalType::TreasurySpend { .. })
    }

    /// Returns true if this proposal type uses joint issuer+validator voting
    pub fn is_joint_voting(&self) -> bool {
        matches!(self, ProposalType::ComplianceUpdate { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalState {
    Pending,
    Active,
    Passed,
    Defeated,
    Queued,
    Executed,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vote {
    Yes,
    No,
    Abstain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: u64,
    pub proposer: Address,
    pub proposal_type: ProposalType,
    pub title: String,
    pub description: String,
    pub voting_power_yes: Balance,
    pub voting_power_no: Balance,
    pub voting_power_abstain: Balance,
    pub start_block: u64,
    pub end_block: u64,
    pub execution_block: Option<u64>,
    pub state: ProposalState,
    pub quorum_required: Balance,
    pub execution_data: Vec<u8>,
    pub deposit: Balance,
}

impl Proposal {
    /// Total voting power cast
    pub fn total_voted(&self) -> Balance {
        self.voting_power_yes
            .saturating_add(self.voting_power_no)
            .saturating_add(self.voting_power_abstain)
    }

    /// Check if the proposal has passed quorum
    pub fn has_quorum(&self) -> bool {
        self.total_voted() >= self.quorum_required
    }

    /// Check if the proposal has more yes than no
    pub fn is_majority_yes(&self) -> bool {
        self.voting_power_yes > self.voting_power_no
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteDelegation {
    pub delegator: Address,
    pub delegate: Address,
    pub amount: Balance,
    pub expires_at: u64, // block height
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmergencyPauseState {
    pub is_paused: bool,
    pub pause_reason: String,
    pub pause_signatures: HashMap<Address, bool>, // validator_id → signed
}

/// Events emitted during proposal state machine advancement.
/// Drained each block and forwarded to WebSocket subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GovernanceEvent {
    ProposalAdvanced {
        id: u64,
        from: ProposalState,
        to: ProposalState,
    },
    ProposalExecuted {
        id: u64,
        proposal_type: String,
    },
    ProposalExpired {
        id: u64,
    },
    ProposalDefeated {
        id: u64,
    },
}

/// A protocol upgrade scheduled by a governance proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledUpgrade {
    pub proposal_id: u64,
    pub activation_block: u64,
    pub changelog: String,
    pub applied: bool,
}

/// A fee currency registered via governance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeCurrencyEntry {
    pub name: String,
    pub oracle_price_key: String,
    pub added_at_block: u64,
}
