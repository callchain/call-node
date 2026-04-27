//! Fee calculation, deduction, allocation, and stablecoin conversion.

use call_primitives::{Address, FeeCurrency, TxHash};
use crate::account::AccountState;
use crate::sponsor::SponsorRegistry;
use crate::ProtocolResult;
use crate::tx::model::GasConfig;

/// Compute the actual fee for a transaction
pub fn compute_fee(
    gas_units: u64,
    priority_fee_per_gas: u128,
    base_fee: u128,
) -> u128 {
    let base_total = (gas_units as u128).saturating_mul(base_fee);
    let priority_total = (gas_units as u128).saturating_mul(priority_fee_per_gas);
    base_total.saturating_add(priority_total)
}

// ── Gas deduction ─────────────────────────────────────────────────────

/// Deduct gas fee from payer (per spec §3.6)
pub fn deduct_gas(
    account: &mut AccountState,
    config: &GasConfig,
    fee_currency: &FeeCurrency,
    fee: u128,
    sender: Address,
    _tx_hash: TxHash,
    sponsor_registry: &mut SponsorRegistry,
    current_day: u64,
) -> ProtocolResult<()> {
    match fee_currency {
        FeeCurrency::Call => {
            deduct_call_from_payer(account, config, fee, sender, sponsor_registry, current_day)
        }
        FeeCurrency::Stablecoin(asset_id) => {
            deduct_stablecoin_from_payer(
                account, config, *asset_id, fee, sender, sponsor_registry, current_day,
            )
        }
    }
}

/// Deduct CALL fees (per spec §3.6)
fn deduct_call_from_payer(
    account: &mut AccountState,
    config: &GasConfig,
    fee: u128,
    sender: Address,
    sponsor_registry: &mut SponsorRegistry,
    current_day: u64,
) -> ProtocolResult<()> {
    match config {
        GasConfig::SelfPay => account.deduct_balance(crate::CALL_ASSET_ID, sender, fee),
        GasConfig::AuthorizedSponsor { sponsor } => {
            sponsor_registry.verify_and_deduct_authorized_sponsor(
                sponsor, &sender, fee, current_day, account,
            )
        }
        GasConfig::PoolSponsor { sponsor } => {
            sponsor_registry.verify_and_deduct_pool_sponsor(sponsor, &sender, fee, account)
        }
        GasConfig::PerTxSponsor { sponsor, .. } => {
            sponsor_registry.verify_and_deduct_per_tx_sponsor(sponsor, fee, account)
        }
    }
}

/// Deduct stablecoin fees (per spec §3.6)
fn deduct_stablecoin_from_payer(
    account: &mut AccountState,
    config: &GasConfig,
    asset_id: u64,
    fee: u128,
    sender: Address,
    sponsor_registry: &mut SponsorRegistry,
    current_day: u64,
) -> ProtocolResult<()> {
    match config {
        GasConfig::SelfPay => account.deduct_balance(asset_id, sender, fee),
        GasConfig::AuthorizedSponsor { sponsor } => {
            sponsor_registry.verify_and_deduct_authorized_sponsor(
                sponsor, &sender, fee, current_day, account,
            )
        }
        GasConfig::PoolSponsor { sponsor } => {
            sponsor_registry.verify_and_deduct_pool_sponsor(sponsor, &sender, fee, account)
        }
        GasConfig::PerTxSponsor { sponsor, .. } => {
            sponsor_registry.verify_and_deduct_per_tx_sponsor(sponsor, fee, account)
        }
    }
}

// ── Fee allocation (per spec §12.2.5) ────────────────────────────────

/// Allocate fees: 50% base burn, 50% validator reward, 100% priority to proposer
pub fn allocate_call_fee(base_fee_total: u128, priority_fee_total: u128, proposer: Address) -> FeeAllocation {
    let burn = base_fee_total / 2;
    let validator_reward = base_fee_total - burn;
    FeeAllocation {
        burn,
        validator_reward,
        proposer_fee: priority_fee_total,
        proposer,
    }
}

/// Allocate stablecoin fees: 50% to treasury, 50% validator reward
pub fn allocate_stablecoin_fee(base_fee_total: u128, priority_fee_total: u128, proposer: Address) -> FeeAllocation {
    FeeAllocation {
        burn: 0,
        validator_reward: base_fee_total / 2,
        proposer_fee: priority_fee_total,
        proposer,
    }
}

#[derive(Debug, Clone)]
pub struct FeeAllocation {
    pub burn: u128,
    pub validator_reward: u128,
    pub proposer_fee: u128,
    pub proposer: Address,
}

// ── Stablecoin fee conversion (per spec §12.3.0) ─────────────────────

/// Convert CALL fee to stablecoin using oracle price (ceil rounding)
pub fn convert_fee_to_stablecoin(
    call_fee: u128,
    call_price_usd: u128,
    stablecoin_decimals: u8,
) -> u128 {
    if call_price_usd == 0 {
        return u128::MAX;
    }
    // ceil(call_fee * call_price / stablecoin_price)
    // Use saturating multiplication to prevent overflow
    let scaled = call_fee.saturating_mul(call_price_usd);
    let divisor = 10u128.pow(stablecoin_decimals as u32);
    if divisor == 0 {
        return u128::MAX;
    }
    scaled.div_ceil(divisor)
}
