//! Gas unit table and fee parameters.

/// Minimum priority fee per gas (1 wei).
pub const MIN_PRIORITY_FEE_PER_GAS: u128 = 1;

// ── Fee parameters (per spec §12.2.3) ─────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeeParams {
    /// Current base fee in wei
    pub base_fee: u128,
    /// Target gas per block
    pub target_gas_per_block: u64,
    /// Maximum gas per block
    pub max_gas_per_block: u64,
    /// Adjustment coefficient (1/8)
    pub adjustment_coefficient: u128,
    /// Minimum base fee (1 wei)
    pub min_base_fee: u128,
    /// Maximum base fee (1B wei)
    pub max_base_fee: u128,
    /// Initial base fee (10 wei)
    pub initial_base_fee: u128,
    /// Percentage of block fees allocated to oracle rewards (basis points, 100 = 1%)
    pub oracle_fee_share_bps: u16,
}

impl Default for FeeParams {
    fn default() -> Self {
        Self {
            base_fee: 10,                // 10 wei initial
            target_gas_per_block: 10_000_000, // 10M
            max_gas_per_block: 20_000_000,    // 20M
            adjustment_coefficient: 1,   // numerator (denominator = 8)
            min_base_fee: 1,             // 1 wei
            max_base_fee: 1_000_000_000, // 1B wei
            initial_base_fee: 10,        // 10 wei
            oracle_fee_share_bps: 100,   // 1% of block fees to oracle rewards
        }
    }
}

/// Update base fee based on block utilization (per spec §12.2.3)
pub fn update_base_fee(params: &mut FeeParams, gas_used: u64) {
    let target = params.target_gas_per_block as i128;
    let used = gas_used as i128;
    let diff = used - target;

    if diff > 0 {
        // Increase: base_fee * (1 + diff/target * coeff/8)
        let numerator = diff * params.adjustment_coefficient as i128;
        let denominator = target * 8;
        let increase = (params.base_fee as i128)
            .saturating_mul(numerator)
            .saturating_div(denominator);
        params.base_fee = (params.base_fee as i128 + increase)
            .min(params.max_base_fee as i128)
            .max(params.min_base_fee as i128) as u128;
    } else if diff < 0 {
        // Decrease: base_fee * (1 - |diff|/target * coeff/8)
        let numerator = (-diff) * params.adjustment_coefficient as i128;
        let denominator = target * 8;
        let decrease = (params.base_fee as i128)
            .saturating_mul(numerator)
            .saturating_div(denominator);
        params.base_fee = (params.base_fee as i128 - decrease)
            .max(params.min_base_fee as i128) as u128;
    }
}
