use call_primitives::Balance;
use serde::{Deserialize, Serialize};

/// Total supply: 1B CALL * 10^18 (18 decimals)
pub const TOTAL_SUPPLY: Balance = 1_000_000_000_000_000_000_000_000_000u128;

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
pub const TOTAL_SUPPLY_DIV_10: Balance = TOTAL_SUPPLY / 10;

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
            validator_quorum_bps: 6667, // 2/3
            supply_quorum_bps: 2000,    // 20%
            treasury_quorum_bps: 2000,  // 20%
            simple_majority_bps: 5001,  // 50% + 1
            emergency_pause_bps: 6667,  // 2/3
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
