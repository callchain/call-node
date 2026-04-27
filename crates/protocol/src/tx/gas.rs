//! Gas unit table and fee parameters.

use crate::instructions::Instruction;

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
        Instruction::BridgeToEvm { .. } | Instruction::BridgeToProtocol { .. } => 25_000,
        Instruction::EvmIssuerMint { .. } => 50_000,
        Instruction::ValidatorStake { .. } => 50_000,
        Instruction::ValidatorUnstake { .. } => 25_000,
        Instruction::ValidatorClaimUnbonded { .. } => 25_000,
        Instruction::RegisterAsset { .. } => 50_000,
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
