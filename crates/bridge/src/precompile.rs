//! Bridge precompile entry point (0x103).
//!
//! Thin wrapper that routes EVM calls to [`BridgeStorage`].
//! Business logic lives in [`BridgeStorage`]; this file only handles
//! ABI decode/encode, gas accounting and selector dispatch.

use alloy_sol_types::{sol, SolCall};
use call_asset::AssetStorage;
use call_precompile::{
    address_to_u256, check_compliance, dispatch, is_validator, require_caller, slot_asset_meta,
    storage::storage_slot,
    u128_to_u256, u256_to_address, u256_to_u128, u256_to_u64, u64_to_u256, StorageRef,
    ASSET_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;
use call_validator::ValidatorStorage;
use revm_precompile::{PrecompileError, PrecompileResult};

#[cfg(feature = "light-client-bridge")]
use crate::external::types::{FraudProof, FraudProofType};
#[cfg(feature = "light-client-bridge")]
use call_light_client::{
    parse_bridge_event_from_logs, parse_receipt_logs, rlp_encode_u64, verify_mpt_proof,
};

pub const BRIDGE_ADDRESS: Address =
    alloy_primitives::address!("0000000000000000000000000000000000000103");

pub const CALL_ASSET_ID: u64 = 1;
/// Maximum proof length in bytes for `initiate_challenge`.
/// 16 KiB bounds gas cost of storing proof chunks.
pub const MAX_PROOF_LEN: usize = 16 * 1024;

/// Cached defaults from `BridgeConfig::default()` to avoid repeated allocation.
const DEFAULT_MAX_PER_TX: u128 = 1_000_000_000_000_000_000_000u128;
const DEFAULT_DAILY_LIMIT: u128 = 10_000_000_000_000_000_000_000u128;
const DEFAULT_BLOCKS_PER_DAY: u64 = 345_600;
const DEFAULT_MAX_WITHDRAW_PER_PERIOD: u128 = 5_000_000_000_000_000_000_000u128;
const DEFAULT_BRIDGE_FEE: u128 = 0;
const DEFAULT_PROCESSED_RETENTION: u64 = 4_838_400;

// ── Error type ────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum BridgeError {
    AssetNotRegistered,
    BridgePaused,
    AssetZeroNotBridgeable,
    AlreadyProcessed,
    NotProcessed,
    AlreadyChallenged,
    ChallengePeriodExpired,
    ChallengeNotPending,
    ChallengeDeadlineNotReached,
    NotChallenger,
    ChallengeNotSuccessful,
    InsufficientBalance,
    BalanceOverflow,
    InvalidInput,
    ExceedsMaxPerTx(u64, u128, u128),
    ExceedsDailyLimit(u64, u128, u128),
    ExternalAssetNotAllowed(u64),
    UnauthorizedBridgeContract(u64, Address),
    ExceedsWithdrawLimit(u64, u128, u128),
    BridgeFeeExceedsAmount(u128, u128),
    UnauthorizedResolver(Address),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::AssetNotRegistered => write!(f, "asset not registered"),
            BridgeError::BridgePaused => write!(f, "bridge paused"),
            BridgeError::AssetZeroNotBridgeable => write!(f, "asset 0 not bridgeable"),
            BridgeError::AlreadyProcessed => write!(f, "source tx already processed"),
            BridgeError::NotProcessed => write!(f, "source tx not processed"),
            BridgeError::AlreadyChallenged => write!(f, "challenge already exists"),
            BridgeError::ChallengePeriodExpired => write!(f, "challenge period expired"),
            BridgeError::ChallengeNotPending => write!(f, "challenge not pending"),
            BridgeError::ChallengeDeadlineNotReached => write!(f, "challenge deadline not reached"),
            BridgeError::NotChallenger => write!(f, "not challenger"),
            BridgeError::ChallengeNotSuccessful => write!(f, "challenge not successful"),
            BridgeError::InsufficientBalance => write!(f, "insufficient balance"),
            BridgeError::BalanceOverflow => write!(f, "balance overflow"),
            BridgeError::InvalidInput => write!(f, "invalid input"),
            BridgeError::ExceedsMaxPerTx(asset_id, amount, limit) => {
                write!(
                    f,
                    "exceeds max per tx: asset={asset_id}, amount={amount}, limit={limit}"
                )
            }
            BridgeError::ExceedsDailyLimit(asset_id, used, limit) => {
                write!(
                    f,
                    "exceeds daily limit: asset={asset_id}, daily_used={used}, limit={limit}"
                )
            }
            BridgeError::ExternalAssetNotAllowed(asset_id) => {
                write!(f, "external bridge asset not allowed: {asset_id}")
            }
            BridgeError::UnauthorizedBridgeContract(chain_id, contract) => {
                write!(
                    f,
                    "unauthorized bridge contract: chain={chain_id}, contract={contract}"
                )
            }
            BridgeError::ExceedsWithdrawLimit(asset_id, withdrawn, limit) => {
                write!(
                    f,
                    "exceeds withdraw limit: asset={asset_id}, period_withdrawn={withdrawn}, limit={limit}"
                )
            }
            BridgeError::BridgeFeeExceedsAmount(fee, amount) => {
                write!(f, "bridge fee exceeds amount: fee={fee}, amount={amount}")
            }
            BridgeError::UnauthorizedResolver(addr) => {
                write!(f, "unauthorized resolver: {addr}")
            }
        }
    }
}

impl std::error::Error for BridgeError {}

impl From<call_asset::AssetError> for BridgeError {
    fn from(e: call_asset::AssetError) -> Self {
        match e {
            call_asset::AssetError::InsufficientBalance => BridgeError::InsufficientBalance,
            call_asset::AssetError::BalanceOverflow => BridgeError::BalanceOverflow,
            _ => BridgeError::InvalidInput,
        }
    }
}

// ── Storage slot helpers ──────────────────────────────────────────────

fn slot_bridge_total_deposits() -> U256 {
    U256::from(0)
}

fn slot_bridge_total_withdrawals() -> U256 {
    U256::from(1)
}

fn slot_bridge_paused() -> U256 {
    storage_slot(&[b"paused"])
}

fn slot_bridge_processed(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"processed", &tx_hash])
}

fn slot_bridge_challenge_status(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"challenge_status", &tx_hash])
}

fn slot_bridge_challenge_challenger(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"challenge_challenger", &tx_hash])
}

fn slot_bridge_challenge_deadline(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"challenge_deadline", &tx_hash])
}

fn slot_bridge_challenge_bond(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"challenge_bond", &tx_hash])
}

fn slot_bridge_challenge_proof_hash(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"challenge_proof_hash", &tx_hash])
}

fn slot_bridge_challenge_proof_len(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"challenge_proof_len", &tx_hash])
}

fn slot_bridge_challenge_original_validator(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"challenge_validator", &tx_hash])
}

fn slot_bridge_deposit_asset_id(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"deposit_asset_id", &tx_hash])
}

fn slot_bridge_deposit_recipient(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"deposit_recipient", &tx_hash])
}

fn slot_bridge_deposit_amount(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"deposit_amount", &tx_hash])
}

fn slot_bridge_deposit_block_height(tx_hash: [u8; 32]) -> U256 {
    storage_slot(&[b"deposit_block", &tx_hash])
}

fn slot_challenge_period() -> U256 {
    storage_slot(&[b"challenge_period"])
}

fn slot_challenge_bond_amount() -> U256 {
    storage_slot(&[b"challenge_bond"])
}

fn slot_bridge_challenge_proof_chunk(tx_hash: [u8; 32], chunk_index: u64) -> U256 {
    storage_slot(&[b"challenge_proof", &tx_hash, &chunk_index.to_be_bytes()])
}

fn slot_bridge_max_per_tx(asset_id: u64) -> U256 {
    storage_slot(&[b"max_per_tx", &asset_id.to_be_bytes()])
}

fn slot_bridge_daily_limit(asset_id: u64) -> U256 {
    storage_slot(&[b"daily_limit", &asset_id.to_be_bytes()])
}

fn slot_bridge_daily_used(asset_id: u64, day: u64) -> U256 {
    storage_slot(&[b"daily_used", &asset_id.to_be_bytes(), &day.to_be_bytes()])
}

fn slot_bridge_blocks_per_day() -> U256 {
    storage_slot(&[b"blocks_per_day"])
}

fn slot_bridge_asset_allowed(asset_id: u64) -> U256 {
    storage_slot(&[b"asset_allowed", &asset_id.to_be_bytes()])
}

fn slot_bridge_authorized_contract(chain_id: u64, contract: Address) -> U256 {
    storage_slot(&[b"authorized", &chain_id.to_be_bytes(), contract.as_slice()])
}

fn slot_bridge_max_withdraw_per_period() -> U256 {
    storage_slot(&[b"max_withdraw_period"])
}

fn slot_bridge_period_withdrawn(asset_id: u64) -> U256 {
    storage_slot(&[b"period_withdrawn", &asset_id.to_be_bytes()])
}

fn slot_bridge_withdraw_period() -> U256 {
    storage_slot(&[b"withdraw_period"])
}

fn slot_bridge_bridge_fee() -> U256 {
    storage_slot(&[b"bridge_fee"])
}

fn slot_bridge_current_period_start() -> U256 {
    storage_slot(&[b"period_start"])
}

fn slot_bridge_processed_retention() -> U256 {
    storage_slot(&[b"processed_retention"])
}

// ── Challenge status ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeStatus {
    None = 0,
    Pending = 1,
    Successful = 2,
    Failed = 3,
    Withdrawn = 4,
}

/// Result of checking the withdraw limit; used for split-phase validation + write.
enum WithdrawLimitCheck {
    /// New period — reset counters on record.
    NewPeriod,
    /// Existing period — increment by the new total.
    ExistingPeriod { new_total: u128 },
}

// ── BridgeStorage ─────────────────────────────────────────────────────

/// Business logic for bridge operations backed by any StorageBackend.
pub struct BridgeStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> BridgeStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    // ── Read operations ───────────────────────────────────────────────

    pub fn get_total_deposits(&mut self) -> u128 {
        u256_to_u128(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_total_deposits()),
        )
    }

    pub fn get_total_withdrawals(&mut self) -> u128 {
        u256_to_u128(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_total_withdrawals()),
        )
    }

    pub fn is_paused(&mut self) -> bool {
        self.backend
            .load(BRIDGE_ADDRESS, slot_bridge_paused())
            .to_be_bytes::<32>()[31]
            != 0
    }

    pub fn is_processed(&mut self, tx_hash: [u8; 32], current_block: u64) -> bool {
        let stored = self.backend.load(BRIDGE_ADDRESS, slot_bridge_processed(tx_hash));
        if stored == U256::ZERO {
            return false;
        }
        let processed_at = u256_to_u64(stored);
        let retention = self.read_processed_retention_blocks();
        current_block.saturating_sub(processed_at) <= retention
    }

    pub fn read_challenge_status(&mut self, tx_hash: [u8; 32]) -> ChallengeStatus {
        let status = self
            .backend
            .load(BRIDGE_ADDRESS, slot_bridge_challenge_status(tx_hash))
            .to_be_bytes::<32>()[31];
        match status {
            1 => ChallengeStatus::Pending,
            2 => ChallengeStatus::Successful,
            3 => ChallengeStatus::Failed,
            4 => ChallengeStatus::Withdrawn,
            _ => ChallengeStatus::None,
        }
    }

    pub fn read_challenge_deadline(&mut self, tx_hash: [u8; 32]) -> u64 {
        u256_to_u64(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_challenge_deadline(tx_hash)),
        )
    }

    pub fn read_challenge_bond_for_tx(&mut self, tx_hash: [u8; 32]) -> u128 {
        u256_to_u128(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_challenge_bond(tx_hash)),
        )
    }

    pub fn read_challenge_challenger(&mut self, tx_hash: [u8; 32]) -> Address {
        u256_to_address(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_challenge_challenger(tx_hash)),
        )
    }

    pub fn read_deposit_asset_id(&mut self, tx_hash: [u8; 32]) -> u64 {
        u256_to_u64(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_deposit_asset_id(tx_hash)),
        )
    }

    pub fn read_deposit_recipient(&mut self, tx_hash: [u8; 32]) -> Address {
        u256_to_address(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_deposit_recipient(tx_hash)),
        )
    }

    pub fn read_deposit_amount(&mut self, tx_hash: [u8; 32]) -> u128 {
        u256_to_u128(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_deposit_amount(tx_hash)),
        )
    }

    pub fn read_deposit_block_height(&mut self, tx_hash: [u8; 32]) -> u64 {
        u256_to_u64(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_deposit_block_height(tx_hash)),
        )
    }

    /// Read a u64 config value with offset+1 encoding: 0 means "use default",
    /// any non-zero value means `stored - 1`. This allows 0 to be explicitly set.
    fn load_u64_or_default(&mut self, slot: U256, default: u64) -> u64 {
        let stored = u256_to_u64(self.backend.load(BRIDGE_ADDRESS, slot));
        if stored == 0 {
            default
        } else {
            stored - 1
        }
    }

    /// Read a u128 config value with offset+1 encoding: 0 means "use default",
    /// any non-zero value means `stored - 1`. This allows 0 to be explicitly set.
    fn load_u128_or_default(&mut self, slot: U256, default: u128) -> u128 {
        let stored = u256_to_u128(self.backend.load(BRIDGE_ADDRESS, slot));
        if stored == 0 {
            default
        } else {
            stored - 1
        }
    }

    pub fn read_challenge_period(&mut self) -> u64 {
        self.load_u64_or_default(slot_challenge_period(), crate::DEFAULT_CHALLENGE_PERIOD)
    }

    pub fn read_global_challenge_bond(&mut self) -> u128 {
        self.load_u128_or_default(slot_challenge_bond_amount(), crate::DEFAULT_CHALLENGE_BOND)
    }

    fn read_max_per_tx(&mut self, asset_id: u64) -> u128 {
        self.load_u128_or_default(slot_bridge_max_per_tx(asset_id), DEFAULT_MAX_PER_TX)
    }

    fn read_daily_limit(&mut self, asset_id: u64) -> u128 {
        self.load_u128_or_default(slot_bridge_daily_limit(asset_id), DEFAULT_DAILY_LIMIT)
    }

    fn read_blocks_per_day(&mut self) -> u64 {
        self.load_u64_or_default(slot_bridge_blocks_per_day(), DEFAULT_BLOCKS_PER_DAY)
    }

    fn read_asset_allowed(&mut self, asset_id: u64) -> bool {
        let stored = self
            .backend
            .load(BRIDGE_ADDRESS, slot_bridge_asset_allowed(asset_id));
        if stored == U256::ZERO {
            asset_id == CALL_ASSET_ID // default: only asset 1 is allowed
        } else {
            stored.to_be_bytes::<32>()[31] != 0
        }
    }

    fn read_authorized_contract(&mut self, _chain_id: u64, _contract: Address) -> bool {
        let stored = self
            .backend
            .load(BRIDGE_ADDRESS, slot_bridge_authorized_contract(_chain_id, _contract));
        if stored == U256::ZERO {
            // Default: no contracts authorized (must be configured by governance)
            false
        } else {
            stored.to_be_bytes::<32>()[31] != 0
        }
    }

    fn read_max_external_withdraw_per_period(&mut self) -> u128 {
        self.load_u128_or_default(
            slot_bridge_max_withdraw_per_period(),
            DEFAULT_MAX_WITHDRAW_PER_PERIOD,
        )
    }

    fn read_withdraw_period(&mut self) -> u64 {
        self.load_u64_or_default(
            slot_bridge_withdraw_period(),
            crate::DEFAULT_WITHDRAW_PERIOD_BLOCKS,
        )
    }

    fn read_bridge_fee(&mut self) -> u128 {
        self.load_u128_or_default(slot_bridge_bridge_fee(), DEFAULT_BRIDGE_FEE)
    }

    fn read_processed_retention_blocks(&mut self) -> u64 {
        self.load_u64_or_default(
            slot_bridge_processed_retention(),
            DEFAULT_PROCESSED_RETENTION,
        )
    }

    fn read_daily_used(&mut self, asset_id: u64, day: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_daily_used(asset_id, day)),
        )
    }

    /// Validate limits without writing state. Returns the `day` index so the
    /// caller can record consumption after all fallible operations succeed.
    fn check_limits(
        &mut self,
        asset_id: u64,
        amount: u128,
        block_height: u64,
    ) -> Result<u64, BridgeError> {
        let max_per_tx = self.read_max_per_tx(asset_id);
        if amount > max_per_tx {
            return Err(BridgeError::ExceedsMaxPerTx(asset_id, amount, max_per_tx));
        }

        let blocks_per_day = self.read_blocks_per_day();
        if blocks_per_day == 0 {
            return Err(BridgeError::InvalidInput);
        }
        let day = block_height / blocks_per_day;
        let daily_limit = self.read_daily_limit(asset_id);
        let daily_used = self.read_daily_used(asset_id, day);
        let new_daily_used = daily_used
            .checked_add(amount)
            .ok_or(BridgeError::BalanceOverflow)?;
        if new_daily_used > daily_limit {
            return Err(BridgeError::ExceedsDailyLimit(
                asset_id,
                daily_used,
                daily_limit,
            ));
        }
        Ok(day)
    }

    fn record_daily_used(&mut self, asset_id: u64, amount: u128, day: u64) {
        let daily_used = self.read_daily_used(asset_id, day);
        let new_daily_used = daily_used.saturating_add(amount);
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_daily_used(asset_id, day),
            u128_to_u256(new_daily_used),
        );
    }

    // ── Validation ────────────────────────────────────────────────────

    fn asset_registered(&mut self, asset_id: u64) -> bool {
        let issuer = u256_to_address(
            self.backend
                .load(ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer")),
        );
        issuer != Address::ZERO
    }

    fn validate_basic(&mut self, asset_id: u64) -> Result<(), BridgeError> {
        if asset_id == 0 {
            return Err(BridgeError::AssetZeroNotBridgeable);
        }
        if !self.asset_registered(asset_id) {
            return Err(BridgeError::AssetNotRegistered);
        }
        if self.is_paused() {
            return Err(BridgeError::BridgePaused);
        }
        Ok(())
    }

    // ── Write operations ──────────────────────────────────────────────

    pub fn external_deposit(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        source_chain: u64,
        source_contract: Address,
        source_tx_hash: [u8; 32],
        asset_id: u64,
        recipient: Address,
        amount: u128,
        validator: Address,
        block_height: u64,
    ) -> Result<u128, BridgeError> {
        if amount == 0 {
            return Err(BridgeError::InvalidInput);
        }
        if recipient == Address::ZERO {
            return Err(BridgeError::InvalidInput);
        }
        if source_tx_hash == [0u8; 32] {
            return Err(BridgeError::InvalidInput);
        }
        if self.is_processed(source_tx_hash, block_height) {
            return Err(BridgeError::AlreadyProcessed);
        }
        self.validate_basic(asset_id)?;
        if !self.read_asset_allowed(asset_id) {
            return Err(BridgeError::ExternalAssetNotAllowed(asset_id));
        }
        if !self.read_authorized_contract(source_chain, source_contract) {
            return Err(BridgeError::UnauthorizedBridgeContract(
                source_chain,
                source_contract,
            ));
        }
        let day = self.check_limits(asset_id, amount, block_height)?;
        let fee = self.read_bridge_fee();
        if fee >= amount {
            return Err(BridgeError::BridgeFeeExceedsAmount(fee, amount));
        }
        let net_amount = amount - fee;

        // Perform all fallible balance operations first to avoid partial state.
        asset_store.add_balance(asset_id, recipient, net_amount)?;
        if fee > 0 {
            asset_store.add_balance(asset_id, BRIDGE_ADDRESS, fee)?;
        }
        self.add_total_deposits(net_amount)?;

        // Now record state that cannot fail.
        self.record_daily_used(asset_id, amount, day);
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_processed(source_tx_hash),
            u64_to_u256(block_height),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_deposit_asset_id(source_tx_hash),
            u64_to_u256(asset_id),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_deposit_recipient(source_tx_hash),
            address_to_u256(recipient),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_deposit_amount(source_tx_hash),
            u128_to_u256(net_amount),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_deposit_block_height(source_tx_hash),
            u64_to_u256(block_height),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_original_validator(source_tx_hash),
            address_to_u256(validator),
        );
        Ok(net_amount)
    }

    pub fn external_withdraw(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        asset_id: u64,
        amount: u128,
        caller: Address,
        current_block: u64,
    ) -> Result<u128, BridgeError> {
        if amount == 0 {
            return Err(BridgeError::InvalidInput);
        }
        self.validate_basic(asset_id)?;
        let limit_check = self.check_withdraw_limit(asset_id, amount, current_block)?;
        let fee = self.read_bridge_fee();
        if fee >= amount {
            return Err(BridgeError::BridgeFeeExceedsAmount(fee, amount));
        }
        let net_amount = amount - fee;
        asset_store.deduct_balance(asset_id, caller, amount)?;
        if fee > 0 {
            asset_store.add_balance(asset_id, BRIDGE_ADDRESS, fee)?;
        }
        self.add_total_withdrawals(net_amount)?;
        // Record limit consumption only after all fallible ops succeed.
        self.record_withdraw_used(asset_id, amount, current_block, limit_check);
        Ok(net_amount)
    }

    fn check_withdraw_limit(
        &mut self,
        asset_id: u64,
        amount: u128,
        current_block: u64,
    ) -> Result<WithdrawLimitCheck, BridgeError> {
        let max_per_period = self.read_max_external_withdraw_per_period();
        let period_start = u256_to_u64(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_current_period_start()),
        );
        let withdraw_period = self.read_withdraw_period();

        if current_block >= period_start.saturating_add(withdraw_period) {
            if amount > max_per_period {
                return Err(BridgeError::ExceedsWithdrawLimit(
                    asset_id,
                    0,
                    max_per_period,
                ));
            }
            return Ok(WithdrawLimitCheck::NewPeriod);
        }

        let period_withdrawn = u256_to_u128(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_period_withdrawn(asset_id)),
        );
        let new_total = period_withdrawn
            .checked_add(amount)
            .ok_or(BridgeError::BalanceOverflow)?;
        if new_total > max_per_period {
            return Err(BridgeError::ExceedsWithdrawLimit(
                asset_id,
                period_withdrawn,
                max_per_period,
            ));
        }
        Ok(WithdrawLimitCheck::ExistingPeriod { new_total })
    }

    fn record_withdraw_used(
        &mut self,
        asset_id: u64,
        amount: u128,
        current_block: u64,
        check: WithdrawLimitCheck,
    ) {
        match check {
            WithdrawLimitCheck::NewPeriod => {
                self.backend.store(
                    BRIDGE_ADDRESS,
                    slot_bridge_current_period_start(),
                    u64_to_u256(current_block),
                );
                self.backend.store(
                    BRIDGE_ADDRESS,
                    slot_bridge_period_withdrawn(asset_id),
                    u128_to_u256(amount),
                );
            }
            WithdrawLimitCheck::ExistingPeriod { new_total } => {
                self.backend.store(
                    BRIDGE_ADDRESS,
                    slot_bridge_period_withdrawn(asset_id),
                    u128_to_u256(new_total),
                );
            }
        }
    }

    pub fn deposit(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        target_address: Address,
        amount: u128,
        asset_id: u64,
        caller: Address,
    ) -> Result<(), BridgeError> {
        if target_address == Address::ZERO || amount == 0 {
            return Err(BridgeError::InvalidInput);
        }
        self.validate_basic(asset_id)?;
        asset_store.deduct_balance(asset_id, caller, amount)?;
        asset_store.add_balance(asset_id, target_address, amount)?;
        self.add_total_deposits(amount)?;
        Ok(())
    }

    pub fn initiate_challenge(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        source_tx_hash: [u8; 32],
        proof: Vec<u8>,
        challenger: Address,
        current_block: u64,
    ) -> Result<(), BridgeError> {
        // Use deposit block height to check existence, NOT is_processed,
        // because is_processed applies retention expiry which can falsely
        // reject challenges during the valid challenge window.
        let deposit_height = self.read_deposit_block_height(source_tx_hash);
        if deposit_height == 0 {
            return Err(BridgeError::NotProcessed);
        }
        if self.read_challenge_status(source_tx_hash) != ChallengeStatus::None {
            return Err(BridgeError::AlreadyChallenged);
        }
        let challenge_period = self.read_challenge_period();
        let challenge_deadline = deposit_height
            .checked_add(challenge_period)
            .ok_or(BridgeError::ChallengePeriodExpired)?;
        if current_block >= challenge_deadline {
            return Err(BridgeError::ChallengePeriodExpired);
        }

        if proof.len() < 32 || proof.len() > MAX_PROOF_LEN {
            return Err(BridgeError::InvalidInput);
        }

        let bond = self.read_global_challenge_bond();
        asset_store.deduct_balance(CALL_ASSET_ID, challenger, bond)?;

        let deadline = current_block
            .checked_add(challenge_period)
            .ok_or(BridgeError::ChallengePeriodExpired)?;
        let proof_hash = alloy_primitives::keccak256(&proof);
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_status(source_tx_hash),
            U256::from(1u8),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_challenger(source_tx_hash),
            address_to_u256(challenger),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_deadline(source_tx_hash),
            u64_to_u256(deadline),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_bond(source_tx_hash),
            u128_to_u256(bond),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_proof_hash(source_tx_hash),
            U256::from_be_slice(proof_hash.as_slice()),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_proof_len(source_tx_hash),
            u64_to_u256(proof.len() as u64),
        );

        // Store proof bytes in 32-byte chunks so they can be loaded back during resolution.
        for (i, chunk) in proof.chunks(32).enumerate() {
            let mut bytes = [0u8; 32];
            bytes[..chunk.len()].copy_from_slice(chunk);
            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_proof_chunk(source_tx_hash, i as u64),
                U256::from_be_bytes(bytes),
            );
        }

        Ok(())
    }

    pub fn resolve_challenge(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        validator_store: &mut ValidatorStorage<B>,
        source_tx_hash: [u8; 32],
        current_block: u64,
        resolver: Address,
    ) -> Result<(bool, Address), BridgeError> {
        if self.read_challenge_status(source_tx_hash) != ChallengeStatus::Pending {
            return Err(BridgeError::ChallengeNotPending);
        }

        let deadline = self.read_challenge_deadline(source_tx_hash);
        if current_block < deadline {
            return Err(BridgeError::ChallengeDeadlineNotReached);
        }

        let challenger = self.read_challenge_challenger(source_tx_hash);
        let validator = u256_to_address(self.backend.load(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_original_validator(source_tx_hash),
        ));
        if resolver != challenger && resolver != validator {
            return Err(BridgeError::UnauthorizedResolver(resolver));
        }

        let proof_valid = self.verify_fraud_proof(source_tx_hash);
        let bond = self.read_challenge_bond_for_tx(source_tx_hash);

        if proof_valid {
            // Slash validator first — if this fails no state has changed yet.
            validator_store
                .slash_stake(asset_store, validator)
                .map_err(|_| BridgeError::InvalidInput)?;

            let asset_id = self.read_deposit_asset_id(source_tx_hash);
            let recipient = self.read_deposit_recipient(source_tx_hash);
            let amount = self.read_deposit_amount(source_tx_hash);

            asset_store
                .deduct_balance(asset_id, recipient, amount)
                .map_err(|_| BridgeError::InsufficientBalance)?;
            self.sub_total_deposits(amount)?;

            // Do NOT reset processed — the source tx hash is permanently
            // blocked from being deposited again after a successful challenge.

            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_status(source_tx_hash),
                U256::from(2u8),
            );
            // Clear deposit metadata to prevent stale state on re-deposit.
            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_deposit_asset_id(source_tx_hash),
                U256::ZERO,
            );
            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_deposit_recipient(source_tx_hash),
                U256::ZERO,
            );
            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_deposit_amount(source_tx_hash),
                U256::ZERO,
            );
            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_deposit_block_height(source_tx_hash),
                U256::ZERO,
            );
            // Clear challenge metadata to prevent storage bloat.
            // Keep challenger and bond because `withdraw_challenge_bond` needs them.
            self.clear_challenge_metadata(source_tx_hash, true);
            Ok((true, challenger))
        } else {
            asset_store
                .add_balance(CALL_ASSET_ID, validator, bond)
                .map_err(|_| BridgeError::BalanceOverflow)?;

            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_status(source_tx_hash),
                U256::from(3u8),
            );
            // Clear challenge metadata (including proof chunks) to prevent storage bloat.
            self.clear_challenge_metadata(source_tx_hash, false);
            Ok((false, validator))
        }
    }

    /// Withdraw challenge bond. Returns the reward amount (bond + 10%) on success.
    pub fn withdraw_challenge_bond(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        source_tx_hash: [u8; 32],
        caller: Address,
    ) -> Result<u128, BridgeError> {
        if self.read_challenge_status(source_tx_hash) != ChallengeStatus::Successful {
            return Err(BridgeError::ChallengeNotSuccessful);
        }
        let challenger = self.read_challenge_challenger(source_tx_hash);
        if challenger != caller {
            return Err(BridgeError::NotChallenger);
        }

        let bond = self.read_challenge_bond_for_tx(source_tx_hash);
        let reward = bond
            .checked_add(bond / 10)
            .ok_or(BridgeError::BalanceOverflow)?;
        asset_store
            .add_balance(CALL_ASSET_ID, challenger, reward)
            .map_err(|_| BridgeError::BalanceOverflow)?;

        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_status(source_tx_hash),
            U256::from(4u8),
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_bond(source_tx_hash),
            U256::ZERO,
        );
        Ok(reward)
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Clear challenge metadata slots. If `keep_payout_info` is true,
    /// preserves challenger and bond (needed by `withdraw_challenge_bond`).
    fn clear_challenge_metadata(&mut self, source_tx_hash: [u8; 32], keep_payout_info: bool) {
        let proof_len = u256_to_u64(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_challenge_proof_len(source_tx_hash)),
        ) as usize;
        let num_chunks = (proof_len + 31) / 32;
        for i in 0..num_chunks {
            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_proof_chunk(source_tx_hash, i as u64),
                U256::ZERO,
            );
        }
        if !keep_payout_info {
            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_challenger(source_tx_hash),
                U256::ZERO,
            );
            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_bond(source_tx_hash),
                U256::ZERO,
            );
        }
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_deadline(source_tx_hash),
            U256::ZERO,
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_proof_hash(source_tx_hash),
            U256::ZERO,
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_proof_len(source_tx_hash),
            U256::ZERO,
        );
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_original_validator(source_tx_hash),
            U256::ZERO,
        );
    }

    fn add_total_deposits(&mut self, amount: u128) -> Result<(), BridgeError> {
        let total = self.get_total_deposits();
        let new_total = total
            .checked_add(amount)
            .ok_or(BridgeError::BalanceOverflow)?;
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_total_deposits(),
            u128_to_u256(new_total),
        );
        Ok(())
    }

    fn add_total_withdrawals(&mut self, amount: u128) -> Result<(), BridgeError> {
        let total = self.get_total_withdrawals();
        let new_total = total
            .checked_add(amount)
            .ok_or(BridgeError::BalanceOverflow)?;
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_total_withdrawals(),
            u128_to_u256(new_total),
        );
        Ok(())
    }

    fn sub_total_deposits(&mut self, amount: u128) -> Result<(), BridgeError> {
        let total = self.get_total_deposits();
        let new_total = total
            .checked_sub(amount)
            .ok_or(BridgeError::BalanceOverflow)?;
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_total_deposits(),
            u128_to_u256(new_total),
        );
        Ok(())
    }

    fn verify_fraud_proof(&mut self, source_tx_hash: [u8; 32]) -> bool {
        #[cfg(not(feature = "light-client-bridge"))]
        {
            // Without light-client support, reject all challenges —
            // structural validation alone is insufficient for production.
            return false;
        }

        #[cfg(feature = "light-client-bridge")]
        {
            let proof_hash = self.backend.load(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_proof_hash(source_tx_hash),
            );
            if proof_hash == U256::ZERO {
                return false;
            }
            let proof_len = u256_to_u64(self.backend.load(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_proof_len(source_tx_hash),
            ));
            if proof_len < 32 {
                return false;
            }

            // Load proof bytes from 32-byte storage chunks.
            let proof_len = proof_len as usize;
            let mut proof_bytes = Vec::with_capacity(proof_len);
            let num_chunks = (proof_len + 31) / 32;
            for i in 0..num_chunks {
                let chunk = self.backend.load(
                    BRIDGE_ADDRESS,
                    slot_bridge_challenge_proof_chunk(source_tx_hash, i as u64),
                );
                let chunk_bytes = chunk.to_be_bytes::<32>();
                let take = std::cmp::min(32, proof_len - proof_bytes.len());
                proof_bytes.extend_from_slice(&chunk_bytes[..take]);
            }

            // Verify the loaded proof matches the commitment hash.
            let computed_hash = alloy_primitives::keccak256(&proof_bytes);
            if U256::from_be_slice(computed_hash.as_slice()) != proof_hash {
                return false;
            }

            self.verify_fraud_proof_cryptographic(source_tx_hash, &proof_bytes)
        }
    }

    /// Cryptographic fraud-proof verification using MPT proofs.
    #[cfg(feature = "light-client-bridge")]
    fn verify_fraud_proof_cryptographic(
        &mut self,
        source_tx_hash: [u8; 32],
        proof_bytes: &[u8],
    ) -> bool {
        let fraud_proof: FraudProof = match postcard::from_bytes(proof_bytes) {
            Ok(fp) => fp,
            Err(_) => return false,
        };

        // Verify the header hash matches its RLP bytes (self-consistency).
        let computed_hash = alloy_primitives::keccak256(&fraud_proof.header.rlp_bytes);
        if computed_hash != fraud_proof.header.block_hash {
            return false;
        }

        match fraud_proof.proof_type {
            FraudProofType::TxNonExistence => {
                let tx_root = match fraud_proof.header.transactions_root() {
                    Some(root) => root,
                    None => return false,
                };
                let result = verify_mpt_proof(tx_root, &source_tx_hash, &fraud_proof.mpt_nodes);
                matches!(result, Ok(None))
            }
            FraudProofType::ReceiptConflict { receipt_index } => {
                let receipts_root = match fraud_proof.header.receipts_root() {
                    Some(root) => root,
                    None => return false,
                };
                let index_key = rlp_encode_u64(receipt_index);
                let result = verify_mpt_proof(receipts_root, &index_key, &fraud_proof.mpt_nodes);
                let receipt_rlp = match result {
                    Ok(Some(rlp)) => rlp,
                    _ => return false,
                };

                // Parse receipt logs and extract bridge event.
                let logs = match parse_receipt_logs(&receipt_rlp) {
                    Ok(logs) => logs,
                    Err(_) => return false,
                };
                let event = match parse_bridge_event_from_logs(&logs) {
                    Some(ev) => ev,
                    None => return false,
                };

                // Load the deposit metadata recorded by the bridge.
                let stored_asset_id = self.read_deposit_asset_id(source_tx_hash);
                let stored_recipient = self.read_deposit_recipient(source_tx_hash);
                let stored_amount = self.read_deposit_amount(source_tx_hash);

                // Fraud is proven if the receipt event contradicts the recorded deposit.
                event.asset_id != stored_asset_id
                    || event.recipient != stored_recipient
                    || event.amount != stored_amount
            }
        }
    }
}

// ── sol! interface ────────────────────────────────────────────────────

sol! {
    interface IProtocolBridge {
        function getTotalDeposits() external view returns (uint128);
        function getTotalWithdrawals() external view returns (uint128);
        function externalDeposit(uint64 sourceChain, address sourceContract, bytes32 sourceTxHash, uint64 assetId, address recipient, uint128 amount) external;
        function externalWithdraw(uint64 targetChain, bytes calldata targetAddress, uint64 assetId, uint128 amount) external;
        function deposit(address targetAddress, uint128 amount, uint64 assetId) external;
        function initiateChallenge(bytes32 sourceTxHash, bytes calldata proof) external;
        function resolveChallenge(bytes32 sourceTxHash) external;
        function getChallengeStatus(bytes32 sourceTxHash) external view returns (uint64 status, uint64 deadline, uint128 bond, address challenger);
        function withdrawChallengeBond(bytes32 sourceTxHash) external;
    }
}

// ── BridgePrecompile ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct BridgePrecompile;

impl BridgePrecompile {
    fn get_total_deposits(
        &self,
        calldata: &[u8],
        storage: &mut dyn call_precompile::storage::StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolBridge::getTotalDepositsCall, _, _>(
            calldata,
            1500,
            storage,
            |_call, _storage| {
                let mut store = BridgeStorage::new(sr);
                Ok(store.get_total_deposits())
            },
        )
    }

    fn get_total_withdrawals(
        &self,
        calldata: &[u8],
        storage: &mut dyn call_precompile::storage::StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolBridge::getTotalWithdrawalsCall, _, _>(
            calldata,
            1500,
            storage,
            |_call, _storage| {
                let mut store = BridgeStorage::new(sr);
                Ok(store.get_total_withdrawals())
            },
        )
    }

    fn external_deposit(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn call_precompile::storage::StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::externalDepositCall, _>(
            calldata,
            30000,
            storage,
            |call, storage| {
                let validator = require_caller(msg_sender)?;
                if !is_validator(storage, validator) {
                    return Err(PrecompileError::Other("not a validator".into()));
                }
                check_compliance(validator, storage)?;
                check_compliance(call.recipient, storage)?;
                let mut bridge_store = BridgeStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                let block_height = storage.block_number();
                let net_amount = bridge_store
                    .external_deposit(
                        &mut asset_store,
                        call.sourceChain,
                        call.sourceContract,
                        call.sourceTxHash.into(),
                        call.assetId,
                        call.recipient,
                        call.amount,
                        validator,
                        block_height,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let topic0 = alloy_primitives::keccak256(
                    b"ExternalDeposit(bytes32,uint64,address,uint128,address,uint64,uint64,address)",
                );
                let topic1 = alloy_primitives::B256::from(call.sourceTxHash);
                let topic2 = alloy_primitives::B256::from(
                    address_to_u256(call.recipient).to_be_bytes::<32>(),
                );
                let topic3 = alloy_primitives::B256::from(
                    address_to_u256(validator).to_be_bytes::<32>(),
                );
                let mut data = Vec::with_capacity(160);
                data.extend_from_slice(&u64_to_u256(call.assetId).to_be_bytes::<32>());
                data.extend_from_slice(
                    &address_to_u256(call.sourceContract).to_be_bytes::<32>(),
                );
                data.extend_from_slice(&u128_to_u256(net_amount).to_be_bytes::<32>());
                data.extend_from_slice(&u64_to_u256(block_height).to_be_bytes::<32>());
                data.extend_from_slice(&u64_to_u256(call.sourceChain).to_be_bytes::<32>());
                let log = alloy_primitives::LogData::new(
                    vec![topic0, topic1, topic2, topic3],
                    alloy_primitives::Bytes::from(data),
                )
                .expect("invariant: topics non-empty, LogData::new always succeeds");
                storage.emit_event(BRIDGE_ADDRESS, log)?;

                Ok(())
            },
        )
    }

    fn external_withdraw(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn call_precompile::storage::StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::externalWithdrawCall, _>(
            calldata,
            30000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                check_compliance(caller, storage)?;
                let mut bridge_store = BridgeStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                let block_number = storage.block_number();
                let net_amount = bridge_store
                    .external_withdraw(&mut asset_store, call.assetId, call.amount, caller, block_number)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let target_addr_hash = alloy_primitives::keccak256(&call.targetAddress);
                let topic0 = alloy_primitives::keccak256(
                    b"ExternalWithdraw(address,uint64,uint128,uint64,bytes32)",
                );
                let topic1 = alloy_primitives::B256::from(
                    address_to_u256(caller).to_be_bytes::<32>(),
                );
                let mut data = Vec::with_capacity(128);
                data.extend_from_slice(&u64_to_u256(call.assetId).to_be_bytes::<32>());
                data.extend_from_slice(&u128_to_u256(net_amount).to_be_bytes::<32>());
                data.extend_from_slice(&u64_to_u256(call.targetChain).to_be_bytes::<32>());
                data.extend_from_slice(target_addr_hash.as_slice());
                let log = alloy_primitives::LogData::new(
                    vec![topic0, topic1],
                    alloy_primitives::Bytes::from(data),
                )
                .expect("invariant: topics non-empty, LogData::new always succeeds");
                storage.emit_event(BRIDGE_ADDRESS, log)?;

                Ok(())
            },
        )
    }

    fn deposit(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn call_precompile::storage::StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::depositCall, _>(
            calldata,
            30000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                check_compliance(caller, storage)?;
                check_compliance(call.targetAddress, storage)?;
                let mut bridge_store = BridgeStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                let block_height = storage.block_number();
                bridge_store
                    .deposit(
                        &mut asset_store,
                        call.targetAddress,
                        call.amount,
                        call.assetId,
                        caller,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let topic0 = alloy_primitives::keccak256(
                    b"Deposit(address,address,uint128,uint64,uint64)",
                );
                let topic1 = alloy_primitives::B256::from(
                    address_to_u256(caller).to_be_bytes::<32>(),
                );
                let mut data = Vec::with_capacity(128);
                data.extend_from_slice(
                    &address_to_u256(call.targetAddress).to_be_bytes::<32>(),
                );
                data.extend_from_slice(&u128_to_u256(call.amount).to_be_bytes::<32>());
                data.extend_from_slice(&u64_to_u256(call.assetId).to_be_bytes::<32>());
                data.extend_from_slice(&u64_to_u256(block_height).to_be_bytes::<32>());
                let log = alloy_primitives::LogData::new(
                    vec![topic0, topic1],
                    alloy_primitives::Bytes::from(data),
                )
                .expect("invariant: topics non-empty, LogData::new always succeeds");
                storage.emit_event(BRIDGE_ADDRESS, log)?;

                Ok(())
            },
        )
    }

    fn initiate_challenge(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn call_precompile::storage::StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::initiateChallengeCall, _>(
            calldata,
            50000,
            storage,
            |call, storage| {
                let challenger = require_caller(msg_sender)?;
                check_compliance(challenger, storage)?;
                let mut bridge_store = BridgeStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                let block_number = storage.block_number();
                let source_tx_hash: [u8; 32] = call.sourceTxHash.into();
                bridge_store
                    .initiate_challenge(
                        &mut asset_store,
                        source_tx_hash,
                        call.proof.to_vec(),
                        challenger,
                        block_number,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let deadline = bridge_store.read_challenge_deadline(source_tx_hash);
                let bond = bridge_store.read_challenge_bond_for_tx(source_tx_hash);
                let topic0 = alloy_primitives::keccak256(
                    b"ChallengeInitiated(bytes32,address,uint64,uint128)",
                );
                let topic1 = alloy_primitives::B256::from(source_tx_hash);
                let topic2 = alloy_primitives::B256::from(
                    address_to_u256(challenger).to_be_bytes::<32>(),
                );
                let mut data = Vec::with_capacity(64);
                data.extend_from_slice(&u64_to_u256(deadline).to_be_bytes::<32>());
                data.extend_from_slice(&u128_to_u256(bond).to_be_bytes::<32>());
                let log = alloy_primitives::LogData::new(
                    vec![topic0, topic1, topic2],
                    alloy_primitives::Bytes::from(data),
                )
                .expect("invariant: topics non-empty, LogData::new always succeeds");
                storage.emit_event(BRIDGE_ADDRESS, log)?;

                Ok(())
            },
        )
    }

    fn resolve_challenge(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn call_precompile::storage::StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::resolveChallengeCall, _>(
            calldata,
            150000,
            storage,
            |call, storage| {
                let resolver = require_caller(msg_sender)?;
                check_compliance(resolver, storage)?;
                let mut bridge_store = BridgeStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                let mut validator_store = ValidatorStorage::new(sr);
                let block_number = storage.block_number();
                let source_tx_hash: [u8; 32] = call.sourceTxHash.into();
                let (success, beneficiary) = bridge_store
                    .resolve_challenge(
                        &mut asset_store,
                        &mut validator_store,
                        source_tx_hash,
                        block_number,
                        resolver,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let topic0 = alloy_primitives::keccak256(
                    b"ChallengeResolved(bytes32,bool,address)",
                );
                let topic1 = alloy_primitives::B256::from(source_tx_hash);
                let topic2 = alloy_primitives::B256::from(
                    address_to_u256(beneficiary).to_be_bytes::<32>(),
                );
                let mut data = Vec::with_capacity(32);
                data.extend_from_slice(&u64_to_u256(if success { 1 } else { 0 }).to_be_bytes::<32>());
                let log = alloy_primitives::LogData::new(
                    vec![topic0, topic1, topic2],
                    alloy_primitives::Bytes::from(data),
                )
                .expect("invariant: topics non-empty, LogData::new always succeeds");
                storage.emit_event(BRIDGE_ADDRESS, log)?;

                Ok(())
            },
        )
    }

    fn get_challenge_status(
        &self,
        calldata: &[u8],
        storage: &mut dyn call_precompile::storage::StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolBridge::getChallengeStatusCall, _, _>(
            calldata,
            1500,
            storage,
            |call, _storage| {
                let mut store = BridgeStorage::new(sr);
                let status = match store.read_challenge_status(call.sourceTxHash.into()) {
                    ChallengeStatus::Pending => 1u64,
                    ChallengeStatus::Successful => 2u64,
                    ChallengeStatus::Failed => 3u64,
                    ChallengeStatus::Withdrawn => 4u64,
                    ChallengeStatus::None => 0u64,
                };
                let deadline = store.read_challenge_deadline(call.sourceTxHash.into());
                let bond = store.read_challenge_bond_for_tx(call.sourceTxHash.into());
                let challenger = store.read_challenge_challenger(call.sourceTxHash.into());
                Ok((status, deadline, bond, challenger))
            },
        )
    }

    fn withdraw_challenge_bond(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn call_precompile::storage::StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::withdrawChallengeBondCall, _>(
            calldata,
            5000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                check_compliance(caller, storage)?;
                let mut bridge_store = BridgeStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                let source_tx_hash: [u8; 32] = call.sourceTxHash.into();
                let reward = bridge_store
                    .withdraw_challenge_bond(&mut asset_store, source_tx_hash, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let topic0 = alloy_primitives::keccak256(
                    b"ChallengeBondWithdrawn(bytes32,address,uint128)",
                );
                let topic1 = alloy_primitives::B256::from(source_tx_hash);
                let topic2 = alloy_primitives::B256::from(
                    address_to_u256(caller).to_be_bytes::<32>(),
                );
                let mut data = Vec::with_capacity(32);
                data.extend_from_slice(&u128_to_u256(reward).to_be_bytes::<32>());
                let log = alloy_primitives::LogData::new(
                    vec![topic0, topic1, topic2],
                    alloy_primitives::Bytes::from(data),
                )
                .expect("invariant: topics non-empty, LogData::new always succeeds");
                storage.emit_event(BRIDGE_ADDRESS, log)?;

                Ok(())
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for BridgePrecompile {
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn call_precompile::storage::StorageProvider,
    ) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let selector: [u8; 4] = calldata[..4]
            .try_into()
            .map_err(|_| PrecompileError::Other("invalid selector".into()))?;
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolBridge::getTotalDepositsCall::SELECTOR => {
                self.get_total_deposits(calldata, storage, sr)
            }
            IProtocolBridge::getTotalWithdrawalsCall::SELECTOR => {
                self.get_total_withdrawals(calldata, storage, sr)
            }
            IProtocolBridge::externalDepositCall::SELECTOR => {
                self.external_deposit(calldata, msg_sender, storage, sr)
            }
            IProtocolBridge::externalWithdrawCall::SELECTOR => {
                self.external_withdraw(calldata, msg_sender, storage, sr)
            }
            IProtocolBridge::depositCall::SELECTOR => {
                self.deposit(calldata, msg_sender, storage, sr)
            }
            IProtocolBridge::initiateChallengeCall::SELECTOR => {
                self.initiate_challenge(calldata, msg_sender, storage, sr)
            }
            IProtocolBridge::resolveChallengeCall::SELECTOR => {
                self.resolve_challenge(calldata, msg_sender, storage, sr)
            }
            IProtocolBridge::getChallengeStatusCall::SELECTOR => {
                self.get_challenge_status(calldata, storage, sr)
            }
            IProtocolBridge::withdrawChallengeBondCall::SELECTOR => {
                self.withdraw_challenge_bond(calldata, msg_sender, storage, sr)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{
        slot_balance, slot_compliance, u128_to_u256, u256_to_u128, COMPLIANCE_ADDRESS,
        StatefulPrecompile,
    };
    use call_primitives::Address;

    #[test]
    fn test_bridge_address() {
        assert_eq!(
            BRIDGE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000103")
        );
    }

    #[test]
    fn test_bridge_precompile_stateful_reads() {
        let mut provider = HashMapStorageProvider::new(1_000_000);

        provider.set(BRIDGE_ADDRESS, U256::from(0), u128_to_u256(5000));
        provider.set(BRIDGE_ADDRESS, U256::from(1), u128_to_u256(2000));

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::getTotalDepositsCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let deposits = u256_to_u128(U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        assert_eq!(deposits, 5000);

        let input = IProtocolBridge::getTotalWithdrawalsCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let withdrawals = u256_to_u128(U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        assert_eq!(withdrawals, 2000);
    }

    #[test]
    fn test_external_deposit_records_metadata() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        // Register asset_id=1 and validator
        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        // Authorize Address::ZERO as source contract for chain 1
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_ok(),
            "external_deposit failed: {:?}",
            result.err()
        );

        let stored_asset_id = u256_to_u64(
            provider
                .get(BRIDGE_ADDRESS, slot_bridge_deposit_asset_id(source_tx_hash))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(stored_asset_id, 1);

        let stored_recipient = u256_to_address(
            provider
                .get(
                    BRIDGE_ADDRESS,
                    slot_bridge_deposit_recipient(source_tx_hash),
                )
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(stored_recipient, recipient);

        let stored_amount = u256_to_u128(
            provider
                .get(BRIDGE_ADDRESS, slot_bridge_deposit_amount(source_tx_hash))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(stored_amount, 1000);

        let stored_height = u256_to_u64(
            provider
                .get(
                    BRIDGE_ADDRESS,
                    slot_bridge_deposit_block_height(source_tx_hash),
                )
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(stored_height, 10);

        let stored_validator = u256_to_address(
            provider
                .get(
                    BRIDGE_ADDRESS,
                    slot_bridge_challenge_original_validator(source_tx_hash),
                )
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(stored_validator, validator);
    }

    #[test]
    fn test_initiate_challenge_success() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        // Register asset_id=1, validator, and seed challenger balance
        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, challenger),
            u128_to_u256(5000),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, recipient), u128_to_u256(0));
        // Authorize Address::ZERO and set short challenge period for test
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_challenge_period(),
            u64_to_u256(101),
        );

        let mut precompile = BridgePrecompile;

        // 1. externalDeposit
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, validator, &mut provider).unwrap();

        // 2. initiateChallenge at block 10 (within period)
        let input = IProtocolBridge::initiateChallengeCall {
            sourceTxHash: source_tx_hash.into(),
            proof: alloy_primitives::Bytes::from_static(&[0u8; 32]),
        }
        .abi_encode();

        let result = precompile.call(&input, challenger, &mut provider);
        assert!(
            result.is_ok(),
            "initiate_challenge failed: {:?}",
            result.err()
        );

        let status = provider
            .get(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        assert_eq!(status, 1); // Pending

        let stored_challenger = u256_to_address(
            provider
                .get(
                    BRIDGE_ADDRESS,
                    slot_bridge_challenge_challenger(source_tx_hash),
                )
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(stored_challenger, challenger);

        let deadline = u256_to_u64(
            provider
                .get(
                    BRIDGE_ADDRESS,
                    slot_bridge_challenge_deadline(source_tx_hash),
                )
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(deadline, 110); // block 10 + period 100

        // Bond deducted from challenger
        let challenger_bal = u256_to_u128(
            provider
                .get(ASSET_ADDRESS, slot_balance(CALL_ASSET_ID, challenger))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(challenger_bal, 4000); // 5000 - 1000 bond
    }

    #[test]
    fn test_initiate_challenge_fails_not_processed() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let challenger = Address::repeat_byte(0x33);
        let source_tx_hash = [0xABu8; 32];

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::initiateChallengeCall {
            sourceTxHash: source_tx_hash.into(),
            proof: alloy_primitives::Bytes::from_static(&[0u8; 32]),
        }
        .abi_encode();

        let result = precompile.call(&input, challenger, &mut provider);
        assert!(result.is_err(), "should fail: source tx not processed");
    }

    #[test]
    fn test_initiate_challenge_fails_period_expired() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        // externalDeposit at block 10
        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, challenger),
            u128_to_u256(5000),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, recipient), u128_to_u256(0));
        // Authorize Address::ZERO and set short challenge period for test
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_challenge_period(),
            u64_to_u256(101),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, validator, &mut provider).unwrap();

        // initiateChallenge at block 200 (deposit at block 10, period = 100, so expired)
        provider.set_block_number(200);

        let input = IProtocolBridge::initiateChallengeCall {
            sourceTxHash: source_tx_hash.into(),
            proof: alloy_primitives::Bytes::from_static(&[0u8; 32]),
        }
        .abi_encode();

        let result = precompile.call(&input, challenger, &mut provider);
        assert!(result.is_err(), "should fail: challenge period expired");
    }

    #[test]
    fn test_initiate_challenge_fails_already_challenged() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, challenger),
            u128_to_u256(5000),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, recipient), u128_to_u256(0));
        // Authorize Address::ZERO and set short challenge period for test
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_challenge_period(),
            u64_to_u256(101),
        );

        let mut precompile = BridgePrecompile;

        // externalDeposit
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, validator, &mut provider).unwrap();

        // First challenge
        let input = IProtocolBridge::initiateChallengeCall {
            sourceTxHash: source_tx_hash.into(),
            proof: alloy_primitives::Bytes::from_static(&[0u8; 32]),
        }
        .abi_encode();
        precompile.call(&input, challenger, &mut provider).unwrap();

        // Second challenge should fail
        let result = precompile.call(&input, challenger, &mut provider);
        assert!(result.is_err(), "should fail: already challenged");
    }

    #[test]
    fn test_resolve_challenge_false_proof_bond_forfeited() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        // Setup state and externalDeposit + initiateChallenge at block 10
        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, challenger),
            u128_to_u256(5000),
        );
        provider.set(
            ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, validator),
            u128_to_u256(100),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, recipient), u128_to_u256(0));
        // Authorize Address::ZERO and set short challenge period for test
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_challenge_period(),
            u64_to_u256(101),
        );

        let mut precompile = BridgePrecompile;

        // externalDeposit
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, validator, &mut provider).unwrap();

        // initiateChallenge
        let input = IProtocolBridge::initiateChallengeCall {
            sourceTxHash: source_tx_hash.into(),
            proof: alloy_primitives::Bytes::from_static(&[0u8; 32]),
        }
        .abi_encode();
        precompile.call(&input, challenger, &mut provider).unwrap();

        // resolveChallenge at block 120 (deadline = 10 + 100 = 110)
        provider.set_block_number(120);

        let input = IProtocolBridge::resolveChallengeCall {
            sourceTxHash: source_tx_hash.into(),
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_ok(),
            "resolve_challenge failed: {:?}",
            result.err()
        );

        // Status should be Failed (3)
        let status = provider
            .get(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        assert_eq!(status, 3);

        // Validator should have received the bond
        let validator_bal = u256_to_u128(
            provider
                .get(ASSET_ADDRESS, slot_balance(CALL_ASSET_ID, validator))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(validator_bal, 1100); // 100 + 1000 bond
    }

    #[test]
    fn test_resolve_challenge_true_proof_bond_rewarded() {
        let mut provider = HashMapStorageProvider::with_block(2_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        // Setup state and externalDeposit + initiateChallenge at block 10
        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, challenger),
            u128_to_u256(5000),
        );
        provider.set(
            ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, validator),
            u128_to_u256(100),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, recipient), u128_to_u256(0));
        // Authorize Address::ZERO and set short challenge period for test
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_challenge_period(),
            u64_to_u256(101),
        );

        let mut precompile = BridgePrecompile;

        // externalDeposit
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, validator, &mut provider).unwrap();

        // Build a valid TxNonExistence fraud proof.
        // The MPT has a leaf at key [0,0,0,0] which does NOT match source_tx_hash's
        // nibbles [A,B,A,B,...], so verify_mpt_proof returns None (non-existence).
        let _compact_key = vec![0x20, 0x00, 0x00]; // even leaf, 4 zero nibbles
        let compact_rlp = vec![0x83, 0x20, 0x00, 0x00];
        let value_rlp = vec![0x85, b'd', b'u', b'm', b'm', b'y'];
        let mut leaf_rlp = vec![0xCA]; // short list, payload = 10
        leaf_rlp.extend_from_slice(&compact_rlp);
        leaf_rlp.extend_from_slice(&value_rlp);
        let tx_root = alloy_primitives::keccak256(&leaf_rlp);

        // Build minimal Ethereum header RLP with tx_root set.
        let mut fields: Vec<Vec<u8>> = Vec::new();
        fields.push({
            let mut v = vec![0xa0];
            v.extend([0u8; 32]);
            v
        }); // parent_hash
        fields.push(vec![0x80]); // sha3_uncles
        fields.push(vec![0x80]); // miner
        fields.push({
            let mut v = vec![0xa0];
            v.extend([0u8; 32]);
            v
        }); // state_root
        fields.push({
            let mut v = vec![0xa0];
            v.extend_from_slice(&tx_root.0);
            v
        }); // tx_root
        fields.push({
            let mut v = vec![0xa0];
            v.extend([0u8; 32]);
            v
        }); // receipt_root
        fields.push(vec![0x80]); // logs_bloom
        fields.push(vec![0x01]); // difficulty
        fields.push(vec![0x01]); // block_number
        fields.push(vec![0x01]); // gas_limit
        fields.push(vec![0x80]); // gas_used
        fields.push(vec![0x01]); // timestamp
        fields.push(vec![0x80]); // extra_data
        fields.push({
            let mut v = vec![0xa0];
            v.extend([0u8; 32]);
            v
        }); // mix_hash
        fields.push({
            let mut v = vec![0x88];
            v.extend([0u8; 8]);
            v
        }); // nonce
        fields.push(vec![0x80]); // base_fee
        let payload_len: usize = fields.iter().map(|f| f.len()).sum();
        let mut header_rlp = Vec::new();
        if payload_len < 56 {
            header_rlp.push(0xC0 + payload_len as u8);
        } else {
            let len_bytes = payload_len.to_be_bytes();
            let skip = len_bytes
                .iter()
                .position(|&b| b != 0)
                .unwrap_or(len_bytes.len());
            header_rlp.push(0xF7 + (len_bytes.len() - skip) as u8);
            header_rlp.extend_from_slice(&len_bytes[skip..]);
        }
        for f in fields {
            header_rlp.extend_from_slice(&f);
        }
        let header = call_light_client::EthHeader::from_rlp(header_rlp);

        let fraud_proof = crate::external::types::FraudProof {
            header,
            proof_type: crate::external::types::FraudProofType::TxNonExistence,
            mpt_nodes: vec![leaf_rlp],
        };
        let proof_bytes = postcard::to_allocvec(&fraud_proof).unwrap();

        // initiateChallenge with the cryptographically valid proof
        let input = IProtocolBridge::initiateChallengeCall {
            sourceTxHash: source_tx_hash.into(),
            proof: alloy_primitives::Bytes::from(proof_bytes),
        }
        .abi_encode();
        precompile.call(&input, challenger, &mut provider).unwrap();

        // resolveChallenge at block 120 (deadline = 10 + 100 = 110)
        provider.set_block_number(120);

        let input = IProtocolBridge::resolveChallengeCall {
            sourceTxHash: source_tx_hash.into(),
        }
        .abi_encode();

        let result = precompile.call(&input, challenger, &mut provider);
        assert!(
            result.is_ok(),
            "resolve_challenge failed: {:?}",
            result.err()
        );

        // Status should be Successful (2)
        let status = provider
            .get(BRIDGE_ADDRESS, slot_bridge_challenge_status(source_tx_hash))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
        assert_eq!(status, 2);

        // Challenger balance after resolve (reward not yet withdrawn)
        let challenger_bal = u256_to_u128(
            provider
                .get(ASSET_ADDRESS, slot_balance(CALL_ASSET_ID, challenger))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(challenger_bal, 4000); // 5000 - 1000 bond

        // Recipient balance should have been reversed (1000 deducted)
        let recipient_bal = u256_to_u128(
            provider
                .get(ASSET_ADDRESS, slot_balance(1, recipient))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(recipient_bal, 0);

        // Withdraw challenge bond to collect reward
        let input = IProtocolBridge::withdrawChallengeBondCall {
            sourceTxHash: source_tx_hash.into(),
        }
        .abi_encode();
        let result = precompile.call(&input, challenger, &mut provider);
        assert!(
            result.is_ok(),
            "withdraw_challenge_bond failed: {:?}",
            result.err()
        );

        let challenger_bal = u256_to_u128(
            provider
                .get(ASSET_ADDRESS, slot_balance(CALL_ASSET_ID, challenger))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(challenger_bal, 4000 + 1000 + 100); // 4000 + bond + 10% reward
    }

    #[test]
    fn test_get_challenge_status() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, challenger),
            u128_to_u256(5000),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, recipient), u128_to_u256(0));
        // Authorize Address::ZERO and set short challenge period for test
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_challenge_period(),
            u64_to_u256(101),
        );

        let mut precompile = BridgePrecompile;

        // externalDeposit
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, validator, &mut provider).unwrap();

        // initiateChallenge
        let input = IProtocolBridge::initiateChallengeCall {
            sourceTxHash: source_tx_hash.into(),
            proof: alloy_primitives::Bytes::from_static(&[0u8; 32]),
        }
        .abi_encode();
        precompile.call(&input, challenger, &mut provider).unwrap();

        // getChallengeStatus
        let input = IProtocolBridge::getChallengeStatusCall {
            sourceTxHash: source_tx_hash.into(),
        }
        .abi_encode();

        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes.len(), 128);
        assert_eq!(result.bytes[31], 1); // status = Pending
        let deadline = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&result.bytes[56..64]);
            buf
        });
        assert_eq!(deadline, 110);
        let bond = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&result.bytes[80..96]);
            buf
        });
        assert_eq!(bond, 1000);
        let returned_challenger = Address::from_slice(&result.bytes[108..128]);
        assert_eq!(returned_challenger, challenger);
    }

    #[test]
    fn test_deposit_success() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let caller = Address::repeat_byte(0x11);
        let target = Address::repeat_byte(0x22);

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, caller), u128_to_u256(5000));

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::depositCall {
            targetAddress: target,
            amount: 1000,
            assetId: 1,
        }
        .abi_encode();

        let result = precompile.call(&input, caller, &mut provider);
        assert!(result.is_ok(), "deposit failed: {:?}", result.err());

        let target_balance = u256_to_u128(
            provider
                .get(ASSET_ADDRESS, slot_balance(1, target))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(target_balance, 1000);

        let caller_balance = u256_to_u128(
            provider
                .get(ASSET_ADDRESS, slot_balance(1, caller))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(caller_balance, 4000);
    }

    #[test]
    fn test_external_withdraw_success() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let caller = Address::repeat_byte(0x11);

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, caller), u128_to_u256(5000));

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalWithdrawCall {
            targetChain: 1,
            targetAddress: alloy_primitives::Bytes::from(vec![0xAA; 20]),
            assetId: 1,
            amount: 1000,
        }
        .abi_encode();

        let result = precompile.call(&input, caller, &mut provider);
        assert!(
            result.is_ok(),
            "external_withdraw failed: {:?}",
            result.err()
        );

        let caller_balance = u256_to_u128(
            provider
                .get(ASSET_ADDRESS, slot_balance(1, caller))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(caller_balance, 4000);
    }

    #[test]
    fn test_deposit_rejects_sanctioned_caller() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let caller = Address::repeat_byte(0x11);
        let target = Address::repeat_byte(0x22);

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        // Mark caller as sanctioned (compliance status != 0)
        provider.set(
            COMPLIANCE_ADDRESS,
            slot_compliance(caller),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::depositCall {
            targetAddress: target,
            amount: 1000,
            assetId: 1,
        }
        .abi_encode();

        let result = precompile.call(&input, caller, &mut provider);
        assert!(result.is_err(), "deposit should reject sanctioned caller");
    }

    #[test]
    fn test_external_deposit_rejects_zero_recipient() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient: Address::ZERO,
            amount: 1000,
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_err(),
            "externalDeposit should reject zero recipient"
        );
    }

    #[test]
    fn test_external_deposit_rejects_zero_tx_hash() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: [0u8; 32].into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_err(),
            "externalDeposit should reject zero tx hash"
        );
    }

    #[test]
    fn test_external_deposit_rejects_exceeds_max_per_tx() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        // Set max_per_tx = 500 for asset_id=1 (offset+1: store 501)
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_max_per_tx(1),
            u128_to_u256(501),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_err(),
            "externalDeposit should reject amount exceeding max_per_tx"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds max per tx"), "error should mention max per tx: {err}");
    }

    #[test]
    fn test_external_deposit_rejects_exceeds_daily_limit() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        // Set daily_limit = 500 for asset_id=1; blocks_per_day = 100 (offset+1)
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_daily_limit(1),
            u128_to_u256(501),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_blocks_per_day(),
            u64_to_u256(101),
        );
        // Pre-fill daily_used at day=0 (block 10 / 100 = 0) with 200
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_daily_used(1, 0),
            u128_to_u256(200),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 400, // 200 + 400 = 600 > 500 daily_limit
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_err(),
            "externalDeposit should reject amount exceeding daily_limit"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds daily limit"), "error should mention daily limit: {err}");
    }

    #[test]
    fn test_external_deposit_daily_limit_resets_next_day() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 250);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash_1 = [0xABu8; 32];
        let source_tx_hash_2 = [0xCDu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        // Set daily_limit = 500 for asset_id=1; blocks_per_day = 100 (offset+1)
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_daily_limit(1),
            u128_to_u256(501),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_blocks_per_day(),
            u64_to_u256(101),
        );
        // day=2 (block 250 / 100 = 2) has used 0, so 400 should succeed
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash_1.into(),
            assetId: 1,
            recipient,
            amount: 400,
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_ok(),
            "externalDeposit should succeed on a new day: {:?}",
            result.err()
        );

        // daily_used for day=2 should now be 400
        let daily_used = u256_to_u128(
            provider
                .get(BRIDGE_ADDRESS, slot_bridge_daily_used(1, 2))
                .unwrap_or(U256::ZERO),
        );
        assert_eq!(daily_used, 400);

        // Another deposit of 150 should fail (400 + 150 = 550 > 500)
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash_2.into(),
            assetId: 1,
            recipient,
            amount: 150,
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(result.is_err(), "second deposit should exceed daily limit");
    }

    #[test]
    fn test_external_deposit_rejects_not_allowed_asset() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        // Explicitly disallow asset_id=1 by writing non-zero with LSB=0
        // (read_asset_allowed treats ZERO as "use default", so we must write
        // a non-zero value whose last byte is 0 to mean "explicitly false").
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_asset_allowed(1),
            U256::from(256u16),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_err(),
            "externalDeposit should reject not-allowed asset"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("external bridge asset not allowed"),
            "error should mention asset not allowed: {err}"
        );
    }

    #[test]
    fn test_external_deposit_rejects_unauthorized_contract() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );

        let mut precompile = BridgePrecompile;

        // sourceContract = 0xFF... is not in default authorized_contracts
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::repeat_byte(0xFF),
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();

        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_err(),
            "externalDeposit should reject unauthorized contract"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unauthorized bridge contract"),
            "error should mention unauthorized contract: {err}"
        );
    }

    #[test]
    fn test_external_withdraw_rejects_exceeds_period_limit() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let caller = Address::repeat_byte(0x11);

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, caller), u128_to_u256(5000));
        // Set max_withdraw_per_period = 500 (offset+1: store 501)
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_max_withdraw_per_period(),
            u128_to_u256(501),
        );

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalWithdrawCall {
            targetChain: 1,
            targetAddress: alloy_primitives::Bytes::from(vec![0xAA; 20]),
            assetId: 1,
            amount: 600,
        }
        .abi_encode();

        let result = precompile.call(&input, caller, &mut provider);
        assert!(
            result.is_err(),
            "externalWithdraw should reject amount exceeding period limit"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("exceeds withdraw limit"),
            "error should mention withdraw limit: {err}"
        );
    }

    #[test]
    fn test_external_withdraw_period_limit_resets_next_period() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let caller = Address::repeat_byte(0x11);

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, caller), u128_to_u256(5000));
        // Set max_withdraw_per_period = 500 (offset+1: 501); withdraw_period = 100 (offset+1: 101)
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_max_withdraw_per_period(),
            u128_to_u256(501),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_withdraw_period(),
            u64_to_u256(101),
        );
        // First withdraw of 400 at block 10
        let mut precompile = BridgePrecompile;
        let input = IProtocolBridge::externalWithdrawCall {
            targetChain: 1,
            targetAddress: alloy_primitives::Bytes::from(vec![0xAA; 20]),
            assetId: 1,
            amount: 400,
        }
        .abi_encode();
        let result = precompile.call(&input, caller, &mut provider);
        assert!(result.is_ok(), "first withdraw should succeed: {:?}", result.err());

        // Second withdraw of 200 at block 250 (new period, period_start=10, challenge_period=100, so 250 >= 110)
        provider.set_block_number(250);
        let input2 = IProtocolBridge::externalWithdrawCall {
            targetChain: 1,
            targetAddress: alloy_primitives::Bytes::from(vec![0xAA; 20]),
            assetId: 1,
            amount: 200,
        }
        .abi_encode();
        let result = precompile.call(&input2, caller, &mut provider);
        assert!(
            result.is_ok(),
            "second withdraw in new period should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_processed_tx_retention() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        // Set retention = 50 blocks (offset+1: store 51)
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_processed_retention(),
            u64_to_u256(51),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        // First deposit at block 10
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, validator, &mut provider).unwrap();

        // Re-deposit at block 30 (within retention) should fail
        provider.set_block_number(30);
        let result = precompile.call(&input, validator, &mut provider);
        assert!(result.is_err(), "re-deposit within retention should fail");

        // Re-deposit at block 70 (beyond retention: 70 - 10 = 60 > 50) should succeed
        provider.set_block_number(70);
        let result = precompile.call(&input, validator, &mut provider);
        assert!(
            result.is_ok(),
            "re-deposit after retention should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_initiate_challenge_ignores_retention() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, challenger),
            u128_to_u256(5000),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, recipient), u128_to_u256(0));
        // Set retention = 50 blocks (offset+1: 51), challenge_period = 100 (offset+1: 101)
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_processed_retention(),
            u64_to_u256(51),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_challenge_period(),
            u64_to_u256(101),
        );
        // Authorize Address::ZERO as source contract for chain 1
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );

        let mut precompile = BridgePrecompile;

        // externalDeposit at block 10
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, validator, &mut provider).unwrap();

        // At block 70, processed has expired (70 - 10 = 60 > 50)
        // but challenge deadline is 10 + 100 = 110, so challenge should still work.
        provider.set_block_number(70);

        let input = IProtocolBridge::initiateChallengeCall {
            sourceTxHash: source_tx_hash.into(),
            proof: alloy_primitives::Bytes::from_static(&[0u8; 32]),
        }
        .abi_encode();

        let result = precompile.call(&input, challenger, &mut provider);
        assert!(
            result.is_ok(),
            "initiate_challenge should work even when processed expired but within challenge window: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_deposit_rejects_insufficient_balance() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let caller = Address::repeat_byte(0x11);
        let target = Address::repeat_byte(0x22);

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, caller), u128_to_u256(500));

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::depositCall {
            targetAddress: target,
            amount: 1000,
            assetId: 1,
        }
        .abi_encode();

        let result = precompile.call(&input, caller, &mut provider);
        assert!(
            result.is_err(),
            "deposit should reject insufficient balance"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("insufficient balance"), "error should mention balance: {err}");
    }

    #[test]
    fn test_external_withdraw_rejects_insufficient_balance() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let caller = Address::repeat_byte(0x11);

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, caller), u128_to_u256(500));

        let mut precompile = BridgePrecompile;

        let input = IProtocolBridge::externalWithdrawCall {
            targetChain: 1,
            targetAddress: alloy_primitives::Bytes::from(vec![0xAA; 20]),
            assetId: 1,
            amount: 1000,
        }
        .abi_encode();

        let result = precompile.call(&input, caller, &mut provider);
        assert!(
            result.is_err(),
            "externalWithdraw should reject insufficient balance"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("insufficient balance"), "error should mention balance: {err}");
    }

    #[test]
    fn test_external_deposit_with_fee() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(
            call_precompile::VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            u64_to_u256(1),
        );
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_authorized_contract(1, Address::ZERO),
            U256::from(1u8),
        );
        // Set bridge_fee = 100 (offset+1: 101)
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_bridge_fee(),
            u128_to_u256(101),
        );

        let mut precompile = BridgePrecompile;
        let input = IProtocolBridge::externalDepositCall {
            sourceChain: 1,
            sourceContract: Address::ZERO,
            sourceTxHash: source_tx_hash.into(),
            assetId: 1,
            recipient,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, validator, &mut provider).unwrap();

        // Recipient should receive 900 (net)
        let recipient_balance = provider
            .get(ASSET_ADDRESS, slot_balance(1, recipient))
            .map(|v| u256_to_u128(v))
            .unwrap_or(0);
        assert_eq!(recipient_balance, 900, "recipient should receive net amount after fee");

        // Bridge should hold 100 fee
        let bridge_balance = provider
            .get(ASSET_ADDRESS, slot_balance(1, BRIDGE_ADDRESS))
            .map(|v| u256_to_u128(v))
            .unwrap_or(0);
        assert_eq!(bridge_balance, 100, "bridge should hold fee");

        // Stored deposit amount should be net (900) for challenge reversal
        let stored_amount = provider
            .get(BRIDGE_ADDRESS, slot_bridge_deposit_amount(source_tx_hash))
            .map(|v| u256_to_u128(v))
            .unwrap_or(0);
        assert_eq!(stored_amount, 900, "stored deposit amount should be net for challenge");
    }

    #[test]
    fn test_external_withdraw_with_fee() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let caller = Address::repeat_byte(0x11);

        provider.set(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            U256::from(1u8),
        );
        provider.set(ASSET_ADDRESS, slot_balance(1, caller), u128_to_u256(5000));
        // Set bridge_fee = 100 (offset+1: 101)
        provider.set(
            BRIDGE_ADDRESS,
            slot_bridge_bridge_fee(),
            u128_to_u256(101),
        );

        let mut precompile = BridgePrecompile;
        let input = IProtocolBridge::externalWithdrawCall {
            targetChain: 1,
            targetAddress: alloy_primitives::Bytes::from(vec![0xAA; 20]),
            assetId: 1,
            amount: 1000,
        }
        .abi_encode();
        precompile.call(&input, caller, &mut provider).unwrap();

        // Caller should have 4000 left (5000 - 1000)
        let caller_balance = provider
            .get(ASSET_ADDRESS, slot_balance(1, caller))
            .map(|v| u256_to_u128(v))
            .unwrap_or(0);
        assert_eq!(caller_balance, 4000, "caller should pay gross amount");

        // Bridge should hold 100 fee
        let bridge_balance = provider
            .get(ASSET_ADDRESS, slot_balance(1, BRIDGE_ADDRESS))
            .map(|v| u256_to_u128(v))
            .unwrap_or(0);
        assert_eq!(bridge_balance, 100, "bridge should hold fee");

        // Total withdrawals should be 900 (net)
        let total_withdrawals = provider
            .get(BRIDGE_ADDRESS, slot_bridge_total_withdrawals())
            .map(|v| u256_to_u128(v))
            .unwrap_or(0);
        assert_eq!(total_withdrawals, 900, "total withdrawals should be net amount");
    }
}
