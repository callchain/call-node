use call_primitives::{Address, AssetId, Balance, ValidatorId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::{types::*, config::*, error::*};

/// External balance source for real on-chain balances.
/// When set, deposit checks and voting power use this instead of `call_balances`.
pub type BalanceSource = Arc<dyn Fn(Address) -> Balance + Send + Sync>;

/// Trait for executing real on-chain changes when a governance proposal passes.
/// Implement this in the node layer to dispatch to consensus, fork manager, etc.
pub trait ProposalExecutor: Send + Sync {
    /// Execute the on-chain effect of a proposal. Called after state is updated.
    fn on_proposal_executed(&self, proposal: &Proposal) -> Result<(), String>;
}

/// Manages governance proposals, voting, and execution (per spec §13.3)
#[derive(Serialize, Deserialize)]
pub struct GovernanceManager {
    pub(crate) proposals: HashMap<u64, Proposal>,
    pub(crate) next_proposal_id: u64,
    /// Validator addresses for 1=1 voting
    pub(crate) validator_addresses: HashMap<ValidatorId, Address>,
    /// Address → CALL balance for balance-weighted voting
    pub(crate) call_balances: HashMap<Address, Balance>,
    /// Asset issuers: asset_id → issuer address
    pub(crate) asset_issuers: HashMap<AssetId, Address>,
    /// Vote delegations: delegator → delegation
    pub(crate) delegations: HashMap<Address, VoteDelegation>,
    /// Deposit: proposer_address → deposit amount
    pub(crate) deposits: HashMap<Address, Balance>,
    /// Track who has voted on each proposal: proposal_id → set of voter addresses
    pub(crate) voted_addresses: HashMap<u64, std::collections::HashSet<Address>>,
    /// Rate limiting: address → last submission block height
    #[serde(skip)]
    pub(crate) last_submission_block: HashMap<Address, u64>,
    /// Current block height
    pub(crate) current_block: u64,
    /// Emergency pause state
    pub emergency_pause: EmergencyPauseState,
    /// Governance configuration (quorum thresholds, periods)
    pub config: GovernanceConfig,
    /// Events emitted during state advancement (not serialized — drained each block)
    #[serde(skip)]
    pub(crate) events: Vec<GovernanceEvent>,
    /// Optional executor for real on-chain side effects (not serialized — rewired after load)
    #[serde(skip)]
    pub executor: Option<Arc<dyn ProposalExecutor>>,
    /// Optional external balance source for real on-chain balances (not serialized — rewired after load)
    #[serde(skip)]
    pub balance_source: Option<BalanceSource>,
    /// Scheduled protocol upgrades from governance proposals
    #[serde(default)]
    pub scheduled_upgrades: Vec<ScheduledUpgrade>,
    /// Compliance policies: asset_id -> policy_id
    #[serde(default)]
    pub compliance_policies: HashMap<AssetId, u8>,
    /// Fee currencies: asset_id -> entry
    #[serde(default)]
    pub fee_currencies: HashMap<AssetId, FeeCurrencyEntry>,
    /// Fee currencies pending removal: asset_id -> grace_period_end_block
    #[serde(default)]
    pub fee_currencies_pending_removal: HashMap<AssetId, u64>,
    /// Fee currency cap in basis points
    #[serde(default = "crate::types::default_fee_currency_cap")]
    pub fee_currency_cap_bps: u32,
    /// Validator public keys: validator_id -> pubkey
    #[serde(default)]
    pub validator_pubkeys: HashMap<ValidatorId, [u8; 32]>,
}

impl std::fmt::Debug for GovernanceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernanceManager")
            .field("proposals", &self.proposals.len())
            .field("next_proposal_id", &self.next_proposal_id)
            .field("validator_addresses", &self.validator_addresses.len())
            .field("current_block", &self.current_block)
            .field("emergency_pause", &self.emergency_pause)
            .field("scheduled_upgrades", &self.scheduled_upgrades.len())
            .field("fee_currency_cap_bps", &self.fee_currency_cap_bps)
            .finish()
    }
}

impl Clone for GovernanceManager {
    fn clone(&self) -> Self {
        Self {
            proposals: self.proposals.clone(),
            next_proposal_id: self.next_proposal_id,
            validator_addresses: self.validator_addresses.clone(),
            call_balances: self.call_balances.clone(),
            asset_issuers: self.asset_issuers.clone(),
            delegations: self.delegations.clone(),
            deposits: self.deposits.clone(),
            voted_addresses: self.voted_addresses.clone(),
            last_submission_block: self.last_submission_block.clone(),
            current_block: self.current_block,
            emergency_pause: self.emergency_pause.clone(),
            config: self.config.clone(),
            events: Vec::new(), // events are drained each block, not preserved across clones
            executor: None,     // not clonable — rewired after load
            balance_source: None, // not clonable
            scheduled_upgrades: self.scheduled_upgrades.clone(),
            compliance_policies: self.compliance_policies.clone(),
            fee_currencies: self.fee_currencies.clone(),
            fee_currencies_pending_removal: self.fee_currencies_pending_removal.clone(),
            fee_currency_cap_bps: self.fee_currency_cap_bps,
            validator_pubkeys: self.validator_pubkeys.clone(),
        }
    }
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
            last_submission_block: HashMap::new(),
            emergency_pause: EmergencyPauseState::default(),
            config: GovernanceConfig::default(),
            events: Vec::new(),
            executor: None,
            balance_source: None,
            scheduled_upgrades: Vec::new(),
            compliance_policies: HashMap::new(),
            fee_currencies: HashMap::new(),
            fee_currencies_pending_removal: HashMap::new(),
            fee_currency_cap_bps: 10_000,
            validator_pubkeys: HashMap::new(),
        }
    }

    /// Create with custom governance configuration.
    pub fn with_config(mut self, config: GovernanceConfig) -> Self {
        self.config = config;
        self
    }

    /// Set a proposal executor for real on-chain side effects.
    pub fn with_executor(mut self, executor: Arc<dyn ProposalExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Set current block height
    pub fn set_current_block(&mut self, block: u64) {
        self.current_block = block;
    }

    /// Register a validator address for voting
    pub fn register_validator(&mut self, validator_id: ValidatorId, address: Address) {
        self.validator_addresses.insert(validator_id, address);
    }

    /// Look up validator_id by address (reverse mapping)
    pub fn validator_id_by_address(&self, address: Address) -> Option<ValidatorId> {
        self.validator_addresses
            .iter()
            .find(|(_, &a)| a == address)
            .map(|(&vid, _)| vid)
    }

    /// Set CALL balance for an address (for balance-weighted voting)
    pub fn set_call_balance(&mut self, address: Address, balance: Balance) {
        self.call_balances.insert(address, balance);
    }

    /// Get voting balance for an address.
    /// Uses external balance source if configured, falls back to internal `call_balances`.
    pub fn get_voting_balance(&self, address: Address) -> Balance {
        if let Some(ref source) = self.balance_source {
            source(address)
        } else {
            self.call_balances.get(&address).copied().unwrap_or(0)
        }
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
        // Rate limiting: reject if proposer submitted within the last PROPOSAL_COOLDOWN_BLOCKS
        if let Some(&last_block) = self.last_submission_block.get(&proposer) {
            if self.current_block.saturating_sub(last_block) < PROPOSAL_COOLDOWN_BLOCKS {
                let remaining = PROPOSAL_COOLDOWN_BLOCKS - (self.current_block - last_block);
                return Err(GovernanceError::ProposalRateLimited(remaining));
            }
        }

        // Check deposit sufficiency
        let deposit = self.config.proposal_deposit;
        let proposer_balance = self.get_voting_balance(proposer);
        if proposer_balance < deposit {
            return Err(GovernanceError::InsufficientDeposit);
        }

        // Deduct deposit (only from internal balances; external balances are tracked via deposits map)
        let internal = self.call_balances.get(&proposer).copied().unwrap_or(0);
        if internal > 0 {
            let new_balance = internal - deposit.min(internal);
            self.call_balances.insert(proposer, new_balance);
        }
        self.deposits.insert(proposer, deposit);

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
            deposit,
        };

        self.proposals.insert(id, proposal);
        self.last_submission_block.insert(proposer, self.current_block);
        Ok(id)
    }

    /// Submit a proposal where the deposit has already been deducted externally.
    /// Used by instruction execution where BalanceState handles the deposit.
    pub fn submit_proposal_with_deposit(
        &mut self,
        proposer: Address,
        proposal_type: ProposalType,
        title: String,
        description: String,
        execution_data: Vec<u8>,
    ) -> Result<u64, GovernanceError> {
        // Rate limiting
        if let Some(&last_block) = self.last_submission_block.get(&proposer) {
            if self.current_block.saturating_sub(last_block) < PROPOSAL_COOLDOWN_BLOCKS {
                let remaining = PROPOSAL_COOLDOWN_BLOCKS - (self.current_block - last_block);
                return Err(GovernanceError::ProposalRateLimited(remaining));
            }
        }

        // Deposit already paid externally; just record it
        let deposit = self.config.proposal_deposit;
        self.deposits.insert(proposer, deposit);

        let id = self.next_proposal_id;
        self.next_proposal_id += 1;

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
            deposit,
        };

        self.proposals.insert(id, proposal);
        self.last_submission_block.insert(proposer, self.current_block);
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
            p.execution_block = Some(self.current_block + self.config.timelock_period_blocks);
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
            let from = p.state;
            // For emergency pause, skip timelock
            if matches!(proposal_type, ProposalType::EmergencyPause { .. }) {
                p.state = ProposalState::Queued;
                p.execution_block = Some(self.current_block);
            } else {
                p.state = ProposalState::Queued;
                p.execution_block = Some(self.current_block + self.config.timelock_period_blocks);
            }
            self.events.push(GovernanceEvent::ProposalAdvanced { id: proposal_id, from, to: ProposalState::Queued });
        } else {
            let p = self.proposals.get_mut(&proposal_id).unwrap();
            p.state = ProposalState::Defeated;
            // Confiscate deposit
            self.deposits.remove(&proposer);
            self.events.push(GovernanceEvent::ProposalDefeated { id: proposal_id });
            return Err(GovernanceError::ProposalDefeated);
        }

        Ok(())
    }

    /// Execute a queued proposal (after timelock).
    /// Only the original proposer or a registered validator may execute.
    pub fn execute_proposal(
        &mut self,
        proposal_id: u64,
        executor: Address,
    ) -> Result<(), GovernanceError> {
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

        // Authorization: executor must be the proposer or a registered validator
        if executor != proposer && self.validator_id_by_address(executor).is_none() {
            return Err(GovernanceError::UnauthorizedExecutor);
        }

        self.proposals.get_mut(&proposal_id).unwrap().state = ProposalState::Executed;

        // Apply the on-chain change (internal placeholder)
        self.apply_proposal(proposal_id)?;

        // Execute real on-chain side effects via the executor (if configured)
        if let Some(ref executor) = self.executor {
            let proposal = self.proposals.get(&proposal_id).ok_or(GovernanceError::ProposalNotFound)?;
            executor.on_proposal_executed(proposal).map_err(|e| GovernanceError::ExecutionFailed(e))?;
        }

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

        if self.current_block < execution_block + self.config.execution_timeout_blocks {
            return Err(GovernanceError::ExecutionTimeoutNotReached);
        }

        self.proposals.get_mut(&proposal_id).unwrap().state = ProposalState::Expired;
        self.deposits.remove(&proposer);

        Ok(())
    }

    // ── On-Chain Execution ──────────────────────────────────────────

    /// Apply the actual on-chain change from an executed proposal.
    /// Updates governance-internal state for all proposal types.
    /// Cross-system effects (consensus, fork manager, oracle, etc.) are handled
    /// by the optional `ProposalExecutor` callback.
    fn apply_proposal(&mut self, proposal_id: u64) -> Result<(), GovernanceError> {
        let proposal = self.proposals.get(&proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        let current_block = self.current_block;

        match &proposal.proposal_type {
            ProposalType::ParameterChange { param_id, new_value } => {
                // Parse new_value as JSON and apply governance-internal config updates.
                // Cross-system param updates (consensus, validator, oracle) are handled
                // by the executor callback.
                match serde_json::from_str::<serde_json::Value>(new_value) {
                    Ok(val) => {
                        if param_id.starts_with("governance.") {
                            if let Some(v) = val.get("validator_quorum_bps").and_then(|v| v.as_u64()) {
                                self.config.validator_quorum_bps = v as u32;
                            }
                            if let Some(v) = val.get("supply_quorum_bps").and_then(|v| v.as_u64()) {
                                self.config.supply_quorum_bps = v as u32;
                            }
                            if let Some(v) = val.get("treasury_quorum_bps").and_then(|v| v.as_u64()) {
                                self.config.treasury_quorum_bps = v as u32;
                            }
                            if let Some(v) = val.get("simple_majority_bps").and_then(|v| v.as_u64()) {
                                self.config.simple_majority_bps = v as u32;
                            }
                            if let Some(v) = val.get("review_period_blocks").and_then(|v| v.as_u64()) {
                                self.config.review_period_blocks = v;
                            }
                            if let Some(v) = val.get("voting_period_blocks").and_then(|v| v.as_u64()) {
                                self.config.voting_period_blocks = v;
                            }
                            if let Some(v) = val.get("timelock_period_blocks").and_then(|v| v.as_u64()) {
                                self.config.timelock_period_blocks = v;
                            }
                            if let Some(v) = val.get("execution_timeout_blocks").and_then(|v| v.as_u64()) {
                                self.config.execution_timeout_blocks = v;
                            }
                            if let Some(v) = val.get("proposal_deposit").and_then(|v| v.as_u64()) {
                                self.config.proposal_deposit = v as u128;
                            }
                            if let Some(v) = val.get("asset_registration_fee").and_then(|v| v.as_u64()) {
                                self.config.asset_registration_fee = v as u128;
                            }
                            tracing::info!(param_id, new_value, "governance config updated via apply_proposal");
                        } else {
                            tracing::info!(param_id, new_value, "parameter change recorded; cross-system update via executor");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to parse ParameterChange new_value as JSON");
                    }
                }
            }
            ProposalType::ProtocolUpgrade { activation_block, changelog } => {
                self.scheduled_upgrades.push(ScheduledUpgrade {
                    proposal_id,
                    activation_block: *activation_block,
                    changelog: changelog.clone(),
                    applied: false,
                });
                tracing::info!(activation_block, changelog, "protocol upgrade recorded in governance state");
            }
            ProposalType::TreasurySpend { recipient, amount, asset_id } => {
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
                if let Some(addr) = self.validator_addresses.remove(validator_id) {
                    self.call_balances.remove(&addr);
                    self.validator_pubkeys.remove(validator_id);
                    tracing::info!(validator_id, reason, "validator slashed via governance");
                }
            }
            ProposalType::ComplianceUpdate { asset_id, new_policy } => {
                self.compliance_policies.insert(*asset_id, *new_policy);
                tracing::info!(asset_id, new_policy, "compliance policy updated via governance");
            }
            ProposalType::EmergencyPause { reason } => {
                self.emergency_pause.is_paused = true;
                self.emergency_pause.pause_reason = reason.clone();
                tracing::info!(reason, "emergency pause activated via governance");
            }
            ProposalType::FeeCurrencyAdd { asset_id, name, oracle_price_key } => {
                self.fee_currencies.insert(*asset_id, FeeCurrencyEntry {
                    name: name.clone(),
                    oracle_price_key: oracle_price_key.clone(),
                    added_at_block: current_block,
                });
                // If previously pending removal, cancel it
                self.fee_currencies_pending_removal.remove(asset_id);
                tracing::info!(asset_id, name, oracle_price_key, "fee currency added via governance");
            }
            ProposalType::FeeCurrencyRemove { asset_id, grace_period_blocks } => {
                let end_block = current_block.saturating_add(*grace_period_blocks);
                self.fee_currencies_pending_removal.insert(*asset_id, end_block);
                tracing::info!(asset_id, grace_period_blocks, end_block, "fee currency removal scheduled");
            }
            ProposalType::FeeCurrencyCap { new_cap_bps } => {
                self.fee_currency_cap_bps = *new_cap_bps;
                tracing::info!(new_cap_bps, "fee currency cap updated via governance");
            }
            ProposalType::ValidatorKeyRotation { validator_id, old_pubkey, new_pubkey, .. } => {
                self.validator_pubkeys.insert(*validator_id, *new_pubkey);
                tracing::info!(validator_id, ?old_pubkey, ?new_pubkey, "validator key rotation applied");
            }
        }

        Ok(())
    }

    // ── State Machine Advancement ─────────────────────────────────────

    /// Advance all proposals through the state machine based on current block.
    /// Called once per block by the block production loop.
    ///
    /// Transitions:
    /// - Pending → Active (review period passed)
    /// - Active → Queued (voting ended, quorum met, majority yes)
    /// - Active → Defeated (voting ended, quorum failed or majority no)
    /// - Queued → Expired (execution timeout reached, deposit confiscated)
    /// - Queued → Executed (timelock elapsed, auto-executed)
    pub fn advance(&mut self, current_block: u64) {
        self.current_block = current_block;
        self.events.clear();

        // Keep advancing until no more transitions are needed.
        // This handles the case where we jump many blocks at once
        // (e.g., from Pending straight past voting_end).
        loop {
            let mut to_activate = Vec::new();
            let mut to_queue = Vec::new();
            let mut to_defeat = Vec::new();
            let mut to_expire = Vec::new();
            let mut to_execute = Vec::new();

            for (&id, proposal) in &self.proposals {
                match proposal.state {
                    ProposalState::Pending => {
                        if current_block >= proposal.start_block {
                            to_activate.push(id);
                        }
                    }
                    ProposalState::Active => {
                        if current_block > proposal.end_block {
                            if proposal.has_quorum() && proposal.is_majority_yes() {
                                to_queue.push(id);
                            } else {
                                to_defeat.push(id);
                            }
                        }
                    }
                    ProposalState::Queued => {
                        let exec = match proposal.execution_block {
                            Some(b) => b,
                            None => continue,
                        };
                        if current_block > exec + self.config.execution_timeout_blocks {
                            to_expire.push(id);
                        } else if current_block >= exec {
                            to_execute.push(id);
                        }
                    }
                    _ => {}
                }
            }

            if to_activate.is_empty() && to_queue.is_empty() && to_defeat.is_empty() && to_expire.is_empty() && to_execute.is_empty() {
                break;
            }

            for id in to_activate {
                if let Some(p) = self.proposals.get_mut(&id) {
                    let from = p.state;
                    p.state = ProposalState::Active;
                    self.events.push(GovernanceEvent::ProposalAdvanced { id, from, to: ProposalState::Active });
                    tracing::info!(proposal_id = id, "proposal advanced to active");
                }
            }

            for id in &to_queue {
                let _ = self.queue_proposal(*id);
            }

            for id in to_defeat {
                if let Some(p) = self.proposals.get_mut(&id) {
                    let _from = p.state;
                    p.state = ProposalState::Defeated;
                    let proposer = p.proposer;
                    self.deposits.remove(&proposer);
                    self.events.push(GovernanceEvent::ProposalDefeated { id });
                    tracing::info!(proposal_id = id, "proposal defeated — deposit confiscated");
                }
            }

            for id in to_expire {
                if let Some(p) = self.proposals.get_mut(&id) {
                    let _from = p.state;
                    p.state = ProposalState::Expired;
                    let proposer = p.proposer;
                    self.deposits.remove(&proposer);
                    self.events.push(GovernanceEvent::ProposalExpired { id });
                    tracing::info!(proposal_id = id, "proposal expired — deposit confiscated");
                }
            }

            for id in to_execute {
                let proposal_type_str = self.proposals.get(&id)
                    .map(|p| format!("{:?}", p.proposal_type))
                    .unwrap_or_default();
                let proposer = self.proposals.get(&id).map(|p| p.proposer).unwrap_or_default();
                let _ = self.execute_proposal(id, proposer);
                self.events.push(GovernanceEvent::ProposalExecuted { id, proposal_type: proposal_type_str });
            }
        }
    }

    /// Drain and return governance events, clearing the internal buffer.
    /// Called once per block by the node to forward events to subscribers.
    pub fn drain_events(&mut self) -> Vec<GovernanceEvent> {
        std::mem::take(&mut self.events)
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
        let threshold = self.config.emergency_pause_threshold(total_validators);

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
        let delegator_balance = self.get_voting_balance(delegator);
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
                let balance = self.get_voting_balance(voter);
                let with_delegation = balance + self.get_delegated_voting_power(voter);
                power = power.max(with_delegation);
            }
        }

        if proposal_type.is_balance_weighted() {
            // TreasurySpend: CALL balance weighted
            let balance = self.get_voting_balance(voter);
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
        let total_validators = self.validator_addresses.len() as u64;
        let voting_start = self.current_block + self.config.review_period_blocks;
        let voting_end = voting_start + self.config.voting_period_blocks;

        let quorum: Balance = match proposal_type {
            ProposalType::ParameterChange { .. } => {
                self.config.validator_quorum(total_validators) as Balance
            }
            ProposalType::ProtocolUpgrade { .. } => {
                // max(validator quorum, supply quorum)
                let validator_quorum = self.config.validator_quorum(total_validators) as Balance;
                let supply_quorum = self.config.supply_quorum();
                validator_quorum.max(supply_quorum)
            }
            ProposalType::TreasurySpend { .. } => {
                self.config.treasury_quorum()
            }
            ProposalType::ValidatorSlash { .. } => {
                self.config.validator_quorum(total_validators) as Balance
            }
            ProposalType::ComplianceUpdate { .. } => {
                self.config.simple_majority(total_validators) as Balance
            }
            ProposalType::EmergencyPause { .. } => {
                self.config.emergency_pause_threshold(total_validators) as Balance
            }
            ProposalType::FeeCurrencyAdd { .. }
            | ProposalType::FeeCurrencyRemove { .. }
            | ProposalType::FeeCurrencyCap { .. }
            | ProposalType::ValidatorKeyRotation { .. } => {
                self.config.simple_majority(total_validators) as Balance
            }
        };

        (quorum, voting_start, voting_end)
    }
}

// ── Test helpers (pub(crate) so tests.rs can use them) ────────────────

#[cfg(test)]
pub(crate) fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

#[cfg(test)]
pub(crate) fn one_million_call() -> Balance {
    1_000_000 * 10u128.pow(18)
}

#[cfg(test)]
pub(crate) fn make_manager_with_validators(count: u8) -> GovernanceManager {
    let mut mgr = GovernanceManager::new();
    for i in 1..=count {
        mgr.register_validator(i as ValidatorId, test_addr(i));
        // Small balance so balance-weighted voting doesn't dominate
        mgr.set_call_balance(test_addr(i), 1);
    }
    mgr
}
