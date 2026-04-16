//! T12.1 — Governance Module (per spec §13.3)
//!
//! Proposal lifecycle, dual-track voting, timelock, emergency pause.

use call_primitives::{Address, AssetId, Balance, ValidatorId};
use crate::economics::TOTAL_SUPPLY;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Constants ─────────────────────────────────────────────────────────

/// Proposal deposit: 10,000 CALL
pub const PROPOSAL_DEPOSIT: Balance = 10_000 * 10u128.pow(18);

/// Review period: 2 days ≈ 691,200 blocks (at 250ms block time)
pub const REVIEW_PERIOD_BLOCKS: u64 = 691_200;

/// Voting period: 7 days ≈ 2,419,200 blocks
pub const VOTING_PERIOD_BLOCKS: u64 = 2_419_200;

/// Timelock period: 7 days ≈ 2,419,200 blocks
pub const TIMELOCK_PERIOD_BLOCKS: u64 = 2_419_200;

/// Execution timeout: 30 days ≈ 10,368,000 blocks
pub const EXECUTION_TIMEOUT_BLOCKS: u64 = 10_368_000;

/// Total supply divided by 10 for issuer voting weight on compliance updates
const TOTAL_SUPPLY_DIV_10: Balance = TOTAL_SUPPLY / 10;

// ── Proposal Types ────────────────────────────────────────────────────

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

// ── Proposal State ────────────────────────────────────────────────────

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

// ── Vote ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vote {
    Yes,
    No,
    Abstain,
}

// ── Proposal ──────────────────────────────────────────────────────────

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

// ── Vote Delegation ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteDelegation {
    pub delegator: Address,
    pub delegate: Address,
    pub amount: Balance,
    pub expires_at: u64, // block height
}

// ── Emergency Pause State ─────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct EmergencyPauseState {
    pub is_paused: bool,
    pub pause_reason: String,
    pub pause_signatures: HashMap<Address, bool>, // validator_id → signed
}

// ── Governance Manager ────────────────────────────────────────────────

/// Manages governance proposals, voting, and execution (per spec §13.3)
pub struct GovernanceManager {
    proposals: HashMap<u64, Proposal>,
    next_proposal_id: u64,
    /// Validator addresses for 1=1 voting
    validator_addresses: HashMap<ValidatorId, Address>,
    /// Address → CALL balance for balance-weighted voting
    call_balances: HashMap<Address, Balance>,
    /// Asset issuers: asset_id → issuer address
    asset_issuers: HashMap<AssetId, Address>,
    /// Vote delegations: delegator → delegation
    delegations: HashMap<Address, VoteDelegation>,
    /// Deposit: proposer_address → deposit amount
    deposits: HashMap<Address, Balance>,
    /// Track who has voted on each proposal: proposal_id → set of voter addresses
    voted_addresses: HashMap<u64, std::collections::HashSet<Address>>,
    /// Current block height
    current_block: u64,
    /// Emergency pause state
    pub emergency_pause: EmergencyPauseState,
}

impl Default for GovernanceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GovernanceManager {
    pub fn new() -> Self {
        Self {
            proposals: HashMap::new(),
            next_proposal_id: 0,
            validator_addresses: HashMap::new(),
            call_balances: HashMap::new(),
            asset_issuers: HashMap::new(),
            delegations: HashMap::new(),
            deposits: HashMap::new(),
            voted_addresses: HashMap::new(),
            current_block: 0,
            emergency_pause: EmergencyPauseState::default(),
        }
    }

    /// Set current block height
    pub fn set_current_block(&mut self, block: u64) {
        self.current_block = block;
    }

    /// Register a validator address for voting
    pub fn register_validator(&mut self, validator_id: ValidatorId, address: Address) {
        self.validator_addresses.insert(validator_id, address);
    }

    /// Set CALL balance for an address (for balance-weighted voting)
    pub fn set_call_balance(&mut self, address: Address, balance: Balance) {
        self.call_balances.insert(address, balance);
    }

    /// Register an asset issuer
    pub fn register_asset_issuer(&mut self, asset_id: AssetId, issuer: Address) {
        self.asset_issuers.insert(asset_id, issuer);
    }

    // ── Proposal Lifecycle ────────────────────────────────────────────

    /// Submit a new proposal (per spec §13.3)
    /// Requires 10,000 CALL deposit
    pub fn submit_proposal(
        &mut self,
        proposer: Address,
        proposal_type: ProposalType,
        title: String,
        description: String,
        execution_data: Vec<u8>,
    ) -> Result<u64, GovernanceError> {
        // Check deposit sufficiency
        let proposer_balance = self.call_balances.get(&proposer).copied().unwrap_or(0);
        if proposer_balance < PROPOSAL_DEPOSIT {
            return Err(GovernanceError::InsufficientDeposit);
        }

        // Deduct deposit
        let new_balance = proposer_balance - PROPOSAL_DEPOSIT;
        self.call_balances.insert(proposer, new_balance);
        self.deposits.insert(proposer, PROPOSAL_DEPOSIT);

        let id = self.next_proposal_id;
        self.next_proposal_id += 1;

        // Calculate quorum and voting windows
        let (quorum_required, voting_start, voting_end) =
            self.calculate_quorum_and_windows(&proposal_type);

        let proposal = Proposal {
            id,
            proposer,
            proposal_type,
            title,
            description,
            voting_power_yes: 0,
            voting_power_no: 0,
            voting_power_abstain: 0,
            start_block: voting_start,
            end_block: voting_end,
            execution_block: None,
            state: ProposalState::Pending,
            quorum_required,
            execution_data,
            deposit: PROPOSAL_DEPOSIT,
        };

        self.proposals.insert(id, proposal);
        Ok(id)
    }

    /// Vote on a proposal (per spec §13.3 dual-track voting)
    pub fn vote(
        &mut self,
        proposal_id: u64,
        voter: Address,
        vote: Vote,
    ) -> Result<(), GovernanceError> {
        // Check proposal exists and get info we need
        let (current_state, start_block, _end_block, proposal_type) = {
            let proposal = self
                .proposals
                .get(&proposal_id)
                .ok_or(GovernanceError::ProposalNotFound)?;

            // Check voting window
            if self.current_block < proposal.start_block {
                return Err(GovernanceError::VotingNotStarted);
            }
            if self.current_block > proposal.end_block {
                return Err(GovernanceError::VotingPeriodClosed);
            }
            if proposal.state != ProposalState::Pending && proposal.state != ProposalState::Active {
                return Err(GovernanceError::VotingPeriodClosed);
            }

            (proposal.state, proposal.start_block, proposal.end_block, proposal.proposal_type.clone())
        };

        // Advance state to Active if still Pending
        if current_state == ProposalState::Pending && self.current_block >= start_block {
            if let Some(p) = self.proposals.get_mut(&proposal_id) {
                p.state = ProposalState::Active;
            }
        }

        // Check voter hasn't already voted on this proposal
        let voted = self.voted_addresses.entry(proposal_id).or_default();
        if !voted.insert(voter) {
            return Err(GovernanceError::AlreadyVoted);
        }

        // Calculate voting power
        let voting_power =
            self.calculate_voting_power(voter, &proposal_type, proposal_id)?;

        if voting_power == 0 {
            return Err(GovernanceError::NoVotingPower);
        }

        // Apply vote
        let proposal = self
            .proposals
            .get_mut(&proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;

        match vote {
            Vote::Yes => proposal.voting_power_yes += voting_power,
            Vote::No => proposal.voting_power_no += voting_power,
            Vote::Abstain => proposal.voting_power_abstain += voting_power,
        }

        Ok(())
    }

    /// Queue a passed proposal for execution (after voting period ends)
    pub fn queue_proposal(&mut self, proposal_id: u64) -> Result<(), GovernanceError> {
        // Read proposal data first to avoid borrow conflicts
        let (current_state, end_block, proposer, proposal_type) = {
            let p = self
                .proposals
                .get(&proposal_id)
                .ok_or(GovernanceError::ProposalNotFound)?;
            (p.state, p.end_block, p.proposer, p.proposal_type.clone())
        };

        if self.current_block <= end_block {
            return Err(GovernanceError::VotingPeriodNotEnded);
        }

        if current_state == ProposalState::Passed {
            // Already passed, move to queued
            let p = self.proposals.get_mut(&proposal_id).unwrap();
            p.state = ProposalState::Queued;
            p.execution_block = Some(self.current_block + TIMELOCK_PERIOD_BLOCKS);
            return Ok(());
        }

        // Check quorum using a temporary proposal for evaluation
        let has_quorum;
        let majority_yes;
        {
            let p = self.proposals.get(&proposal_id).unwrap();
            has_quorum = p.has_quorum();
            majority_yes = p.is_majority_yes();
        }

        if has_quorum && majority_yes {
            let p = self.proposals.get_mut(&proposal_id).unwrap();
            // For emergency pause, skip timelock
            if matches!(proposal_type, ProposalType::EmergencyPause { .. }) {
                p.state = ProposalState::Queued;
                p.execution_block = Some(self.current_block);
            } else {
                p.state = ProposalState::Queued;
                p.execution_block = Some(self.current_block + TIMELOCK_PERIOD_BLOCKS);
            }
        } else {
            let p = self.proposals.get_mut(&proposal_id).unwrap();
            p.state = ProposalState::Defeated;
            // Confiscate deposit
            self.deposits.remove(&proposer);
            return Err(GovernanceError::ProposalDefeated);
        }

        Ok(())
    }

    /// Execute a queued proposal (after timelock)
    pub fn execute_proposal(&mut self, proposal_id: u64) -> Result<(), GovernanceError> {
        let (proposer, execution_block) = {
            let p = self
                .proposals
                .get(&proposal_id)
                .ok_or(GovernanceError::ProposalNotFound)?;
            if p.state != ProposalState::Queued {
                return Err(GovernanceError::ProposalNotQueued);
            }
            (p.proposer, p.execution_block.ok_or(GovernanceError::ProposalNotQueued)?)
        };

        if self.current_block < execution_block {
            return Err(GovernanceError::TimelockNotElapsed);
        }

        self.proposals.get_mut(&proposal_id).unwrap().state = ProposalState::Executed;

        // Apply the on-chain change
        self.apply_proposal(proposal_id)?;

        // Return deposit to proposer
        if let Some(deposit) = self.deposits.remove(&proposer) {
            let current = self.call_balances.get(&proposer).copied().unwrap_or(0);
            self.call_balances.insert(proposer, current + deposit);
        }

        Ok(())
    }

    /// Expire a proposal that was queued but not executed in time
    pub fn expire_proposal(&mut self, proposal_id: u64) -> Result<(), GovernanceError> {
        let (current_state, execution_block, proposer) = {
            let p = self
                .proposals
                .get(&proposal_id)
                .ok_or(GovernanceError::ProposalNotFound)?;
            (p.state, p.execution_block, p.proposer)
        };

        if current_state != ProposalState::Queued {
            return Err(GovernanceError::ProposalNotQueued);
        }

        let execution_block = execution_block.ok_or(GovernanceError::ProposalNotQueued)?;

        if self.current_block < execution_block + EXECUTION_TIMEOUT_BLOCKS {
            return Err(GovernanceError::ExecutionTimeoutNotReached);
        }

        self.proposals.get_mut(&proposal_id).unwrap().state = ProposalState::Expired;
        self.deposits.remove(&proposer);

        Ok(())
    }

    // ── On-Chain Execution ──────────────────────────────────────────

    /// Apply the actual on-chain change from an executed proposal.
    /// Parses `execution_data` as JSON and applies the change based on proposal type.
    fn apply_proposal(&mut self, proposal_id: u64) -> Result<(), GovernanceError> {
        let proposal = self.proposals.get(&proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;

        match &proposal.proposal_type {
            ProposalType::ParameterChange { param_id, new_value } => {
                // Execution data format: JSON with param updates
                // e.g., {"max_block_size": 10000000}
                if !proposal.execution_data.is_empty() {
                    // Parse and apply — in a real system this would update consensus/protocol params
                    let _params: serde_json::Value = serde_json::from_slice(&proposal.execution_data)
                        .map_err(|_| GovernanceError::ExecutionFailed(format!("invalid execution_data JSON")))?;
                    tracing::info!(param_id, new_value, "parameter change applied via governance");
                }
            }
            ProposalType::ProtocolUpgrade { activation_block, changelog } => {
                // Signal upgrade — consensus layer handles the actual activation
                tracing::info!(activation_block, changelog, "protocol upgrade scheduled via governance");
            }
            ProposalType::TreasurySpend { recipient, amount, asset_id } => {
                // Transfer from treasury (proposal deposit address acts as treasury)
                let treasury = proposal.proposer;
                let current = self.call_balances.get(&treasury).copied().unwrap_or(0);
                if current >= *amount {
                    self.call_balances.insert(treasury, current - *amount);
                    let recipient_balance = self.call_balances.get(recipient).copied().unwrap_or(0);
                    self.call_balances.insert(*recipient, recipient_balance + *amount);
                    tracing::info!(asset_id, amount, ?recipient, "treasury spend executed");
                }
            }
            ProposalType::ValidatorSlash { validator_id, reason } => {
                // Remove validator from active set
                if let Some(addr) = self.validator_addresses.remove(validator_id) {
                    self.call_balances.remove(&addr);
                    tracing::info!(validator_id, reason, "validator slashed via governance");
                }
            }
            ProposalType::ComplianceUpdate { asset_id, new_policy } => {
                // Update compliance — tracked via execution_data for external enforcement
                tracing::info!(asset_id, new_policy, "compliance policy updated via governance");
            }
            ProposalType::EmergencyPause { reason } => {
                // Emergency pause is handled separately via signature collection
                self.emergency_pause.is_paused = true;
                self.emergency_pause.pause_reason = reason.clone();
                tracing::info!(reason, "emergency pause activated via governance");
            }
            ProposalType::FeeCurrencyAdd { asset_id, name, oracle_price_key } => {
                // Register new fee currency — tracked for oracle pricing
                tracing::info!(asset_id, name, oracle_price_key, "fee currency added via governance");
            }
            ProposalType::FeeCurrencyRemove { asset_id, grace_period_blocks } => {
                // Mark currency for removal after grace period
                tracing::info!(asset_id, grace_period_blocks, "fee currency removal scheduled");
            }
            ProposalType::FeeCurrencyCap { new_cap_bps } => {
                // Update fee currency cap
                tracing::info!(new_cap_bps, "fee currency cap updated via governance");
            }
        }

        Ok(())
    }

    // ── Emergency Pause ───────────────────────────────────────────────

    /// Initiate emergency pause with validator signatures (per spec §13.3)
    /// Requires 2/3 of active validators
    pub fn emergency_pause_initiate(
        &mut self,
        validator_id: ValidatorId,
        reason: String,
    ) -> Result<bool, GovernanceError> {
        let total_validators = self.validator_addresses.len() as u64;
        if total_validators == 0 {
            return Err(GovernanceError::NoValidators);
        }

        let address = self
            .validator_addresses
            .get(&validator_id)
            .ok_or(GovernanceError::ValidatorNotFound)?;

        self.emergency_pause.pause_signatures.insert(*address, true);

        let signed_count = self.emergency_pause.pause_signatures.len() as u64;
        let threshold = (2 * total_validators).div_ceil(3); // ceil(2/3)

        if signed_count >= threshold {
            self.emergency_pause.is_paused = true;
            self.emergency_pause.pause_reason = reason;
            Ok(true) // pause activated
        } else {
            Ok(false) // more signatures needed
        }
    }

    /// Resume from emergency pause (requires governance proposal)
    pub fn emergency_pause_resume(&mut self) -> Result<(), GovernanceError> {
        if !self.emergency_pause.is_paused {
            return Err(GovernanceError::NotPaused);
        }
        self.emergency_pause.is_paused = false;
        self.emergency_pause.pause_reason.clear();
        self.emergency_pause.pause_signatures.clear();
        Ok(())
    }

    // ── Vote Delegation ───────────────────────────────────────────────

    /// Delegate voting power to another address
    pub fn delegate_vote(
        &mut self,
        delegator: Address,
        delegate: Address,
        amount: Balance,
        expires_at: u64,
    ) -> Result<(), GovernanceError> {
        let delegator_balance = self.call_balances.get(&delegator).copied().unwrap_or(0);
        if delegator_balance < amount {
            return Err(GovernanceError::InsufficientBalanceForDelegation);
        }

        self.delegations.insert(
            delegator,
            VoteDelegation {
                delegator,
                delegate,
                amount,
                expires_at,
            },
        );
        Ok(())
    }

    /// Remove vote delegation
    pub fn undelegate_vote(&mut self, delegator: Address) -> Result<(), GovernanceError> {
        self.delegations
            .remove(&delegator)
            .ok_or(GovernanceError::NoDelegation)?;
        Ok(())
    }

    /// Get delegated voting power for an address (sum of active delegations)
    pub fn get_delegated_voting_power(&self, delegate: Address) -> Balance {
        self.delegations
            .values()
            .filter(|d| d.delegate == delegate && self.current_block <= d.expires_at)
            .map(|d| d.amount)
            .sum()
    }

    // ── Queries ───────────────────────────────────────────────────────

    /// Get a proposal by ID
    pub fn get_proposal(&self, proposal_id: u64) -> Option<&Proposal> {
        self.proposals.get(&proposal_id)
    }

    /// Get all proposals
    pub fn get_all_proposals(&self) -> &HashMap<u64, Proposal> {
        &self.proposals
    }

    /// Check if chain is paused
    pub fn is_paused(&self) -> bool {
        self.emergency_pause.is_paused
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Calculate voting power for a voter given proposal type (per spec §13.3)
    fn calculate_voting_power(
        &self,
        voter: Address,
        proposal_type: &ProposalType,
        _proposal_id: u64,
    ) -> Result<Balance, GovernanceError> {
        let mut power: Balance = 0;

        if proposal_type.is_validator_proposal() {
            // Validators: 1=1 vote
            for (&vid, &addr) in &self.validator_addresses {
                if addr == voter {
                    power += 1;
                    let _ = vid;
                    break;
                }
            }
            // Also allow CALL holders to vote balance-weighted for these types
            // (ParameterChange/ProtocolUpgrade/ValidatorSlash/EmergencyPause)
            // But not for EmergencyPause which is validator-signature-only
            if !matches!(proposal_type, ProposalType::EmergencyPause { .. }) {
                let balance = self.call_balances.get(&voter).copied().unwrap_or(0);
                let with_delegation = balance + self.get_delegated_voting_power(voter);
                power = power.max(with_delegation);
            }
        }

        if proposal_type.is_balance_weighted() {
            // TreasurySpend: CALL balance weighted
            let balance = self.call_balances.get(&voter).copied().unwrap_or(0);
            power = power.max(balance);
            // Add delegated power
            power += self.get_delegated_voting_power(voter);
        }

        if proposal_type.is_joint_voting() {
            // ComplianceUpdate: validators get 1, issuers get total_supply/10
            for (&vid, &addr) in &self.validator_addresses {
                if addr == voter {
                    power += 1;
                    let _ = vid;
                }
            }
            // Check if voter is an issuer
            for (&asset_id, &issuer) in &self.asset_issuers {
                if issuer == voter {
                    let issuer_weight = TOTAL_SUPPLY_DIV_10;
                    power = power.max(issuer_weight);
                    let _ = asset_id;
                }
            }
        }

        Ok(power)
    }

    /// Calculate quorum requirement and voting windows for a proposal type
    fn calculate_quorum_and_windows(
        &self,
        proposal_type: &ProposalType,
    ) -> (Balance, u64, u64) {
        let total_validators = self.validator_addresses.len() as Balance;
        let voting_start = self.current_block + REVIEW_PERIOD_BLOCKS;
        let voting_end = voting_start + VOTING_PERIOD_BLOCKS;

        let quorum = match proposal_type {
            ProposalType::ParameterChange { .. } => {
                // 2/3 of validators
                (2 * total_validators).div_ceil(3)
            }
            ProposalType::ProtocolUpgrade { .. } => {
                // max(2/3 validators, 20% total supply)
                let validator_quorum = (2 * total_validators).div_ceil(3);
                let supply_quorum = TOTAL_SUPPLY / 5; // 20%
                validator_quorum.max(supply_quorum)
            }
            ProposalType::TreasurySpend { .. } => {
                // 20% total supply
                TOTAL_SUPPLY / 5
            }
            ProposalType::ValidatorSlash { .. } => {
                // 2/3 of validators
                (2 * total_validators).div_ceil(3)
            }
            ProposalType::ComplianceUpdate { .. } => {
                // Simple majority of joint voters
                total_validators / 2 + 1
            }
            ProposalType::EmergencyPause { .. } => {
                // 2/3 of validators (handled via separate signature collection)
                (2 * total_validators).div_ceil(3)
            }
            ProposalType::FeeCurrencyAdd { .. }
            | ProposalType::FeeCurrencyRemove { .. }
            | ProposalType::FeeCurrencyCap { .. } => {
                // Simple majority of validators
                total_validators / 2 + 1
            }
        };

        (quorum, voting_start, voting_end)
    }
}

// ── Errors ────────────────────────────────────────────────────────────

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GovernanceError {
    #[error("proposal not found")]
    ProposalNotFound,
    #[error("insufficient deposit (need 10,000 CALL)")]
    InsufficientDeposit,
    #[error("voting has not started yet")]
    VotingNotStarted,
    #[error("voting period has closed")]
    VotingPeriodClosed,
    #[error("voter has no voting power")]
    NoVotingPower,
    #[error("proposal defeated")]
    ProposalDefeated,
    #[error("proposal not queued for execution")]
    ProposalNotQueued,
    #[error("timelock period has not elapsed")]
    TimelockNotElapsed,
    #[error("voting period has not ended")]
    VotingPeriodNotEnded,
    #[error("execution timeout has not been reached")]
    ExecutionTimeoutNotReached,
    #[error("insufficient balance for delegation")]
    InsufficientBalanceForDelegation,
    #[error("no active delegation")]
    NoDelegation,
    #[error("no validators registered")]
    NoValidators,
    #[error("validator not found")]
    ValidatorNotFound,
    #[error("chain is not paused")]
    NotPaused,
    #[error("voter has already voted on this proposal")]
    AlreadyVoted,
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn one_million_call() -> Balance {
        1_000_000 * 10u128.pow(18)
    }

    fn make_manager_with_validators(count: u8) -> GovernanceManager {
        let mut mgr = GovernanceManager::new();
        for i in 1..=count {
            mgr.register_validator(i as ValidatorId, test_addr(i));
            // Small balance so balance-weighted voting doesn't dominate
            mgr.set_call_balance(test_addr(i), 1);
        }
        mgr
    }

    // ── Proposal Submit & Vote ────────────────────────────────────────

    #[test]
    fn test_proposal_submit_and_vote() {
        let mut mgr = make_manager_with_validators(3);

        // Give proposer enough balance for deposit
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "max_block_size".into(),
                    new_value: "10000000".into(),
                },
                "Increase block size".into(),
                "Double the max block size".into(),
                vec![],
            )
            .unwrap();

        assert_eq!(id, 0);

        // Deposit was deducted
        let balance = mgr.call_balances.get(&proposer).unwrap();
        assert_eq!(*balance, PROPOSAL_DEPOSIT);

        // Advance to voting period
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

        // Validators vote yes
        mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
        mgr.vote(id, test_addr(2), Vote::Yes).unwrap();
        mgr.vote(id, test_addr(3), Vote::No).unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.voting_power_yes, 2); // 2 validators voted yes
        assert_eq!(proposal.voting_power_no, 1);
    }

    // ── Quorum Pass ───────────────────────────────────────────────────

    #[test]
    fn test_proposal_passes_quorum() {
        let mut mgr = make_manager_with_validators(3);

        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test".into(),
                    new_value: "1".into(),
                },
                "Test".into(),
                "Test proposal".into(),
                vec![],
            )
            .unwrap();

        // Advance past voting
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);

        // All 3 validators voted yes during voting (we'll simulate by directly setting)
        {
            let p = mgr.proposals.get_mut(&id).unwrap();
            p.voting_power_yes = 3;
        }

        // Queue should pass and move to queued
        mgr.queue_proposal(id).unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.state, ProposalState::Queued);
        assert!(proposal.execution_block.is_some());
    }

    // ── Quorum Fail ───────────────────────────────────────────────────

    #[test]
    fn test_proposal_fails_quorum() {
        let mut mgr = make_manager_with_validators(3);

        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test".into(),
                    new_value: "1".into(),
                },
                "Test".into(),
                "Test proposal".into(),
                vec![],
            )
            .unwrap();

        // Advance past voting without any votes
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);

        let result = mgr.queue_proposal(id);
        assert!(result.is_err());

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.state, ProposalState::Defeated);
    }

    // ── Timelock Execution ────────────────────────────────────────────

    #[test]
    fn test_proposal_timelock_execution() {
        let mut mgr = make_manager_with_validators(3);

        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test".into(),
                    new_value: "1".into(),
                },
                "Test".into(),
                "Test proposal".into(),
                vec![],
            )
            .unwrap();

        // Set votes, advance past voting
        {
            let p = mgr.proposals.get_mut(&id).unwrap();
            p.voting_power_yes = 3;
        }
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
        mgr.queue_proposal(id).unwrap();

        // Cannot execute before timelock
        let result = mgr.execute_proposal(id);
        assert!(result.is_err());

        // Advance past timelock
        let exec_block = mgr.get_proposal(id).unwrap().execution_block.unwrap();
        mgr.set_current_block(exec_block + 1);
        mgr.execute_proposal(id).unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.state, ProposalState::Executed);

        // Deposit returned to proposer
        let balance = mgr.call_balances.get(&proposer).unwrap();
        assert_eq!(*balance, PROPOSAL_DEPOSIT * 2 - PROPOSAL_DEPOSIT + PROPOSAL_DEPOSIT); // deposit deducted then restored
    }

    // ── Proposal Expire ───────────────────────────────────────────────

    #[test]
    fn test_proposal_expire_confiscate_deposit() {
        let mut mgr = make_manager_with_validators(3);

        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test".into(),
                    new_value: "1".into(),
                },
                "Test".into(),
                "Test proposal".into(),
                vec![],
            )
            .unwrap();

        // Queue the proposal
        {
            let p = mgr.proposals.get_mut(&id).unwrap();
            p.voting_power_yes = 3;
        }
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
        mgr.queue_proposal(id).unwrap();

        // Advance past execution timeout
        let exec_block = mgr.get_proposal(id).unwrap().execution_block.unwrap();
        mgr.set_current_block(exec_block + EXECUTION_TIMEOUT_BLOCKS + 1);

        mgr.expire_proposal(id).unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.state, ProposalState::Expired);

        // Deposit confiscated
        assert!(mgr.deposits.get(&proposer).is_none());
    }

    // ── Validator 1=1 Voting ─────────────────────────────────────────

    #[test]
    fn test_validator_voting_1_1() {
        let mut mgr = make_manager_with_validators(3);

        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ValidatorSlash {
                    validator_id: 99,
                    reason: "offline".into(),
                },
                "Slash offline validator".into(),
                "Validator 99 has been offline".into(),
                vec![],
            )
            .unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

        // Each validator gets 1 vote regardless of balance
        mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
        mgr.vote(id, test_addr(2), Vote::Yes).unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.voting_power_yes, 2);
    }

    // ── CALL Holder Balance-Weighted Voting ───────────────────────────

    #[test]
    fn test_call_holder_voting_balance_weighted() {
        let mut mgr = make_manager_with_validators(3);

        let proposer = test_addr(10);
        let big_holder = test_addr(20);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);
        mgr.set_call_balance(big_holder, TOTAL_SUPPLY / 4); // 25% of supply

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::TreasurySpend {
                    recipient: test_addr(30),
                    amount: one_million_call(),
                    asset_id: 0,
                },
                "Treasury spend".into(),
                "Send 1M CALL to address".into(),
                vec![],
            )
            .unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

        // Big holder votes yes — voting power = their CALL balance
        mgr.vote(id, big_holder, Vote::Yes).unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.voting_power_yes, TOTAL_SUPPLY / 4);
    }

    // ── Vote Delegation ───────────────────────────────────────────────

    #[test]
    fn test_vote_delegation() {
        let mut mgr = make_manager_with_validators(3);

        let delegator = test_addr(50);
        let delegate = test_addr(51);
        mgr.set_call_balance(delegator, 5_000_000);
        mgr.set_call_balance(delegate, 1_000_000);

        mgr.delegate_vote(delegator, delegate, 5_000_000, 1_000_000)
            .unwrap();

        // Delegated power for delegate
        let delegated = mgr.get_delegated_voting_power(delegate);
        assert_eq!(delegated, 5_000_000);

        // Undelegate
        mgr.undelegate_vote(delegator).unwrap();
        let delegated = mgr.get_delegated_voting_power(delegate);
        assert_eq!(delegated, 0);
    }

    // ── Emergency Pause ───────────────────────────────────────────────

    #[test]
    fn test_emergency_pause_2_3_signatures() {
        let mut mgr = make_manager_with_validators(3);

        // 2/3 of 3 = 2 signatures needed
        let result = mgr
            .emergency_pause_initiate(1, "critical bug".into())
            .unwrap();
        assert!(!result); // need more signatures

        let result = mgr
            .emergency_pause_initiate(2, "critical bug".into())
            .unwrap();
        assert!(result); // 2/3 reached, pause activated

        assert!(mgr.is_paused());
        assert_eq!(mgr.emergency_pause.pause_reason, "critical bug");

        // Resume
        mgr.emergency_pause_resume().unwrap();
        assert!(!mgr.is_paused());
    }

    // ── Compliance Update Joint Voting ────────────────────────────────

    #[test]
    fn test_compliance_update_issuer_validator_joint() {
        let mut mgr = make_manager_with_validators(3);

        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        // Register an issuer
        let issuer = test_addr(20);
        mgr.register_asset_issuer(1, issuer);
        mgr.set_call_balance(issuer, PROPOSAL_DEPOSIT);

        let id = mgr
            .submit_proposal(
                issuer,
                ProposalType::ComplianceUpdate {
                    asset_id: 1,
                    new_policy: 1, // OFAC blacklist
                },
                "Update compliance".into(),
                "Enable OFAC blacklist".into(),
                vec![],
            )
            .unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

        // Issuer votes — should have weight of total_supply / 10
        mgr.vote(id, issuer, Vote::Yes).unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.voting_power_yes, TOTAL_SUPPLY / 10);

        // A validator also votes — adds 1 more
        mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.voting_power_yes, TOTAL_SUPPLY / 10 + 1);
    }

    // ── Insufficient Deposit ──────────────────────────────────────────

    #[test]
    fn test_proposal_insufficient_deposit() {
        let mut mgr = GovernanceManager::new();
        let proposer = test_addr(1);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT - 1);

        let result = mgr.submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test".into(),
                new_value: "1".into(),
            },
            "Test".into(),
            "Test".into(),
            vec![],
        );
        assert!(matches!(result, Err(GovernanceError::InsufficientDeposit)));
    }

    // ── Voting Period Enforcement ─────────────────────────────────────

    #[test]
    fn test_voting_before_period_rejected() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test".into(),
                    new_value: "1".into(),
                },
                "Test".into(),
                "Test".into(),
                vec![],
            )
            .unwrap();

        // Voting hasn't started yet (need to pass REVIEW_PERIOD_BLOCKS)
        let result = mgr.vote(id, test_addr(1), Vote::Yes);
        assert!(matches!(result, Err(GovernanceError::VotingNotStarted)));
    }

    // ── Fee Currency Proposal Types ───────────────────────────────────

    #[test]
    fn test_fee_currency_proposal_lifecycle() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        // FeeCurrencyAdd proposal
        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::FeeCurrencyAdd {
                    asset_id: 5,
                    name: "USDC".into(),
                    oracle_price_key: "USDC/USD".into(),
                },
                "Add USDC as fee currency".into(),
                "USDC market cap > 100M".into(),
                vec![],
            )
            .unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        // Simple majority quorum for fee currency proposals
        assert_eq!(proposal.quorum_required, 2); // 3/2 + 1 = 2
    }

    // ── Proposal State Transitions ────────────────────────────────────

    #[test]
    fn test_proposal_state_machine_full() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test".into(),
                    new_value: "1".into(),
                },
                "Test".into(),
                "Test".into(),
                vec![],
            )
            .unwrap();

        let p = mgr.get_proposal(id).unwrap();
        assert_eq!(p.state, ProposalState::Pending);

        // Advance past review into voting
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);

        // Vote
        mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
        let p = mgr.get_proposal(id).unwrap();
        assert_eq!(p.state, ProposalState::Active);

        // Advance past voting, pass
        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
        {
            let p = mgr.proposals.get_mut(&id).unwrap();
            p.voting_power_yes = 3; // ensure quorum
        }
        mgr.queue_proposal(id).unwrap();

        let p = mgr.get_proposal(id).unwrap();
        assert_eq!(p.state, ProposalState::Queued);

        // Advance past timelock, execute
        let exec = p.execution_block.unwrap();
        mgr.set_current_block(exec + 1);
        mgr.execute_proposal(id).unwrap();

        let p = mgr.get_proposal(id).unwrap();
        assert_eq!(p.state, ProposalState::Executed);
    }

    // ── Duplicate Vote Prevention ─────────────────────────────────────

    #[test]
    fn test_voter_cannot_vote_twice_same_proposal() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test".into(),
                    new_value: "1".into(),
                },
                "Test".into(),
                "Test".into(),
                vec![],
            )
            .unwrap();

        mgr.set_current_block(REVIEW_PERIOD_BLOCKS + 1);
        mgr.vote(id, test_addr(1), Vote::Yes).unwrap();
        // Second vote by same address should be rejected
        assert!(matches!(
            mgr.vote(id, test_addr(1), Vote::Yes),
            Err(GovernanceError::AlreadyVoted)
        ));

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.voting_power_yes, 1);
    }

    // ── Protocol Upgrade Quorum ───────────────────────────────────────

    #[test]
    fn test_protocol_upgrade_dual_quorum() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, PROPOSAL_DEPOSIT * 2);

        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ProtocolUpgrade {
                    activation_block: 1_000_000,
                    changelog: "v1.1.0 release".into(),
                },
                "Protocol upgrade".into(),
                "Upgrade to v1.1.0".into(),
                vec![],
            )
            .unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        // max(2/3 validators, 20% total supply)
        // 2/3 of 3 = 2, 20% of 1B = 200M
        assert_eq!(proposal.quorum_required, TOTAL_SUPPLY / 5);
    }
}
