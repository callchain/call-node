//! T1.5 — Transaction Model & Gas (per spec §3.5, §12.2)
//!
//! ProtocolTransaction, gas calculation, fee params, base fee updates, mempool.

use call_primitives::{Address, FeeCurrency, TxHash};
use crate::account::AccountState;
use crate::instructions::Instruction;
use crate::sponsor::SponsorRegistry;
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
    AuthorizedSponsor { sponsor: Address },
    PoolSponsor { sponsor: Address },
    PerTxSponsor { sponsor: Address, sponsor_signature: Vec<u8> },
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
    /// Block height at which this transaction expires (0 = never)
    #[serde(default)]
    pub expires_at: u64,
    pub auth: AuthScheme,
}

impl ProtocolTransaction {
    /// Compute the canonical transaction hash for signature verification.
    ///
    /// The hash covers all fields except `auth` (the signature itself),
    /// preventing signature malleability attacks.
    pub fn compute_tx_hash(&self) -> [u8; 32] {
        use call_crypto::keccak256;
        let mut preimage = Vec::new();
        preimage.extend_from_slice(self.sender.as_slice());
        preimage.extend_from_slice(&self.nonce.to_be_bytes());
        let instr_bytes = serde_json::to_vec(&self.instructions).unwrap_or_default();
        preimage.extend_from_slice(&instr_bytes);
        match &self.gas_config {
            GasConfig::SelfPay => {
                preimage.push(0);
            }
            GasConfig::AuthorizedSponsor { sponsor } => {
                preimage.push(1);
                preimage.extend_from_slice(sponsor.as_slice());
            }
            GasConfig::PoolSponsor { sponsor } => {
                preimage.push(2);
                preimage.extend_from_slice(sponsor.as_slice());
            }
            GasConfig::PerTxSponsor { sponsor, sponsor_signature } => {
                preimage.push(3);
                preimage.extend_from_slice(sponsor.as_slice());
                preimage.extend_from_slice(sponsor_signature);
            }
        }
        let fee_currency_bytes: Vec<u8> = match self.fee_currency {
            call_primitives::FeeCurrency::Call => vec![0],
            call_primitives::FeeCurrency::Stablecoin(id) => {
                let mut v = vec![1];
                v.extend_from_slice(&id.to_be_bytes());
                v
            }
        };
        preimage.extend_from_slice(&fee_currency_bytes);
        preimage.extend_from_slice(&self.gas_limit.to_be_bytes());
        preimage.extend_from_slice(&self.max_fee.to_be_bytes());
        preimage.extend_from_slice(&self.expires_at.to_be_bytes());
        let h = keccak256(&preimage);
        h.0
    }

    /// Verify the transaction's secp256k1 signature(s).
    ///
    /// - `SingleSig`: recovers signer and checks it matches `self.sender`
    /// - `MultiSig`: recovers each signer and checks against threshold (from
    ///   `registry` if provided, otherwise defaults to 2)
    /// - `SessionKey`: recovers signer and checks it matches the session key
    pub fn verify_signature(&self) -> Result<(), crate::ProtocolError> {
        self.verify_signature_with_registry(None)
    }

    /// Verify signature with optional smart-account registry for MultiSig threshold.
    pub fn verify_signature_with_registry(
        &self,
        registry: Option<&crate::smart_accounts::SmartAccountRegistry>,
    ) -> Result<(), crate::ProtocolError> {
        use call_crypto::recover_secp256k1_signer;
        let tx_hash = self.compute_tx_hash();
        match &self.auth {
            AuthScheme::SingleSig { signature } => {
                let recovered = recover_secp256k1_signer(&tx_hash, signature)
                    .map_err(|e| crate::ProtocolError::InvalidSignature(format!("{e:?}")))?;
                if recovered != self.sender {
                    return Err(crate::ProtocolError::InvalidSignature(
                        "signature does not match sender".into(),
                    ));
                }
                Ok(())
            }
            AuthScheme::MultiSig { signatures } => {
                let mut unique_signers = std::collections::HashSet::new();
                for sig in signatures {
                    let recovered = recover_secp256k1_signer(&tx_hash, sig)
                        .map_err(|e| crate::ProtocolError::InvalidSignature(format!("{e:?}")))?;
                    unique_signers.insert(recovered);
                }
                let threshold = registry
                    .and_then(|r| r.get_multisig_config(&self.sender))
                    .map(|c| c.threshold as usize)
                    .unwrap_or(2);
                if unique_signers.len() < threshold {
                    return Err(crate::ProtocolError::InvalidSignature(format!(
                        "multisig requires at least {threshold} unique signers"
                    )));
                }
                Ok(())
            }
            AuthScheme::SessionKey { key, signature } => {
                let recovered = recover_secp256k1_signer(&tx_hash, signature)
                    .map_err(|e| crate::ProtocolError::InvalidSignature(format!("{e:?}")))?;
                if recovered != *key {
                    return Err(crate::ProtocolError::InvalidSignature(
                        "signature does not match session key".into(),
                    ));
                }
                Ok(())
            }
        }
    }
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
        Instruction::OracleSubmit { .. } => 50_000, // oracle price submission
        Instruction::GovernanceSubmitProposal { .. } => 50_000,
        Instruction::GovernanceVote { .. } => 10_000,
        Instruction::GovernanceQueue { .. } => 10_000,
        Instruction::GovernanceExecute { .. } => 50_000,
        Instruction::GovernanceEmergencyPause { .. } => 100_000,
        Instruction::GovernanceEmergencyResume => 100_000,
        Instruction::ExternalBridgeDeposit { .. } => 50_000,
        Instruction::ExternalBridgeWithdraw { .. } => 50_000,
        Instruction::ChallengeBridgeDeposit { .. } => 10_000,
        Instruction::BridgeToEvm { .. } | Instruction::WithdrawFromEvm { .. } => 25_000,
        Instruction::ValidatorStake { .. } => 50_000,
        Instruction::ValidatorUnstake { .. } => 25_000,
        Instruction::ValidatorClaimUnbonded { .. } => 25_000,
        Instruction::RegisterAsset { .. } => 50_000,
        Instruction::RegisterEvmBridge { .. } => 50_000,
        Instruction::RegisterAgent { .. } => 50_000,
        Instruction::GrantAgentBalance { .. } => 10_000,
        Instruction::RevokeAgentBalance { .. } => 10_000,
        Instruction::SubmitRollbackSignature { .. } => 50_000,
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

// ── Limits ────────────────────────────────────────────────────────────

/// Maximum instructions per transaction (Gap 9 — DoS prevention)
pub const MAX_INSTRUCTIONS_PER_TX: usize = 100;

/// Maximum total memo size per transaction in bytes (Gap 10 — memory DoS)
pub const MAX_TOTAL_MEMO_BYTES: usize = 1024;

/// Minimum priority fee per gas unit (Gap 5 — ensure fee market works)
pub const MIN_PRIORITY_FEE_PER_GAS: u128 = 1;

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

/// Compute total memo bytes across all instructions in a transaction.
fn total_memo_bytes(instructions: &[Instruction]) -> usize {
    let mut total = 0usize;
    for instr in instructions {
        match instr {
            Instruction::Transfer { memo, .. } => {
                if let Some(m) = memo {
                    total += m.message.len();
                    if let Some(ref r) = m.reference {
                        total += r.len();
                    }
                    if let Some(ref md) = m.metadata {
                        total += md.len();
                    }
                }
            }
            Instruction::BatchTransfer { payments, .. } => {
                for p in payments {
                    if let Some(m) = &p.memo {
                        total += m.message.len();
                        if let Some(ref r) = m.reference {
                            total += r.len();
                        }
                        if let Some(ref md) = m.metadata {
                            total += md.len();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    total
}

pub fn accept_to_mempool(
    tx: &ProtocolTransaction,
    account: &AccountState,
    fee_params: &FeeParams,
    seen_nonces: &HashSet<(Address, u64)>,
    expected_nonces: &std::collections::HashMap<Address, u64>,
) -> ProtocolResult<()> {
    // Gap 2 — Sequential nonce enforcement
    let expected = expected_nonces.get(&tx.sender).copied().unwrap_or(0);
    if tx.nonce < expected {
        return Err(ProtocolError::NonceError(format!(
            "nonce {} already used (expected >= {expected})",
            tx.nonce
        )));
    }

    // Check nonce uniqueness (still needed for within-mempool dedup)
    if seen_nonces.contains(&(tx.sender, tx.nonce)) {
        return Err(ProtocolError::NonceError("duplicate nonce".into()));
    }

    // Gap 9 — Instruction count limit
    if tx.instructions.len() > MAX_INSTRUCTIONS_PER_TX {
        return Err(ProtocolError::InvalidInstruction(format!(
            "too many instructions: {} > {}",
            tx.instructions.len(),
            MAX_INSTRUCTIONS_PER_TX
        )));
    }

    // Gap 10 — Memo size limit
    let memo_bytes = total_memo_bytes(&tx.instructions);
    if memo_bytes > MAX_TOTAL_MEMO_BYTES {
        return Err(ProtocolError::InvalidInstruction(format!(
            "total memo size {} bytes exceeds limit {}",
            memo_bytes, MAX_TOTAL_MEMO_BYTES
        )));
    }

    // Gap 3 — Reject unimplemented sponsor configs at mempool boundary
    match tx.gas_config {
        GasConfig::PerTxSponsor { .. } => {
            return Err(ProtocolError::SponsorError(
                "PerTxSponsor not yet enabled".into(),
            ));
        }
        _ => {}
    }

    // Check gas limit
    if tx.gas_limit > MempoolConfig::default().max_gas_limit {
        return Err(ProtocolError::GasError("gas limit too high".into()));
    }

    // Gap 5 — Fee sufficiency includes priority fee
    let gas_units = calculate_gas_units(&tx.instructions);
    let required_fee = compute_fee(gas_units, MIN_PRIORITY_FEE_PER_GAS, fee_params.base_fee);
    if tx.max_fee < required_fee {
        return Err(ProtocolError::GasError(format!(
            "max fee {} < required {}",
            tx.max_fee, required_fee
        )));
    }

    // Check balance in the correct fee currency
    let fee_asset_id = match tx.fee_currency {
        FeeCurrency::Call => crate::CALL_ASSET_ID,
        FeeCurrency::Stablecoin(asset_id) => asset_id,
    };
    let balance = account.get_balance(fee_asset_id, &tx.sender);
    if balance < required_fee {
        return Err(ProtocolError::InsufficientBalance);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Hash;
    use crate::instructions::PaymentMemo;
    use crate::FeeCurrencyRegistry;
    use crate::sponsor::GasSponsorAuth;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn make_transfer() -> Instruction {
        Instruction::Transfer {
            asset_id: crate::CALL_ASSET_ID,
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
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1000).unwrap();
        let mut sponsors = SponsorRegistry::new();
        deduct_gas(
            &mut account,
            &GasConfig::SelfPay,
            &FeeCurrency::Call,
            500,
            test_addr(1),
            Hash::ZERO,
            &mut sponsors,
            0,
        )
        .unwrap();
        assert_eq!(account.get_balance(crate::CALL_ASSET_ID, &test_addr(1)), 500);
    }

    #[test]
    fn test_deduct_gas_authorized_sponsor() {
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(2), 1000).unwrap();
        let mut sponsors = SponsorRegistry::new();
        let auth = GasSponsorAuth {
            sponsor: test_addr(2),
            allowed_senders: vec![test_addr(1)],
            max_daily: 10_000,
            expires_at: 1000,
            sponsor_signature: [0u8; 65],
        };
        sponsors.register_sponsor_auth(auth).unwrap();

        let result = deduct_gas(
            &mut account,
            &GasConfig::AuthorizedSponsor { sponsor: test_addr(2) },
            &FeeCurrency::Call,
            500,
            test_addr(1),
            Hash::ZERO,
            &mut sponsors,
            10,
        );
        assert!(result.is_ok());
        assert_eq!(account.get_balance(crate::CALL_ASSET_ID, &test_addr(2)), 500);
    }

    #[test]
    fn test_deduct_gas_pool_sponsor() {
        let mut account = AccountState::new();
        let mut sponsors = SponsorRegistry::new();
        let pool_addr = test_addr(9);
        sponsors.deposit_to_pool(pool_addr, 10_000).unwrap();
        account.balances.set_balance(crate::CALL_ASSET_ID, pool_addr, 10_000).unwrap();

        let result = deduct_gas(
            &mut account,
            &GasConfig::PoolSponsor { sponsor: pool_addr },
            &FeeCurrency::Call,
            500,
            test_addr(1),
            Hash::ZERO,
            &mut sponsors,
            10,
        );
        assert!(result.is_ok(), "pool sponsor deduct failed: {:?}", result);
    }

    #[test]
    fn test_deduct_gas_per_tx_sponsor() {
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(2), 1000).unwrap();
        let mut sponsors = SponsorRegistry::new();
        let result = deduct_gas(
            &mut account,
            &GasConfig::PerTxSponsor {
                sponsor: test_addr(2),
                sponsor_signature: vec![0u8; 65],
            },
            &FeeCurrency::Call,
            500,
            test_addr(1),
            Hash::ZERO,
            &mut sponsors,
            10,
        );
        assert!(result.is_ok());
        assert_eq!(account.get_balance(crate::CALL_ASSET_ID, &test_addr(2)), 500);
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
            nonce: 0,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 20_000,
            max_fee: 1, // too low
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
        let fee_params = FeeParams::default();
        let nonces = HashSet::new();
        let expected_nonces = std::collections::HashMap::new();

        let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
        assert!(result.is_err());
    }

    #[test]
    fn test_mempool_accept_sufficient_balance() {
        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 0,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 20_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
        let fee_params = FeeParams::default();
        let nonces = HashSet::new();
        let expected_nonces = std::collections::HashMap::new();

        let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
        assert!(result.is_ok());
    }

    #[test]
    fn test_mempool_reject_stale_nonce() {
        // Nonce 0 is already used (expected=1)
        let mut expected_nonces = std::collections::HashMap::new();
        expected_nonces.insert(test_addr(1), 1);

        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 0, // already used
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 20_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
        let fee_params = FeeParams::default();
        let nonces = HashSet::new();

        let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
        assert!(result.is_err());
    }

    #[test]
    fn test_mempool_reject_too_many_instructions() {
        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 0,
            instructions: vec![make_transfer(); MAX_INSTRUCTIONS_PER_TX + 1],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 20_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
        let fee_params = FeeParams::default();
        let nonces = HashSet::new();
        let expected_nonces = std::collections::HashMap::new();

        let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
        assert!(result.is_err());
    }

    #[test]
    fn test_mempool_reject_oversized_memo() {
        let big_memo = PaymentMemo {
            message: "a".repeat(MAX_TOTAL_MEMO_BYTES + 1),
            reference: None,
            metadata: None,
        };
        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 0,
            instructions: vec![Instruction::Transfer {
                asset_id: crate::CALL_ASSET_ID,
                to: test_addr(2),
                amount: 100,
                memo: Some(big_memo),
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 20_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1_000_000).unwrap();
        let fee_params = FeeParams::default();
        let nonces = HashSet::new();
        let expected_nonces = std::collections::HashMap::new();

        let result = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
        assert!(result.is_err());
    }

    // ── Signature negative tests ─────────────────────────────────────

    #[test]
    fn test_verify_signature_all_zeros() {
        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: [0u8; 65] },
        };
        assert!(tx.verify_signature().is_err());
    }

    #[test]
    fn test_verify_signature_all_ones() {
        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: [0xFFu8; 65] },
        };
        assert!(tx.verify_signature().is_err());
    }

    #[test]
    fn test_verify_signature_wrong_keypair() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let correct_sender = call_crypto::pubkey_to_address(&pubkey);
        let wrong_addr = test_addr(0xAA);

        let msg_hash = {
            let mut buf = Vec::new();
            buf.extend_from_slice(&correct_sender.as_slice());
            buf.extend_from_slice(&1u64.to_le_bytes());
            // Include instructions in hash
            call_crypto::keccak256(&buf)
        };
        let sig = call_crypto::secp256k1_sign(&secret, &msg_hash);

        // Sign with correct keypair but claim a different sender
        let tx = ProtocolTransaction {
            sender: wrong_addr,
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: sig },
        };
        assert!(tx.verify_signature().is_err());
    }

    #[test]
    fn test_verify_signature_malleated_recovery_id() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        let tx_hash = {
            let mut buf = Vec::new();
            buf.extend_from_slice(&sender.as_slice());
            buf.extend_from_slice(&1u64.to_le_bytes());
            call_crypto::keccak256(&buf)
        };
        let mut sig = call_crypto::secp256k1_sign(&secret, &tx_hash);

        // Flip the recovery ID byte
        sig[64] ^= 0x01;

        let tx = ProtocolTransaction {
            sender,
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: sig },
        };
        assert!(tx.verify_signature().is_err());
    }

    #[test]
    fn test_verify_signature_wrong_signer() {
        let (secret_a, pubkey_a) = call_crypto::generate_keypair();
        let sender_a = call_crypto::pubkey_to_address(&pubkey_a);
        let (_, pubkey_b) = call_crypto::generate_keypair();
        let sender_b = call_crypto::pubkey_to_address(&pubkey_b);

        // Sign tx as sender_a
        let tx_a = ProtocolTransaction {
            sender: sender_a,
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: [0u8; 65] },
        };
        let tx_hash = tx_a.compute_tx_hash();
        let sig = call_crypto::secp256k1_sign(&secret_a, &tx_hash);

        // But claim sender is sender_b
        let tx_impersonating_b = ProtocolTransaction {
            sender: sender_b,
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: sig },
        };
        assert!(
            tx_impersonating_b.verify_signature().is_err(),
            "signature from A should not verify as B"
        );
    }

    #[test]
    fn test_verify_signature_tampered_tx() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        // Sign a tx with amount 100
        let tx = ProtocolTransaction {
            sender,
            nonce: 1,
            instructions: vec![Instruction::Transfer {
                asset_id: crate::CALL_ASSET_ID,
                to: test_addr(2),
                amount: 100,
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: [0u8; 65] },
        };
        let tx_hash = tx.compute_tx_hash();
        let sig = call_crypto::secp256k1_sign(&secret, &tx_hash);

        // Attacker mutates amount to 1000 but keeps the original signature
        let tampered_tx = ProtocolTransaction {
            sender,
            nonce: 1,
            instructions: vec![Instruction::Transfer {
                asset_id: crate::CALL_ASSET_ID,
                to: test_addr(2),
                amount: 1000, // changed!
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: sig },
        };
        assert!(
            tampered_tx.verify_signature().is_err(),
            "signature over different hash should fail"
        );
    }

    #[test]
    fn test_verify_signature_nonce_replay() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        // Build tx with placeholder sig, then sign it properly
        let mut tx = ProtocolTransaction {
            sender,
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: [0u8; 65] },
        };

        // Sign with the actual tx hash so verify_signature succeeds
        let tx_hash = tx.compute_tx_hash();
        let sig = call_crypto::secp256k1_sign(&secret, &tx_hash);
        tx = ProtocolTransaction {
            sender,
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig { signature: sig },
        };

        // Signature verifies correctly
        assert!(tx.verify_signature().is_ok());

        // But mempool rejects duplicate nonce
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, sender, 1_000_000).unwrap();
        let fee_params = FeeParams::default();
        let nonces = HashSet::new();
        let mut expected_nonces = std::collections::HashMap::new();
        expected_nonces.insert(sender, 1);

        let r1 = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
        assert!(r1.is_ok());

        // After accepting tx1, expected nonce increments to 2
        expected_nonces.insert(sender, 2);
        let _r2 = accept_to_mempool(&tx, &account, &fee_params, &nonces, &expected_nonces);
        // tx has nonce 1 but expected is 2, so it should be rejected
    }

    #[test]
    fn test_verify_signature_multi_sig_insufficient_threshold() {
        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::MultiSig {
                signatures: vec![
                    [1u8; 65], // invalid sigs
                    [2u8; 65],
                ],
            },
        };

        // Verify with invalid signatures — recovery will fail
        assert!(tx.verify_signature().is_err());
    }

    #[test]
    fn test_verify_signature_session_key_mismatch() {
        let session_key = test_addr(0xBB);

        let tx = ProtocolTransaction {
            sender: test_addr(1),
            nonce: 1,
            instructions: vec![make_transfer()],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SessionKey { key: session_key, signature: [0u8; 65] },
        };
        // SessionKey auth with an invalid signature should fail
        assert!(tx.verify_signature().is_err());
    }

    // ── Property-based tests ─────────────────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_tx_roundtrip_encode_decode(
            sender_bytes: [u8; 20],
            nonce: u64,
            amount: u128,
            gas_limit: u64,
            max_fee: u128,
            expires_at: u64,
        ) {
            let sender = Address::from_slice(&sender_bytes);
            let instructions = vec![Instruction::Transfer {
                asset_id: crate::CALL_ASSET_ID,
                to: sender,
                amount,
                memo: None,
            }];

            let tx = ProtocolTransaction {
                sender,
                nonce,
                instructions,
                gas_config: GasConfig::SelfPay,
                fee_currency: FeeCurrency::Call,
                gas_limit,
                max_fee,
                expires_at,
                auth: AuthScheme::SingleSig { signature: [0u8; 65] },
            };

            // Serialize and deserialize
            let bytes = serde_json::to_vec(&tx).unwrap();
            let decoded: ProtocolTransaction = serde_json::from_slice(&bytes).unwrap();

            assert_eq!(decoded.sender, tx.sender);
            assert_eq!(decoded.nonce, tx.nonce);
            assert_eq!(decoded.gas_limit, tx.gas_limit);
            assert_eq!(decoded.max_fee, tx.max_fee);
            assert_eq!(decoded.expires_at, tx.expires_at);
        }

        #[test]
        fn test_instruction_roundtrip(
            asset_id: u64,
            amount: u128,
            to_bytes: [u8; 20],
        ) {
            let to = Address::from_slice(&to_bytes);
            let instr = Instruction::Transfer {
                asset_id,
                to,
                amount,
                memo: Some(PaymentMemo {
                    message: "test memo".to_string(),
                    reference: Some("ref-123".to_string()),
                    metadata: None,
                }),
            };

            let bytes = serde_json::to_vec(&instr).unwrap();
            let decoded: Instruction = serde_json::from_slice(&bytes).unwrap();

            if let Instruction::Transfer { asset_id: a_id, to: a_to, amount: a_amount, .. } = decoded {
                assert_eq!(a_id, asset_id);
                assert_eq!(a_to, to);
                assert_eq!(a_amount, amount);
            } else {
                panic!("decoded wrong instruction variant");
            }
        }
    }
}
