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
    /// Percentage of block fees allocated to proposer validator rewards (basis points)
    pub validator_fee_share_bps: u16,
}

impl Default for FeeParams {
    fn default() -> Self {
        Self {
            base_fee: 10,                     // 10 wei initial
            target_gas_per_block: 10_000_000, // 10M
            max_gas_per_block: 20_000_000,    // 20M
            adjustment_coefficient: 1,        // numerator (denominator = 8)
            min_base_fee: 1,                  // 1 wei
            max_base_fee: 1_000_000_000,      // 1B wei
            initial_base_fee: 10,             // 10 wei
            oracle_fee_share_bps: 100,        // 1% of block fees to oracle rewards
            validator_fee_share_bps: 0,       // 0% to validator rewards by default
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
        params.base_fee = (params.base_fee as i128)
            .saturating_add(increase)
            .min(params.max_base_fee as i128)
            .max(params.min_base_fee as i128) as u128;
    } else if diff < 0 {
        // Decrease: base_fee * (1 - |diff|/target * coeff/8)
        let numerator = (-diff) * params.adjustment_coefficient as i128;
        let denominator = target * 8;
        let decrease = (params.base_fee as i128)
            .saturating_mul(numerator)
            .saturating_div(denominator);
        params.base_fee = (params.base_fee as i128)
            .saturating_sub(decrease)
            .max(params.min_base_fee as i128) as u128;
    }
}

// ─── Tests ─────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_priority_fee_constant() {
        assert_eq!(MIN_PRIORITY_FEE_PER_GAS, 1);
    }

    #[test]
    fn test_fee_params_default() {
        let p = FeeParams::default();
        assert_eq!(p.base_fee, 10);
        assert_eq!(p.target_gas_per_block, 10_000_000);
        assert_eq!(p.max_gas_per_block, 20_000_000);
        assert_eq!(p.adjustment_coefficient, 1);
        assert_eq!(p.min_base_fee, 1);
        assert_eq!(p.max_base_fee, 1_000_000_000);
        assert_eq!(p.initial_base_fee, 10);
        assert_eq!(p.oracle_fee_share_bps, 100);
        assert_eq!(p.validator_fee_share_bps, 0);
    }

    #[test]
    fn test_update_base_fee_no_change_when_exact_target() {
        let mut p = FeeParams::default();
        let target = p.target_gas_per_block;
        update_base_fee(&mut p, target);
        assert_eq!(p.base_fee, 10);
    }

    #[test]
    fn test_update_base_fee_increases_when_above_target() {
        let mut p = FeeParams {
            base_fee: 80,
            ..FeeParams::default()
        };
        let target = p.target_gas_per_block;
        update_base_fee(&mut p, target + 5_000_000);
        // diff = 5M, increase = 80 * 5M / 80M = 5
        assert!(p.base_fee > 80, "base fee should increase when gas_used > target");
    }

    #[test]
    fn test_update_base_fee_decreases_when_below_target() {
        let mut p = FeeParams {
            base_fee: 80,
            ..FeeParams::default()
        };
        let target = p.target_gas_per_block;
        update_base_fee(&mut p, target - 5_000_000);
        // diff = -5M, decrease = 80 * 5M / 80M = 5
        assert!(p.base_fee < 80, "base fee should decrease when gas_used < target");
    }

    #[test]
    fn test_update_base_fee_clamps_to_max() {
        let mut p = FeeParams {
            base_fee: 999_999_990,
            target_gas_per_block: 10_000_000,
            max_gas_per_block: 20_000_000,
            adjustment_coefficient: 1,
            min_base_fee: 1,
            max_base_fee: 1_000_000_000,
            initial_base_fee: 10,
            oracle_fee_share_bps: 0,
            validator_fee_share_bps: 0,
        };
        update_base_fee(&mut p, 20_000_000); // max gas
        assert_eq!(p.base_fee, 1_000_000_000, "base fee should clamp to max_base_fee");
    }

    #[test]
    fn test_update_base_fee_clamps_to_min() {
        let mut p = FeeParams {
            base_fee: 2,
            target_gas_per_block: 10_000_000,
            max_gas_per_block: 20_000_000,
            adjustment_coefficient: 8, // max coefficient so decrease = 2 * 10M * 8 / 80M = 2
            min_base_fee: 1,
            max_base_fee: 1_000_000_000,
            initial_base_fee: 10,
            oracle_fee_share_bps: 0,
            validator_fee_share_bps: 0,
        };
        update_base_fee(&mut p, 0); // zero gas
        assert_eq!(p.base_fee, 1, "base fee should clamp to min_base_fee");
    }

    #[test]
    fn test_update_base_fee_zero_gas_used() {
        let mut p = FeeParams::default();
        update_base_fee(&mut p, 0);
        // base_fee * (1 - target/target * 1/8) = base_fee * 7/8
        assert_eq!(p.base_fee, 9, "10 * 7/8 = 8.75 floored to 9 via i128 math");
    }

    #[test]
    fn test_update_base_fee_max_gas_used() {
        let mut p = FeeParams::default();
        let max = p.max_gas_per_block;
        update_base_fee(&mut p, max);
        // diff = 10M, numerator = 10M, denominator = 80M
        // increase = 10 * 10M / 80M = 1.25 -> 1
        assert_eq!(p.base_fee, 11);
    }

    #[test]
    fn test_update_base_fee_double_target() {
        let mut p = FeeParams::default();
        // gas_used = 2 * target
        update_base_fee(&mut p, 20_000_000);
        // diff = 10M, increase = 10 * 10M / 80M = 1
        assert_eq!(p.base_fee, 11);
    }

    #[test]
    fn test_update_base_fee_half_target() {
        let mut p = FeeParams::default();
        update_base_fee(&mut p, 5_000_000);
        // diff = -5M, decrease = 10 * 5M / 80M = 0
        assert_eq!(p.base_fee, 10);
    }

    #[test]
    fn test_update_base_fee_with_higher_coefficient() {
        let mut p = FeeParams {
            base_fee: 80,
            target_gas_per_block: 10_000_000,
            max_gas_per_block: 20_000_000,
            adjustment_coefficient: 2,
            min_base_fee: 1,
            max_base_fee: 1_000_000_000,
            initial_base_fee: 10,
            oracle_fee_share_bps: 0,
            validator_fee_share_bps: 0,
        };
        update_base_fee(&mut p, 20_000_000);
        // diff = 10M, numerator = 20M, denominator = 80M
        // increase = 80 * 20M / 80M = 20
        assert_eq!(p.base_fee, 100);
    }

    #[test]
    fn test_consecutive_blocks_converge_to_target() {
        let mut p = FeeParams {
            base_fee: 1_000,
            target_gas_per_block: 10_000_000,
            max_gas_per_block: 20_000_000,
            adjustment_coefficient: 1,
            min_base_fee: 1,
            max_base_fee: 1_000_000_000,
            initial_base_fee: 10,
            oracle_fee_share_bps: 0,
            validator_fee_share_bps: 0,
        };

        // Two full blocks in a row
        update_base_fee(&mut p, 20_000_000);
        let after_first = p.base_fee;
        update_base_fee(&mut p, 20_000_000);
        let after_second = p.base_fee;
        assert!(after_second > after_first, "fee should keep rising with full blocks");

        // Then two empty blocks
        update_base_fee(&mut p, 0);
        let after_empty = p.base_fee;
        assert!(after_empty < after_second, "fee should drop with empty blocks");

        update_base_fee(&mut p, 0);
        let after_empty2 = p.base_fee;
        assert!(after_empty2 < after_empty, "fee should keep dropping");
    }

    #[test]
    fn test_base_fee_never_overflows_or_panics() {
        let mut p = FeeParams {
            base_fee: u128::MAX / 2,
            target_gas_per_block: 10_000_000,
            max_gas_per_block: 20_000_000,
            adjustment_coefficient: 1,
            min_base_fee: 1,
            max_base_fee: u128::MAX,
            initial_base_fee: 10,
            oracle_fee_share_bps: 0,
            validator_fee_share_bps: 0,
        };
        // Should not panic despite large base_fee
        update_base_fee(&mut p, 20_000_000);
        // Because of saturating_mul, the result should be clamped reasonably
        assert!(p.base_fee >= 1);
    }

    #[test]
    fn test_base_fee_stability_at_target() {
        let mut p = FeeParams::default();
        let target = p.target_gas_per_block;
        // Run many blocks exactly at target
        for _ in 0..100 {
            update_base_fee(&mut p, target);
        }
        assert_eq!(p.base_fee, 10, "fee should remain stable when demand = target");
    }
}
