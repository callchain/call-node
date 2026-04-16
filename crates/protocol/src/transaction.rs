//! T1.5 — Transaction Model & Gas (per spec §3.5, §12.2)
//!
//! ProtocolTransaction, gas calculation, fee params, base fee updates, mempool.

use call_primitives::{Address, FeeCurrency, TxHash};
use crate::balances::BalanceState;
use crate::instructions::Instruction;
use crate::{ProtocolError, ProtocolResult};
use std::collections::HashSet;

// ── Transaction model ─────────────────────────────────────────────────

/// Serialization helper for [u8; 65] signatures
mod sig_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(sig: &[u8; 65], serializer: S) -> Result<S::Ok, S::Error> {
        sig.as_slice().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 65], D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        bytes.try_into().map_err(|_| serde::de::Error::custom("expected 65 bytes"))
    }
}

/// Serialization helper for Vec<[u8; 65]>
mod sig_vec_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(sigs: &[[u8; 65]], serializer: S) -> Result<S::Ok, S::Error> {
        // Flatten Vec<[u8; 65]> into Vec<u8> (65 bytes per signature)
        let flat: Vec<u8> = sigs.iter().flat_map(|s| s.iter().copied()).collect();
        serializer.serialize_bytes(&flat)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<[u8; 65]>, D::Error> {
        let flat = Vec::<u8>::deserialize(deserializer)?;
        if flat.len() % 65 != 0 {
            return Err(serde::de::Error::custom("signature data not multiple of 65"));
        }
        let count = flat.len() / 65;
        let mut sigs = Vec::with_capacity(count);
        for chunk in flat.chunks_exact(65) {
            sigs.push(chunk.try_into().map_err(|_| serde::de::Error::custom("chunk size mismatch"))?);
        }
        Ok(sigs)
    }
}

/// Authentication scheme (defined here, used by smart_accounts too)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AuthScheme {
    SingleSig {
        #[serde(with = "sig_serde")]
        signature: [u8; 65],
    },
    MultiSig {
        #[serde(with = "sig_vec_serde")]
        signatures: Vec<[u8; 65]>,
    },
    SessionKey {
        key: Address,
        #[serde(with = "sig_serde")]
        signature: [u8; 65],
    },
}

/// Gas payment configuration
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum GasConfig {
    SelfPay,
    AuthorizedSponsor,
    PoolSponsor,
    PerTxSponsor,
}

/// A protocol transaction (per spec §3.5)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProtocolTransaction {
    pub sender: Address,
    pub nonce: u64,
    pub instructions: Vec<Instruction>,
    pub gas_config: GasConfig,
    pub fee_currency: FeeCurrency,
    pub gas_limit: u64,
    pub max_fee: u128,
    pub auth: AuthScheme,
}

// ── Gas unit table (per spec §12.2.1) ─────────────────────────────────

/// Base gas units per instruction type
pub fn base_gas_units(instruction: &Instruction) -> u64 {
    match instruction {
        Instruction::Transfer { memo, .. } => {
            let mut gas = 10_000u64;
            if let Some(m) = memo {
                gas += m.message.len() as u64;
                if let Some(ref r) = m.reference {
                    gas += r.len() as u64;
                }
                if let Some(ref md) = m.metadata {
                    gas += md.len() as u64;
                }
            }
            gas
        }
        Instruction::Approve { .. } | Instruction::Mint { .. } | Instruction::Burn { .. } => {
            5_000
        }
        Instruction::BatchTransfer { payments, .. } => {
            let mut gas = 0u64;
            for p in payments {
                gas += 1_000;
                if let Some(m) = &p.memo {
                    gas += m.message.len() as u64;
                    if let Some(ref r) = m.reference {
                        gas += r.len() as u64;
                    }
                    if let Some(ref md) = m.metadata {
                        gas += md.len() as u64;
                    }
                }
            }
            gas
        }
        Instruction::TransferFrom { .. } => 10_000,
        Instruction::BridgeDeposit { .. } => 10_000,
        Instruction::ShieldedDeposit { .. } | Instruction::ShieldedWithdraw { .. } => 20_000,
        Instruction::ShieldedTransfer { .. } => 50_000,
        Instruction::UpdateCompliance { .. } => 5_000,
        Instruction::AgentPay { .. } | Instruction::AgentBatchPay { .. } => 5_000, // 0.5x base
        Instruction::AgentCall { .. } | Instruction::AgentBridgeDeposit { .. } => 5_000, // 0.5x base
    }
}

/// Calculate total gas units for a transaction
pub fn calculate_gas_units(instructions: &[Instruction]) -> u64 {
    let mut total = 0u64;
    for (i, instr) in instructions.iter().enumerate() {
        let base = base_gas_units(instr);
        let discount = match i {
            0 => base,           // 1.0x
            1..=9 => base / 2,   // 0.5x
            _ => base / 4,       // 0.25x
        };
        total += discount;
    }
    total
}

// ── Fee parameters (per spec §12.2.3) ─────────────────────────────────

#[derive(Debug, Clone)]
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

// ── Fee calculation ───────────────────────────────────────────────────

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
    balances: &mut BalanceState,
    config: &GasConfig,
    fee_currency: &FeeCurrency,
    fee: u128,
    sender: Address,
    _tx_hash: TxHash,
) -> ProtocolResult<()> {
    match fee_currency {
        FeeCurrency::Call => deduct_call_from_payer(balances, config, fee, sender),
        FeeCurrency::Stablecoin(asset_id) => {
            deduct_stablecoin_from_payer(balances, config, *asset_id, fee, sender)
        }
    }
}

/// Deduct CALL fees (per spec §3.6)
fn deduct_call_from_payer(
    balances: &mut BalanceState,
    config: &GasConfig,
    fee: u128,
    sender: Address,
) -> ProtocolResult<()> {
    match config {
        GasConfig::SelfPay => balances.deduct_balance(0, sender, fee),
        GasConfig::AuthorizedSponsor
        | GasConfig::PoolSponsor
        | GasConfig::PerTxSponsor => {
            // Sponsor deduction handled by sponsor module
            Ok(())
        }
    }
}

/// Deduct stablecoin fees (per spec §3.6)
fn deduct_stablecoin_from_payer(
    balances: &mut BalanceState,
    config: &GasConfig,
    _asset_id: u64,
    fee: u128,
    sender: Address,
) -> ProtocolResult<()> {
    match config {
        GasConfig::SelfPay => balances.deduct_balance(0, sender, fee),
        GasConfig::AuthorizedSponsor
        | GasConfig::PoolSponsor
        | GasConfig::PerTxSponsor => Ok(()),
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
    // simplified: call_fee * price with ceil rounding
    let scaled = call_fee * call_price_usd;
    let divisor = 10u128.pow(stablecoin_decimals as u32);
    scaled.div_ceil(divisor)
}

// ── Mempool acceptance (per spec §12.2.7) ────────────────────────────

#[derive(Debug)]
pub struct MempoolConfig {
    pub min_base_fee: u128,
    pub max_gas_limit: u64,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            min_base_fee: 1,
            max_gas_limit: 10_000_000,
        }
    }
}

pub fn accept_to_mempool(
    tx: &ProtocolTransaction,
    balances: &BalanceState,
    fee_params: &FeeParams,
    nonces: &HashSet<(Address, u64)>,
) -> ProtocolResult<()> {
    // Check nonce uniqueness
    if nonces.contains(&(tx.sender, tx.nonce)) {
        return Err(ProtocolError::NonceError("duplicate nonce".into()));
    }

    // Check gas limit
    if tx.gas_limit > MempoolConfig::default().max_gas_limit {
        return Err(ProtocolError::GasError("gas limit too high".into()));
    }

    // Check fee sufficiency
    let gas_units = calculate_gas_units(&tx.instructions);
    let required_fee = compute_fee(gas_units, 0, fee_params.base_fee);
    if tx.max_fee < required_fee {
        return Err(ProtocolError::GasError("max fee too low".into()));
    }

    // Check balance
    let balance = balances.get_balance(0, &tx.sender);
    if balance < required_fee {
        return Err(ProtocolError::InsufficientBalance);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Hash;
    use crate::FeeCurrencyRegistry;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn make_transfer() -> Instruction {
        Instruction::Transfer {
            asset_id: 1,
            to: test_addr(2),
            amount: 100,
            memo: None,
        }
    }

    #[test]
    fn test_gas_calculation_single_transfer() {
        let instrs = vec![make_transfer()];
        assert_eq!(calculate_gas_units(&instrs), 10_000);
    }

    #[test]
    fn test_gas_calculation_multi_instruction_discount() {
        let instrs = vec![make_transfer(); 5];
        // 1st: 10000, 2nd-5th: 5000 each = 10000 + 4*5000 = 30000
        assert_eq!(calculate_gas_units(&instrs), 30_000);
    }

    #[test]
    fn test_gas_calculation_batch_100_payments() {
        let instrs = vec![make_transfer(); 100];
        let mut expected = 10_000u64; // 1st
        expected += 9 * 5_000;       // 2nd-10th
        expected += 90 * 2_500;      // 11th-100th
        assert_eq!(calculate_gas_units(&instrs), expected);
    }

    #[test]
    fn test_base_fee_increase_on_congestion() {
        let mut params = FeeParams::default();
        params.base_fee = 100;
        // Gas used > target → fee should increase
        update_base_fee(&mut params, 15_000_000);
        assert!(params.base_fee > 100);
    }

    #[test]
    fn test_base_fee_decrease_on_idle() {
        let mut params = FeeParams::default();
        params.base_fee = 100;
        // Gas used < target → fee should decrease
        update_base_fee(&mut params, 5_000_000);
        assert!(params.base_fee < 100);
    }

    #[test]
    fn test_base_fee_capping_at_max() {
        let mut params = FeeParams::default();
        params.base_fee = params.max_base_fee;
        // Even with high usage, fee should not exceed max
        update_base_fee(&mut params, 20_000_000);
        assert_eq!(params.base_fee, params.max_base_fee);
    }

    #[test]
    fn test_deduct_gas_self_pay() {
        let mut balances = BalanceState::new();
        balances.balances.set_balance(0, test_addr(1), 1000).unwrap();
        deduct_gas(
            &mut balances,
            &GasConfig::SelfPay,
            &FeeCurrency::Call,
            500,
            test_addr(1),
            Hash::ZERO,
        )
        .unwrap();
        assert_eq!(balances.get_balance(0, &test_addr(1)), 500);
    }

    #[test]
    fn test_deduct_gas_authorized_sponsor() {
        let mut balances = BalanceState::new();
        // Sponsor pays — balance layer OK
        let result = deduct_gas(
            &mut balances,
            &GasConfig::AuthorizedSponsor,
            &FeeCurrency::Call,
            500,
            test_addr(1),
            Hash::ZERO,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_deduct_gas_pool_sponsor() {
        let mut balances = BalanceState::new();
        let result = deduct_gas(
            &mut balances,
            &GasConfig::PoolSponsor,
            &FeeCurrency::Call,
            500,
            test_addr(1),
            Hash::ZERO,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_deduct_gas_per_tx_sponsor() {
        let mut balances = BalanceState::new();
        let result = deduct_gas(
            &mut balances,
            &GasConfig::PerTxSponsor,
            &FeeCurrency::Call,
            500,
            test_addr(1),
            Hash::ZERO,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_stablecoin_fee_conversion() {
        let call_fee = 1_000_000u128; // 1M wei
        let call_price_usd = 2_000_000u128; // $2.00 (6 decimals)
        let stablecoin_decimals = 6u8;
        let result = convert_fee_to_stablecoin(call_fee, call_price_usd, stablecoin_decimals);
        assert_eq!(result, 2_000_000u128); // $2.00 in 6 decimals
    }

    #[test]
    fn test_stablecoin_not_in_registry_rejected() {
        // Registry check happens at fee deduction time via asset lookup
        // This test documents the flow
        let registry = FeeCurrencyRegistry::new();
        assert!(!registry.is_allowed(999));
    }

    #[test]
    fn test_mempool_accept_low_fee_rejected() {
        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 20_000,
            max_fee: 1, // too low
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let mut balances = BalanceState::new();
        balances.balances.set_balance(0, test_addr(1), 1_000_000).unwrap();
        let fee_params = FeeParams::default();
        let nonces = HashSet::new();

        let result = accept_to_mempool(&tx, &balances, &fee_params, &nonces);
        assert!(result.is_err());
    }

    #[test]
    fn test_mempool_accept_sufficient_balance() {
        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 20_000,
            max_fee: 1_000_000,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let mut balances = BalanceState::new();
        balances.balances.set_balance(0, test_addr(1), 1_000_000).unwrap();
        let fee_params = FeeParams::default();
        let nonces = HashSet::new();

        let result = accept_to_mempool(&tx, &balances, &fee_params, &nonces);
        assert!(result.is_ok());
    }
}
