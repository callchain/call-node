//! Callchain Bridge Layer (per spec §5)
//!
//! - Internal bridge: deposit/withdraw between protocol balances and EVM
//! - External bridge: cross-chain deposit/withdraw with validator signatures

mod deposit;
mod external;
pub mod precompile;
mod withdraw;

pub use external::*;
pub use precompile::{BridgePrecompile, BRIDGE_ADDRESS};
pub use withdraw::*;

use alloy_primitives::{Address, B256, U256};
use call_primitives::AssetId;
use call_protocol::ProtocolError;
use thiserror::Error;

/// Default challenge period in blocks (≈14 days at 250ms block time).
pub const DEFAULT_CHALLENGE_PERIOD: u64 = 2_419_200;
/// Default withdraw rate-limit period in blocks.
pub const DEFAULT_WITHDRAW_PERIOD_BLOCKS: u64 = 2_419_200;
/// Default challenge bond amount.
pub const DEFAULT_CHALLENGE_BOND: u128 = 1_000;

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
    #[error("external bridge globally paused")]
    ExternalBridgePaused,
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
    #[error("unauthorized bridge contract: chain={0}, contract={1}")]
    UnauthorizedBridgeContract(u64, Address),
    #[error("bridge fee exceeds amount: fee={0}, amount={1}")]
    BridgeFeeExceedsAmount(u128, u128),
    #[error("EVM execution failed: {0}")]
    EvmExecutionFailed(String),
    #[error("MPT proof verification failed: {0}")]
    MptProofError(String),
    #[error("block {0} not consensus-verified by beacon chain")]
    NotConsensusVerified(u64),
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
            BridgeOp::DepositToEvm { amount, .. } | BridgeOp::WithdrawToProtocol { amount, .. } => {
                *amount
            }
        }
    }
}

/// Bridge event type for indexing
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum BridgeEventType {
    ExternalDepositQueued,
    ExternalDepositFinalized,
    ExternalDepositChallenged,
    ExternalWithdraw,
    InternalDeposit,
    InternalWithdraw,
}

/// Indexed bridge event for audit and verification
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BridgeEvent {
    pub event_type: BridgeEventType,
    pub source_tx_hash: Option<B256>,
    pub asset_id: AssetId,
    pub amount: u128,
    pub fee: u128,
    pub recipient: Option<Address>,
    pub block_height: u64,
}

/// Bridge configuration (per spec §5.6)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BridgeConfig {
    /// Maximum amount per single transaction per asset
    pub max_per_tx: u128,
    /// Daily limit per asset
    pub daily_limit_per_asset: u128,
    /// Minimum confirmations on Ethereum (default 12).
    /// Only used by the light-client-bridge feature; precompile path ignores this.
    pub eth_min_confirmations: u64,
    /// Bridge fee per operation
    pub bridge_fee: u128,
    /// Allowed assets for external bridge
    pub allowed_assets: Vec<AssetId>,
    /// Signature timeout in seconds (default 300)
    pub signature_timeout_secs: u64,
    /// Minimum validator signatures required (default 14 = 2/3 of 21)
    pub min_validator_signatures: u64,
    /// Challenge period in blocks before external deposits finalize.
    /// Default: 2_419_200 ≈ 14 days at 250ms block time.
    pub challenge_period_blocks: u64,
    /// Withdraw rate-limit period in blocks.
    /// Default: 2_419_200 ≈ 14 days at 250ms block time.
    pub withdraw_period_blocks: u64,
    /// Max external withdraw per asset per challenge period.
    /// Limits blast radius if validator keys are compromised.
    pub max_external_withdraw_per_period: u128,
    /// Blocks per day for daily limit auto-reset.
    /// Default: 345_600 ≈ 1 day at 250ms block time (4 blocks/sec).
    pub blocks_per_day: u64,
    /// Retention period for processed external tx hashes (replay protection pruning).
    /// Default: 4_838_400 ≈ 14 days at 250ms block time.
    /// Must be > challenge_period_blocks to prevent accidental replay.
    pub processed_tx_retention_blocks: u64,
    /// Challenge bond amount required to initiate a challenge.
    /// Default: 1_000.
    pub challenge_bond: u128,
    /// Authorized bridge contracts per external chain (chain_id -> contract addresses).
    /// Deposits must originate from one of these contracts.
    /// Empty by default — must be configured by governance before use.
    pub authorized_contracts: std::collections::HashMap<u64, Vec<Address>>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            max_per_tx: 1_000_000_000_000_000_000_000u128,
            daily_limit_per_asset: 10_000_000_000_000_000_000_000u128,
            eth_min_confirmations: 12,
            bridge_fee: 0,
            allowed_assets: vec![1],
            signature_timeout_secs: 300,
            min_validator_signatures: 14,
            challenge_period_blocks: 2_419_200,
            withdraw_period_blocks: 2_419_200,
            max_external_withdraw_per_period: 5_000_000_000_000_000_000_000u128,
            blocks_per_day: 345_600,
            processed_tx_retention_blocks: 4_838_400,
            challenge_bond: 1_000,
            authorized_contracts: std::collections::HashMap::new(),
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
    fn test_bridge_config_defaults() {
        let config = BridgeConfig::default();
        assert_eq!(config.eth_min_confirmations, 12);
        assert_eq!(config.signature_timeout_secs, 300);
        assert_eq!(config.min_validator_signatures, 14);
        assert!(!config.allowed_assets.is_empty());
    }
}
