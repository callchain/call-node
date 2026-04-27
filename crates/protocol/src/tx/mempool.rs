//! Mempool configuration and acceptance logic.

use std::collections::HashSet;
use call_primitives::{Address, FeeCurrency};
use crate::account::AccountState;
use crate::{ProtocolError, ProtocolResult};
use crate::tx::model::{GasConfig, ProtocolTransaction};
use crate::tx::gas::{calculate_gas_units, FeeParams};
use crate::tx::fee::compute_fee;

/// Maximum instructions per transaction (Gap 9 — DoS prevention)
pub const MAX_INSTRUCTIONS_PER_TX: usize = 100;

/// Maximum total memo size per transaction in bytes (Gap 10 — memory DoS)
pub const MAX_TOTAL_MEMO_BYTES: usize = 1024;

/// Minimum priority fee per gas unit (Gap 5 — ensure fee market works)
pub const MIN_PRIORITY_FEE_PER_GAS: u128 = 1;

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
fn total_memo_bytes(instructions: &[crate::instructions::Instruction]) -> usize {
    let mut total = 0usize;
    for instr in instructions {
        match instr {
            crate::instructions::Instruction::Transfer { memo, .. } => {
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
            crate::instructions::Instruction::BatchTransfer { payments, .. } => {
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
