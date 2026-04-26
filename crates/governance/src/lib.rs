//! T12.1 — Governance Module (per spec §13.3)
//!
//! Proposal lifecycle, dual-track voting, timelock, emergency pause.

use call_primitives::{Address, AssetId, Balance, ValidatorId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// External balance source for real on-chain balances.
/// When set, deposit checks and voting power use this instead of `call_balances`.
pub type BalanceSource = Arc<dyn Fn(Address) -> Balance + Send + Sync>;

/// Total supply: 1B CALL * 10^18 (18 decimals)
pub const TOTAL_SUPPLY: Balance = 1_000_000_000_000_000_000_000_000_000u128;

// ── Proposal Executor ─────────────────────────────────────────────────

/// Trait for executing real on-chain changes when a governance proposal passes.
/// Implement this in the node layer to dispatch to consensus, fork manager, etc.
pub trait ProposalExecutor: Send + Sync {
    /// Execute the on-chain effect of a proposal. Called after state is updated.
    fn on_proposal_executed(&self, proposal: &Proposal) -> Result<(), String>;
}

// ── Governance Configuration ──────────────────────────────────────────

/// Quorum and timing configuration for governance proposals.
/// Set at genesis and loaded into `GovernanceManager`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceConfig {
    /// Validator quorum for parameter changes and slashes (basis points, 6667 = 2/3)
    pub validator_quorum_bps: u32,
    /// Supply quorum for protocol upgrades (basis points, 2000 = 20%)
    pub supply_quorum_bps: u32,
    /// Treasury spend quorum (basis points, 2000 = 20%)
    pub treasury_quorum_bps: u32,
    /// Simple majority threshold (basis points, 5001 = 50% + 1)
    pub simple_majority_bps: u32,
    /// Emergency pause signature threshold (basis points, 6667 = 2/3)
    pub emergency_pause_bps: u32,
    /// Review period in blocks (~2 days)
    pub review_period_blocks: u64,
    /// Voting period in blocks (~7 days)
    pub voting_period_blocks: u64,
    /// Timelock period in blocks (~7 days)
    pub timelock_period_blocks: u64,
    /// Execution timeout in blocks (~30 days)
    pub execution_timeout_blocks: u64,
    /// Proposal deposit amount (default: 10,000 CALL)
    pub proposal_deposit: Balance,
    /// Asset registration fee (default: 10 CALL)
    pub asset_registration_fee: Balance,
}

impl Default for GovernanceConfig {
    fn default() -> Self {
        Self {
            validator_quorum_bps: 6667,    // 2/3
            supply_quorum_bps: 2000,        // 20%
            treasury_quorum_bps: 2000,      // 20%
            simple_majority_bps: 5001,      // 50% + 1
            emergency_pause_bps: 6667,      // 2/3
            review_period_blocks: REVIEW_PERIOD_BLOCKS,
            voting_period_blocks: VOTING_PERIOD_BLOCKS,
            timelock_period_blocks: TIMELOCK_PERIOD_BLOCKS,
            execution_timeout_blocks: EXECUTION_TIMEOUT_BLOCKS,
            proposal_deposit: DEFAULT_PROPOSAL_DEPOSIT,
            asset_registration_fee: DEFAULT_ASSET_REGISTRATION_FEE,
        }
    }
}

impl GovernanceConfig {
    /// Calculate validator quorum count (ceil of total_validators * bps / 10000)
    pub fn validator_quorum(&self, total_validators: u64) -> u64 {
        (total_validators * self.validator_quorum_bps as u64).div_ceil(10_000)
    }

    /// Calculate supply quorum (ceil of total_supply * bps / 10000)
    pub fn supply_quorum(&self) -> Balance {
        (TOTAL_SUPPLY * self.supply_quorum_bps as u128) / 10_000
    }

    /// Calculate treasury quorum
    pub fn treasury_quorum(&self) -> Balance {
        (TOTAL_SUPPLY * self.treasury_quorum_bps as u128) / 10_000
    }

    /// Calculate simple majority quorum count
    pub fn simple_majority(&self, total_validators: u64) -> u64 {
        (total_validators * self.simple_majority_bps as u64).div_ceil(10_000)
    }

    /// Calculate emergency pause threshold
    pub fn emergency_pause_threshold(&self, total_validators: u64) -> u64 {
        (total_validators * self.emergency_pause_bps as u64).div_ceil(10_000)
    }
}

// ── Constants ─────────────────────────────────────────────────────────

/// Default proposal deposit: 10,000 CALL
pub const DEFAULT_PROPOSAL_DEPOSIT: Balance = 10_000 * 10u128.pow(18);
/// Default asset registration fee: 10 CALL
pub const DEFAULT_ASSET_REGISTRATION_FEE: Balance = 10_000_000_000_000_000_000u128;

/// Minimum blocks between proposal submissions by the same address (~1 day at 250ms)
pub const PROPOSAL_COOLDOWN_BLOCKS: u64 = 345_600;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmergencyPauseState {
    pub is_paused: bool,
    pub pause_reason: String,
    pub pause_signatures: HashMap<Address, bool>, // validator_id → signed
}

// ── Governance Events ─────────────────────────────────────────────────

/// Events emitted during proposal state machine advancement.
/// Drained each block and forwarded to WebSocket subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GovernanceEvent {
    ProposalAdvanced { id: u64, from: ProposalState, to: ProposalState },
    ProposalExecuted { id: u64, proposal_type: String },
    ProposalExpired { id: u64 },
    ProposalDefeated { id: u64 },
}

// ── Scheduled Upgrade ─────────────────────────────────────────────────

/// A protocol upgrade scheduled by a governance proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledUpgrade {
    pub proposal_id: u64,
    pub activation_block: u64,
    pub changelog: String,
    pub applied: bool,
}

// ── Fee Currency Entry ────────────────────────────────────────────────

/// A fee currency registered via governance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeCurrencyEntry {
    pub name: String,
    pub oracle_price_key: String,
    pub added_at_block: u64,
}

fn default_fee_currency_cap() -> u32 {
    10_000 // 100%
}

// ── Governance Manager ────────────────────────────────────────────────

/// Manages governance proposals, voting, and execution (per spec §13.3)
#[derive(Serialize, Deserialize)]
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
    /// Rate limiting: address → last submission block height
    #[serde(skip)]
    last_submission_block: HashMap<Address, u64>,
    /// Current block height
    current_block: u64,
    /// Emergency pause state
    pub emergency_pause: EmergencyPauseState,
    /// Governance configuration (quorum thresholds, periods)
    pub config: GovernanceConfig,
    /// Events emitted during state advancement (not serialized — drained each block)
    #[serde(skip)]
    events: Vec<GovernanceEvent>,
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
    #[serde(default = "default_fee_currency_cap")]
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
    #[error("proposal rate limited — {0} blocks remaining before next submission allowed")]
    ProposalRateLimited(u64),
    #[error("unauthorized executor")]
    UnauthorizedExecutor,
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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        assert_eq!(*balance, mgr.config.proposal_deposit);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        let result = mgr.execute_proposal(id, proposer);
        assert!(result.is_err());

        // Advance past timelock
        let exec_block = mgr.get_proposal(id).unwrap().execution_block.unwrap();
        mgr.set_current_block(exec_block + 1);
        mgr.execute_proposal(id, proposer).unwrap();

        let proposal = mgr.get_proposal(id).unwrap();
        assert_eq!(proposal.state, ProposalState::Executed);

        // Deposit returned to proposer
        let balance = mgr.call_balances.get(&proposer).unwrap();
        assert_eq!(*balance, mgr.config.proposal_deposit * 2 - mgr.config.proposal_deposit + mgr.config.proposal_deposit); // deposit deducted then restored
    }

    // ── Proposal Expire ───────────────────────────────────────────────

    #[test]
    fn test_proposal_expire_confiscate_deposit() {
        let mut mgr = make_manager_with_validators(3);

        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);
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

        // With 3 validators and 6667 BPS, ceil(3 * 6667 / 10000) = 3, so ALL 3 needed
        let result = mgr
            .emergency_pause_initiate(1, "critical bug".into())
            .unwrap();
        assert!(!result); // need more signatures

        let result = mgr
            .emergency_pause_initiate(2, "critical bug".into())
            .unwrap();
        assert!(!result); // still need one more

        let result = mgr
            .emergency_pause_initiate(3, "critical bug".into())
            .unwrap();
        assert!(result); // 3/3 reached, pause activated

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

        // Register an issuer
        let issuer = test_addr(20);
        mgr.register_asset_issuer(1, issuer);
        mgr.set_call_balance(issuer, mgr.config.proposal_deposit);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit - 1);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        mgr.execute_proposal(id, proposer).unwrap();

        let p = mgr.get_proposal(id).unwrap();
        assert_eq!(p.state, ProposalState::Executed);
    }

    // ── Duplicate Vote Prevention ─────────────────────────────────────

    #[test]
    fn test_voter_cannot_vote_twice_same_proposal() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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

    // ── Auto-advance state machine ────────────────────────────────────

    #[test]
    fn test_advance_auto_transitions() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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

        // Initially pending
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Pending);

        // Advance to review period — should auto-activate
        mgr.advance(REVIEW_PERIOD_BLOCKS + 1);
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Active);

        // Set votes for quorum
        {
            let p = mgr.proposals.get_mut(&id).unwrap();
            p.voting_power_yes = 3;
        }

        // Advance past voting — should auto-queue and auto-execute
        let voting_end = mgr.get_proposal(id).unwrap().end_block;
        mgr.advance(voting_end + 1);
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Queued);

        // Advance past timelock — should auto-execute
        let exec = mgr.get_proposal(id).unwrap().execution_block.unwrap();
        mgr.advance(exec + 1);
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Executed);
    }

    #[test]
    fn test_advance_auto_defeat() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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

        // Advance past review
        mgr.advance(REVIEW_PERIOD_BLOCKS + 1);

        // No votes — advance past voting end
        let voting_end = mgr.get_proposal(id).unwrap().end_block;
        mgr.advance(voting_end + 1);

        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Defeated);
        // Deposit confiscated
        assert!(mgr.deposits.get(&proposer).is_none());
    }

    #[test]
    fn test_advance_auto_expire() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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

        // Set votes, advance to queued
        {
            let p = mgr.proposals.get_mut(&id).unwrap();
            p.voting_power_yes = 3;
        }
        mgr.advance(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);

        // Should be queued now
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Queued);

        // Advance past execution timeout
        let exec = mgr.get_proposal(id).unwrap().execution_block.unwrap();
        mgr.advance(exec + EXECUTION_TIMEOUT_BLOCKS + 1);

        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Expired);
        assert!(mgr.deposits.get(&proposer).is_none());
    }

    // ── Event emission ────────────────────────────────────────────────

    #[test]
    fn test_advance_emits_events() {
        let mut mgr = make_manager_with_validators(3);
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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

        // Advance to active
        mgr.advance(REVIEW_PERIOD_BLOCKS + 1);
        let events = mgr.drain_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], GovernanceEvent::ProposalAdvanced { id: event_id, from: ProposalState::Pending, to: ProposalState::Active } if *event_id == id));

        // Set votes, advance past voting
        {
            let p = mgr.proposals.get_mut(&id).unwrap();
            p.voting_power_yes = 3;
        }
        let voting_end = mgr.get_proposal(id).unwrap().end_block;
        mgr.advance(voting_end + 1);
        let events = mgr.drain_events();
        // Queued transition emits event
        assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalAdvanced { to: ProposalState::Queued, .. })));

        // Advance past timelock to execute
        let exec = mgr.get_proposal(id).unwrap().execution_block.unwrap();
        mgr.advance(exec + 1);
        let events = mgr.drain_events();
        assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalExecuted { id: event_id, .. } if *event_id == id)));

        // drain_events clears the buffer
        assert!(mgr.drain_events().is_empty());
    }

    // ── Balance source ────────────────────────────────────────────────

    #[test]
    fn test_balance_source_fallback() {
        let mut mgr = GovernanceManager::new();
        let addr = test_addr(1);

        // Without balance_source, uses call_balances
        mgr.set_call_balance(addr, 500_000);
        assert_eq!(mgr.get_voting_balance(addr), 500_000);

        // With balance_source set, uses external
        mgr.balance_source = Some(Arc::new(|_| 1_000_000));
        assert_eq!(mgr.get_voting_balance(addr), 1_000_000);
    }

    // ── GovernanceConfig ──────────────────────────────────────────────

    #[test]
    fn test_config_quorum_calculations() {
        let config = GovernanceConfig::default();

        // With div_ceil and 6667 BPS, small validator sets round up
        assert_eq!(config.validator_quorum(3), 3); // ceil(3 * 6667 / 10000) = 3
        assert_eq!(config.validator_quorum(10), 7); // ceil(10 * 6667 / 10000) = 7

        assert_eq!(config.supply_quorum(), TOTAL_SUPPLY / 5); // 20%
        assert_eq!(config.treasury_quorum(), TOTAL_SUPPLY / 5); // 20%

        assert_eq!(config.simple_majority(3), 2); // ceil(3 * 5001 / 10000) = 2
        assert_eq!(config.emergency_pause_threshold(3), 3); // ceil(3 * 6667 / 10000) = 3
    }

    #[test]
    fn test_config_custom_periods() {
        let config = GovernanceConfig {
            review_period_blocks: 100,
            voting_period_blocks: 500,
            timelock_period_blocks: 1000,
            execution_timeout_blocks: 5000,
            ..GovernanceConfig::default()
        };

        let mut mgr = GovernanceManager::new().with_config(config.clone());
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

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

        // Should activate at review_period_blocks (100), not default (691200)
        mgr.advance(101);
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Active);
    }

    // ── Rate limiting ─────────────────────────────────────────────────

    #[test]
    fn test_proposal_rate_limiting() {
        let mut mgr = GovernanceManager::new();
        let proposer = test_addr(1);
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 10);

        // First proposal should succeed
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
        assert_eq!(id, 0);

        // Immediate second proposal should be rate limited
        let result = mgr.submit_proposal(
            proposer,
            ProposalType::ParameterChange {
                param_id: "test2".into(),
                new_value: "2".into(),
            },
            "Test2".into(),
            "Test2".into(),
            vec![],
        );
        assert!(matches!(result, Err(GovernanceError::ProposalRateLimited(_))));

        // Advance past cooldown and try again
        mgr.set_current_block(PROPOSAL_COOLDOWN_BLOCKS + 1);
        let id = mgr
            .submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test2".into(),
                    new_value: "2".into(),
                },
                "Test2".into(),
                "Test2".into(),
                vec![],
            )
            .unwrap();
        assert_eq!(id, 1);
    }

    // ── Full cycle (submit → advance → execute) ───────────────────────

    #[test]
    fn test_full_lifecycle_with_executor() {
        use std::sync::atomic::{AtomicU64, Ordering};

        struct TestExecutor {
            executed_count: AtomicU64,
        }
        impl ProposalExecutor for TestExecutor {
            fn on_proposal_executed(&self, _proposal: &Proposal) -> Result<(), String> {
                self.executed_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let mut mgr = GovernanceManager::new()
            .with_executor(Arc::new(TestExecutor { executed_count: AtomicU64::new(0) }));
        let proposer = test_addr(10);
        mgr.set_call_balance(proposer, mgr.config.proposal_deposit * 2);

        // Register validators
        for i in 1u8..=3 {
            mgr.register_validator(i as u32, test_addr(i));
            mgr.set_call_balance(test_addr(i), 1);
        }

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

        // Set votes for quorum before advancing past voting
        {
            let p = mgr.proposals.get_mut(&id).unwrap();
            p.voting_power_yes = 3; // All 3 validators
        }

        // Step 1: Advance past review + voting to get queued
        mgr.advance(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + 1);
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Queued);

        // Drain events from first advance
        let events = mgr.drain_events();
        assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalAdvanced { to: ProposalState::Queued, .. })));

        // Step 2: Advance past timelock to execute
        let exec = mgr.get_proposal(id).unwrap().execution_block.unwrap();
        mgr.advance(exec + 1);
        assert_eq!(mgr.get_proposal(id).unwrap().state, ProposalState::Executed);

        // Drain events — should include ProposalExecuted
        let events = mgr.drain_events();
        assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalExecuted { id: eid, .. } if *eid == id)));
    }
}
