//! Callchain Bridge Layer (per spec §5)
//!
//! - Internal bridge: deposit/withdraw between protocol balances and EVM
//! - External bridge: cross-chain deposit/withdraw with validator signatures

mod deposit;
mod withdraw;
mod external;

pub use deposit::*;
pub use withdraw::*;
pub use external::*;

use alloy_primitives::{Address, U256};
use call_primitives::AssetId;
use call_protocol::ProtocolError;
use thiserror::Error;

/// Bridge operation error
#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("insufficient protocol balance: asset={0}, need={1}")]
    InsufficientProtocolBalance(AssetId, u128),
    #[error("insufficient EVM balance: contract={0}, need={1}")]
    InsufficientEvmBalance(Address, U256),
    #[error("asset not registered: {0}")]
    AssetNotRegistered(AssetId),
    #[error("bridge paused for asset: {0}")]
    BridgePaused(AssetId),
    #[error("exceeds max per tx: asset={0}, amount={1}, limit={2}")]
    ExceedsMaxPerTx(AssetId, u128, u128),
    #[error("exceeds daily limit: asset={0}, daily_used={1}, limit={2}")]
    ExceedsDailyLimit(AssetId, u128, u128),
    #[error("insufficient bridge signatures: got={0}, required={1}")]
    InsufficientSignatures(u64, u64),
    #[error("invalid bridge signature at index {0}: {1}")]
    InvalidSignature(u64, String),
    #[error("bridge signature timeout: source_tx={0:?}")]
    SignatureTimeout(Option<String>),
    #[error("external bridge asset not allowed: {0}")]
    ExternalAssetNotAllowed(AssetId),
    #[error("EVM execution failed: {0}")]
    EvmExecutionFailed(String),
}

/// Bridge operation types (per spec §5.1)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum BridgeOp {
    DepositToEvm {
        asset_id: AssetId,
        from: Address,
        to: Address,
        amount: u128,
    },
    WithdrawToProtocol {
        asset_id: AssetId,
        from: Address,
        to: Address,
        amount: u128,
    },
}

impl BridgeOp {
    pub fn asset_id(&self) -> AssetId {
        match self {
            BridgeOp::DepositToEvm { asset_id, .. }
            | BridgeOp::WithdrawToProtocol { asset_id, .. } => *asset_id,
        }
    }

    pub fn amount(&self) -> u128 {
        match self {
            BridgeOp::DepositToEvm { amount, .. }
            | BridgeOp::WithdrawToProtocol { amount, .. } => *amount,
        }
    }
}

/// Pending bridge queue entry
#[derive(Debug, Clone)]
pub struct PendingBridgeOp {
    pub op: BridgeOp,
    pub submitted_at_block: u64,
    pub confirmations: u64,
}

/// Bridge state tracking (per spec §5.3)
#[derive(Debug, Default)]
pub struct BridgeStateManager {
    /// Pending deposit/withdraw operations awaiting completion
    pub pending_ops: Vec<PendingBridgeOp>,
    /// Total deposits per asset: AssetId -> total deposited
    pub total_deposits: std::collections::HashMap<AssetId, u128>,
    /// Total withdrawals per asset: AssetId -> total withdrawn
    pub total_withdrawals: std::collections::HashMap<AssetId, u128>,
    /// Daily usage per asset: AssetId -> total used today
    pub daily_usage: std::collections::HashMap<AssetId, u128>,
    /// Bridge paused assets
    pub paused_assets: std::collections::HashSet<AssetId>,
}

impl BridgeStateManager {
    /// Check if bridge is paused for an asset
    pub fn is_paused(&self, asset_id: AssetId) -> bool {
        self.paused_assets.contains(&asset_id)
    }

    /// Pause bridge for an asset
    pub fn pause_asset(&mut self, asset_id: AssetId) {
        self.paused_assets.insert(asset_id);
    }

    /// Unpause bridge for an asset
    pub fn unpause_asset(&mut self, asset_id: AssetId) {
        self.paused_assets.remove(&asset_id);
    }

    /// Check if a deposit/withdrawal would exceed per-tx limit
    pub fn check_per_tx_limit(&self, amount: u128, max_per_tx: u128) -> Result<(), BridgeError> {
        if amount > max_per_tx {
            return Err(BridgeError::ExceedsMaxPerTx(0, amount, max_per_tx));
        }
        Ok(())
    }

    /// Check daily limit and update usage
    pub fn check_and_update_daily_limit(
        &mut self,
        asset_id: AssetId,
        amount: u128,
        daily_limit: u128,
    ) -> Result<(), BridgeError> {
        let used = self.daily_usage.get(&asset_id).copied().unwrap_or(0);
        if used + amount > daily_limit {
            return Err(BridgeError::ExceedsDailyLimit(asset_id, used, daily_limit));
        }
        self.daily_usage.insert(asset_id, used + amount);
        Ok(())
    }

    /// Add a pending bridge operation
    pub fn add_pending_op(&mut self, op: BridgeOp, current_block: u64) {
        self.pending_ops.push(PendingBridgeOp {
            op,
            submitted_at_block: current_block,
            confirmations: 0,
        });
    }

    /// Record completed deposit
    pub fn record_deposit(&mut self, asset_id: AssetId, amount: u128) {
        let total = self.total_deposits.get(&asset_id).copied().unwrap_or(0);
        self.total_deposits.insert(asset_id, total + amount);
    }

    /// Record completed withdrawal
    pub fn record_withdrawal(&mut self, asset_id: AssetId, amount: u128) {
        let total = self.total_withdrawals.get(&asset_id).copied().unwrap_or(0);
        self.total_withdrawals.insert(asset_id, total + amount);
    }

    /// Clear pending ops that have been completed (same-block guarantee per spec §5.5)
    pub fn clear_completed_ops(&mut self) {
        self.pending_ops.clear();
    }

    /// Reset daily usage (called at start of each new day/block cycle)
    pub fn reset_daily_usage(&mut self) {
        self.daily_usage.clear();
    }
}

/// Bridge configuration (per spec §5.6)
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Maximum amount per single transaction per asset
    pub max_per_tx: u128,
    /// Daily limit per asset
    pub daily_limit_per_asset: u128,
    /// Minimum confirmations on Ethereum (default 12)
    pub eth_min_confirmations: u64,
    /// Bridge fee per operation
    pub bridge_fee: u128,
    /// Allowed assets for external bridge
    pub allowed_assets: Vec<AssetId>,
    /// Signature timeout in seconds (default 300)
    pub signature_timeout_secs: u64,
    /// Minimum validator signatures required (default 14 = 2/3 of 21)
    pub min_validator_signatures: u64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            max_per_tx: 1_000_000_000_000_000_000_000u128, // 1000 tokens (18 decimals)
            daily_limit_per_asset: 10_000_000_000_000_000_000_000u128, // 10K tokens
            eth_min_confirmations: 12,
            bridge_fee: 0,
            allowed_assets: vec![1], // CALL
            signature_timeout_secs: 300,
            min_validator_signatures: 14,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_op_accessors() {
        let op = BridgeOp::DepositToEvm {
            asset_id: 1,
            from: Address::ZERO,
            to: Address::ZERO,
            amount: 1000,
        };
        assert_eq!(op.asset_id(), 1);
        assert_eq!(op.amount(), 1000);

        let op = BridgeOp::WithdrawToProtocol {
            asset_id: 2,
            from: Address::ZERO,
            to: Address::ZERO,
            amount: 500,
        };
        assert_eq!(op.asset_id(), 2);
        assert_eq!(op.amount(), 500);
    }

    #[test]
    fn test_bridge_state_pending_ops() {
        let mut state = BridgeStateManager::default();
        let op = BridgeOp::DepositToEvm {
            asset_id: 1,
            from: Address::ZERO,
            to: Address::repeat_byte(1),
            amount: 1000,
        };
        state.add_pending_op(op, 100);
        assert_eq!(state.pending_ops.len(), 1);
        state.clear_completed_ops();
        assert_eq!(state.pending_ops.len(), 0);
    }

    #[test]
    fn test_bridge_state_deposit_tracking() {
        let mut state = BridgeStateManager::default();
        state.record_deposit(1, 500);
        state.record_deposit(1, 300);
        assert_eq!(state.total_deposits.get(&1), Some(&800));

        state.record_withdrawal(1, 200);
        assert_eq!(state.total_withdrawals.get(&1), Some(&200));
    }

    #[test]
    fn test_bridge_daily_limit() {
        let mut state = BridgeStateManager::default();
        assert!(state.check_and_update_daily_limit(1, 100, 500).is_ok());
        assert!(state.check_and_update_daily_limit(1, 200, 500).is_ok());
        // 100 + 200 = 300, next 300 would exceed 500
        assert!(state.check_and_update_daily_limit(1, 300, 500).is_err());
    }

    #[test]
    fn test_bridge_pause_unpause() {
        let mut state = BridgeStateManager::default();
        assert!(!state.is_paused(1));
        state.pause_asset(1);
        assert!(state.is_paused(1));
        state.unpause_asset(1);
        assert!(!state.is_paused(1));
    }

    #[test]
    fn test_bridge_config_defaults() {
        let config = BridgeConfig::default();
        assert_eq!(config.eth_min_confirmations, 12);
        assert_eq!(config.signature_timeout_secs, 300);
        assert_eq!(config.min_validator_signatures, 14);
        assert!(!config.allowed_assets.is_empty());
    }
}
