//! Bridge precompile entry point (0x103).
//!
//! Thin wrapper that routes EVM calls to [`BridgeStorage`] backed by
//! [`JournalBackend`].  Business logic lives in [`BridgeStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use alloy_sol_types::{sol, SolCall};
use call_asset::AssetStorage;
use call_precompile::{
    address_to_u256, dispatch, journal_backend::JournalBackend, require_caller,
    slot_asset_meta, storage::storage_slot, u128_to_u256, u256_to_address, u256_to_u128,
    u256_to_u64, u64_to_u256, ASSET_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;
use call_validator::ValidatorStorage;
use revm_precompile::{PrecompileError, PrecompileResult};

pub const BRIDGE_ADDRESS: Address =
    alloy_primitives::address!("0000000000000000000000000000000000000103");

pub const CALL_ASSET_ID: u64 = 1;
pub const DEFAULT_CHALLENGE_PERIOD: u64 = 100;
pub const DEFAULT_CHALLENGE_BOND: u128 = 1000;

// ── Error type ────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum BridgeError {
    AssetNotRegistered,
    BridgePaused,
    AssetNotActive,
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
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::AssetNotRegistered => write!(f, "asset not registered"),
            BridgeError::BridgePaused => write!(f, "bridge paused"),
            BridgeError::AssetNotActive => write!(f, "asset not active"),
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

// ── Challenge status ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeStatus {
    None = 0,
    Pending = 1,
    Successful = 2,
    Failed = 3,
    Withdrawn = 4,
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

    pub fn get_total_deposits(&self) -> u128 {
        u256_to_u128(self.backend.load(BRIDGE_ADDRESS, slot_bridge_total_deposits()))
    }

    pub fn get_total_withdrawals(&self) -> u128 {
        u256_to_u128(self.backend.load(BRIDGE_ADDRESS, slot_bridge_total_withdrawals()))
    }

    pub fn is_paused(&self) -> bool {
        self.backend
            .load(BRIDGE_ADDRESS, slot_bridge_paused())
            .to_be_bytes::<32>()[31]
            != 0
    }

    pub fn is_processed(&self, tx_hash: [u8; 32]) -> bool {
        self.backend
            .load(BRIDGE_ADDRESS, slot_bridge_processed(tx_hash))
            != U256::ZERO
    }

    pub fn read_challenge_status(&self, tx_hash: [u8; 32]) -> ChallengeStatus {
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

    pub fn read_challenge_deadline(&self, tx_hash: [u8; 32]) -> u64 {
        u256_to_u64(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_challenge_deadline(tx_hash)),
        )
    }

    pub fn read_challenge_bond_for_tx(&self, tx_hash: [u8; 32]) -> u128 {
        u256_to_u128(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_challenge_bond(tx_hash)),
        )
    }

    pub fn read_challenge_challenger(&self, tx_hash: [u8; 32]) -> Address {
        u256_to_address(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_challenge_challenger(tx_hash)),
        )
    }

    pub fn read_deposit_asset_id(&self, tx_hash: [u8; 32]) -> u64 {
        u256_to_u64(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_deposit_asset_id(tx_hash)),
        )
    }

    pub fn read_deposit_recipient(&self, tx_hash: [u8; 32]) -> Address {
        u256_to_address(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_deposit_recipient(tx_hash)),
        )
    }

    pub fn read_deposit_amount(&self, tx_hash: [u8; 32]) -> u128 {
        u256_to_u128(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_deposit_amount(tx_hash)),
        )
    }

    pub fn read_deposit_block_height(&self, tx_hash: [u8; 32]) -> u64 {
        u256_to_u64(
            self.backend
                .load(BRIDGE_ADDRESS, slot_bridge_deposit_block_height(tx_hash)),
        )
    }

    pub fn read_challenge_period(&self) -> u64 {
        let stored = u256_to_u64(self.backend.load(BRIDGE_ADDRESS, slot_challenge_period()));
        if stored == 0 {
            DEFAULT_CHALLENGE_PERIOD
        } else {
            stored
        }
    }

    pub fn read_global_challenge_bond(&self) -> u128 {
        let stored = u256_to_u128(self.backend.load(BRIDGE_ADDRESS, slot_challenge_bond_amount()));
        if stored == 0 {
            DEFAULT_CHALLENGE_BOND
        } else {
            stored
        }
    }

    // ── Validation ────────────────────────────────────────────────────

    fn asset_registered(&self, asset_id: u64) -> bool {
        let issuer = u256_to_address(
            self.backend
                .load(ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer")),
        );
        issuer != Address::ZERO
    }

    fn validate_asset(&self, asset_id: u64) -> Result<(), BridgeError> {
        if asset_id == 0 {
            return Err(BridgeError::AssetZeroNotBridgeable);
        }
        if !self.asset_registered(asset_id) {
            return Err(BridgeError::AssetNotRegistered);
        }
        if self.is_paused() {
            return Err(BridgeError::BridgePaused);
        }
        let status = self
            .backend
            .load(ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
            .to_be_bytes::<32>()[31];
        if status != 0 {
            return Err(BridgeError::AssetNotActive);
        }
        Ok(())
    }

    fn validate_basic(&self, asset_id: u64) -> Result<(), BridgeError> {
        if !self.asset_registered(asset_id) {
            return Err(BridgeError::AssetNotRegistered);
        }
        if self.is_paused() {
            return Err(BridgeError::BridgePaused);
        }
        Ok(())
    }

    // ── Write operations ──────────────────────────────────────────────

    pub fn bridge_to_evm(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        asset_id: u64,
        _to: Address,
        amount: u128,
        caller: Address,
    ) -> Result<(), BridgeError> {
        self.validate_asset(asset_id)?;
        asset_store.deduct_balance(asset_id, caller, amount)?;
        self.add_total_withdrawals(amount);
        Ok(())
    }

    pub fn bridge_to_protocol(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        asset_id: u64,
        to: Address,
        amount: u128,
    ) -> Result<(), BridgeError> {
        self.validate_asset(asset_id)?;
        asset_store.add_balance(asset_id, to, amount)?;
        self.add_total_deposits(amount);
        Ok(())
    }

    pub fn external_deposit(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        source_tx_hash: [u8; 32],
        asset_id: u64,
        recipient: Address,
        amount: u128,
        validator: Address,
        block_height: u64,
    ) -> Result<(), BridgeError> {
        if self.is_processed(source_tx_hash) {
            return Err(BridgeError::AlreadyProcessed);
        }
        self.validate_basic(asset_id)?;

        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_processed(source_tx_hash),
            U256::from(1u8),
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
            u128_to_u256(amount),
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

        asset_store.add_balance(asset_id, recipient, amount)?;
        self.add_total_deposits(amount);
        Ok(())
    }

    pub fn external_withdraw(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        asset_id: u64,
        amount: u128,
        caller: Address,
    ) -> Result<(), BridgeError> {
        self.validate_basic(asset_id)?;
        asset_store.deduct_balance(asset_id, caller, amount)?;
        self.add_total_withdrawals(amount);
        Ok(())
    }

    pub fn deposit(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        _source_chain: u64,
        target_address: Address,
        amount: u128,
        asset_id: u64,
        _proof: Vec<u8>,
    ) -> Result<(), BridgeError> {
        self.validate_basic(asset_id)?;
        asset_store.add_balance(asset_id, target_address, amount)?;
        self.add_total_deposits(amount);
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
        if !self.is_processed(source_tx_hash) {
            return Err(BridgeError::NotProcessed);
        }
        if self.read_challenge_status(source_tx_hash) != ChallengeStatus::None {
            return Err(BridgeError::AlreadyChallenged);
        }

        let deposit_height = self.read_deposit_block_height(source_tx_hash);
        let challenge_period = self.read_challenge_period();
        if current_block >= deposit_height + challenge_period {
            return Err(BridgeError::ChallengePeriodExpired);
        }

        let bond = self.read_global_challenge_bond();
        asset_store.deduct_balance(CALL_ASSET_ID, challenger, bond)?;

        let deadline = current_block + challenge_period;
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

        Ok(())
    }

    pub fn resolve_challenge(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        validator_store: &mut ValidatorStorage<B>,
        source_tx_hash: [u8; 32],
        current_block: u64,
    ) -> Result<bool, BridgeError> {
        if self.read_challenge_status(source_tx_hash) != ChallengeStatus::Pending {
            return Err(BridgeError::ChallengeNotPending);
        }

        let deadline = self.read_challenge_deadline(source_tx_hash);
        if current_block < deadline {
            return Err(BridgeError::ChallengeDeadlineNotReached);
        }

        let proof_valid = self.verify_fraud_proof(source_tx_hash);
        let bond = self.read_challenge_bond_for_tx(source_tx_hash);

        if proof_valid {
            let asset_id = self.read_deposit_asset_id(source_tx_hash);
            let recipient = self.read_deposit_recipient(source_tx_hash);
            let amount = self.read_deposit_amount(source_tx_hash);

            let _ = asset_store.deduct_balance(asset_id, recipient, amount);
            self.sub_total_deposits(amount);

            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_processed(source_tx_hash),
                U256::ZERO,
            );

            let challenger = self.read_challenge_challenger(source_tx_hash);
            let reward = bond + bond / 10;
            asset_store
                .add_balance(CALL_ASSET_ID, challenger, reward)
                .map_err(|_| BridgeError::BalanceOverflow)?;

            let validator = u256_to_address(
                self.backend
                    .load(BRIDGE_ADDRESS, slot_bridge_challenge_original_validator(source_tx_hash)),
            );
            let _ = validator_store.slash_stake(asset_store, validator);

            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_status(source_tx_hash),
                U256::from(2u8),
            );
            Ok(true)
        } else {
            let validator = u256_to_address(
                self.backend
                    .load(BRIDGE_ADDRESS, slot_bridge_challenge_original_validator(source_tx_hash)),
            );
            asset_store
                .add_balance(CALL_ASSET_ID, validator, bond)
                .map_err(|_| BridgeError::BalanceOverflow)?;

            self.backend.store(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_status(source_tx_hash),
                U256::from(3u8),
            );
            Ok(false)
        }
    }

    pub fn withdraw_challenge_bond(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        source_tx_hash: [u8; 32],
        caller: Address,
    ) -> Result<(), BridgeError> {
        if self.read_challenge_status(source_tx_hash) != ChallengeStatus::Successful {
            return Err(BridgeError::ChallengeNotSuccessful);
        }
        let challenger = self.read_challenge_challenger(source_tx_hash);
        if challenger != caller {
            return Err(BridgeError::NotChallenger);
        }

        let bond = self.read_challenge_bond_for_tx(source_tx_hash);
        let reward = bond + bond / 10;
        asset_store
            .add_balance(CALL_ASSET_ID, challenger, reward)
            .map_err(|_| BridgeError::BalanceOverflow)?;

        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_challenge_status(source_tx_hash),
            U256::from(4u8),
        );
        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────

    fn add_total_deposits(&mut self, amount: u128) {
        let total = self.get_total_deposits();
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_total_deposits(),
            u128_to_u256(total + amount),
        );
    }

    fn add_total_withdrawals(&mut self, amount: u128) {
        let total = self.get_total_withdrawals();
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_total_withdrawals(),
            u128_to_u256(total + amount),
        );
    }

    fn sub_total_deposits(&mut self, amount: u128) {
        let total = self.get_total_deposits();
        self.backend.store(
            BRIDGE_ADDRESS,
            slot_bridge_total_deposits(),
            u128_to_u256(total.saturating_sub(amount)),
        );
    }

    fn verify_fraud_proof(&self, source_tx_hash: [u8; 32]) -> bool {
        let proof_hash = self
            .backend
            .load(BRIDGE_ADDRESS, slot_bridge_challenge_proof_hash(source_tx_hash));
        if proof_hash == U256::ZERO {
            return false;
        }
        // Real verification would check the proof against source chain state.
        // Safe default: return false (challenge fails) until crypto logic is implemented.
        false
    }
}

// ── sol! interface ────────────────────────────────────────────────────

sol! {
    interface IProtocolBridge {
        function getTotalDeposits() external view returns (uint128);
        function getTotalWithdrawals() external view returns (uint128);
        function bridgeToEvm(uint64 assetId, address to, uint128 amount) external;
        function bridgeToProtocol(uint64 assetId, address to, uint128 amount) external;
        function externalDeposit(bytes32 sourceTxHash, uint64 assetId, address recipient, uint128 amount) external;
        function externalWithdraw(uint64 targetChain, bytes calldata targetAddress, uint64 assetId, uint128 amount) external;
        function deposit(uint64 sourceChain, address targetAddress, uint128 amount, uint64 assetId, bytes calldata proof) external;
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
    fn get_total_deposits(&self, calldata: &[u8]) -> PrecompileResult {
        dispatch::view::<IProtocolBridge::getTotalDepositsCall, _, _>(calldata, 1500, |_call| {
            let store = BridgeStorage::new(JournalBackend);
            Ok(store.get_total_deposits())
        })
    }

    fn get_total_withdrawals(&self, calldata: &[u8]) -> PrecompileResult {
        dispatch::view::<IProtocolBridge::getTotalWithdrawalsCall, _, _>(calldata, 1500, |_call| {
            let store = BridgeStorage::new(JournalBackend);
            Ok(store.get_total_withdrawals())
        })
    }

    fn bridge_to_evm(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::bridgeToEvmCall, _>(calldata, 30000, |call| {
            let caller = require_caller(msg_sender)?;
            let backend = JournalBackend;
            let mut bridge_store = BridgeStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            bridge_store
                .bridge_to_evm(&mut asset_store, call.assetId, call.to, call.amount, caller)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }

    fn bridge_to_protocol(&self, calldata: &[u8], _msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::bridgeToProtocolCall, _>(calldata, 30000, |call| {
            let backend = JournalBackend;
            let mut bridge_store = BridgeStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            bridge_store
                .bridge_to_protocol(&mut asset_store, call.assetId, call.to, call.amount)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }

    fn external_deposit(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::externalDepositCall, _>(calldata, 30000, |call| {
            let validator = require_caller(msg_sender)?;
            let backend = JournalBackend;
            let mut bridge_store = BridgeStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            let block_height = call_precompile::storage::StorageCtx::block_number();
            bridge_store
                .external_deposit(
                    &mut asset_store,
                    call.sourceTxHash.into(),
                    call.assetId,
                    call.recipient,
                    call.amount,
                    validator,
                    block_height,
                )
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }

    fn external_withdraw(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::externalWithdrawCall, _>(calldata, 30000, |call| {
            let caller = require_caller(msg_sender)?;
            let backend = JournalBackend;
            let mut bridge_store = BridgeStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            bridge_store
                .external_withdraw(&mut asset_store, call.assetId, call.amount, caller)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }

    fn deposit(&self, calldata: &[u8], _msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::depositCall, _>(calldata, 30000, |call| {
            let backend = JournalBackend;
            let mut bridge_store = BridgeStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            bridge_store
                .deposit(
                    &mut asset_store,
                    call.sourceChain,
                    call.targetAddress,
                    call.amount,
                    call.assetId,
                    call.proof.to_vec(),
                )
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }

    fn initiate_challenge(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::initiateChallengeCall, _>(calldata, 50000, |call| {
            let challenger = require_caller(msg_sender)?;
            let backend = JournalBackend;
            let mut bridge_store = BridgeStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            let block_number = call_precompile::storage::StorageCtx::block_number();
            bridge_store
                .initiate_challenge(
                    &mut asset_store,
                    call.sourceTxHash.into(),
                    call.proof.to_vec(),
                    challenger,
                    block_number,
                )
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }

    fn resolve_challenge(&self, calldata: &[u8], _msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::resolveChallengeCall, _>(calldata, 100000, |call| {
            let backend = JournalBackend;
            let mut bridge_store = BridgeStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            let mut validator_store = ValidatorStorage::new(backend);
            let block_number = call_precompile::storage::StorageCtx::block_number();
            bridge_store
                .resolve_challenge(
                    &mut asset_store,
                    &mut validator_store,
                    call.sourceTxHash.into(),
                    block_number,
                )
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }

    fn get_challenge_status(&self, calldata: &[u8]) -> PrecompileResult {
        dispatch::view::<IProtocolBridge::getChallengeStatusCall, _, _>(calldata, 1500, |call| {
            let store = BridgeStorage::new(JournalBackend);
            let status = store.read_challenge_status(call.sourceTxHash.into());
            let deadline = store.read_challenge_deadline(call.sourceTxHash.into());
            let bond = store.read_challenge_bond_for_tx(call.sourceTxHash.into());
            let challenger = store.read_challenge_challenger(call.sourceTxHash.into());
            Ok((status as u64, deadline, bond, challenger))
        })
    }

    fn withdraw_challenge_bond(&self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolBridge::withdrawChallengeBondCall, _>(calldata, 5000, |call| {
            let caller = require_caller(msg_sender)?;
            let backend = JournalBackend;
            let mut bridge_store = BridgeStorage::new(backend);
            let mut asset_store = AssetStorage::new(backend);
            bridge_store
                .withdraw_challenge_bond(&mut asset_store, call.sourceTxHash.into(), caller)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            Ok(())
        })
    }
}

impl call_precompile::StatefulPrecompile for BridgePrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let selector: [u8; 4] = calldata[..4].try_into().unwrap();
        match selector {
            IProtocolBridge::getTotalDepositsCall::SELECTOR => self.get_total_deposits(calldata),
            IProtocolBridge::getTotalWithdrawalsCall::SELECTOR => {
                self.get_total_withdrawals(calldata)
            }
            IProtocolBridge::bridgeToEvmCall::SELECTOR => self.bridge_to_evm(calldata, msg_sender),
            IProtocolBridge::bridgeToProtocolCall::SELECTOR => {
                self.bridge_to_protocol(calldata, msg_sender)
            }
            IProtocolBridge::externalDepositCall::SELECTOR => {
                self.external_deposit(calldata, msg_sender)
            }
            IProtocolBridge::externalWithdrawCall::SELECTOR => {
                self.external_withdraw(calldata, msg_sender)
            }
            IProtocolBridge::depositCall::SELECTOR => self.deposit(calldata, msg_sender),
            IProtocolBridge::initiateChallengeCall::SELECTOR => {
                self.initiate_challenge(calldata, msg_sender)
            }
            IProtocolBridge::resolveChallengeCall::SELECTOR => {
                self.resolve_challenge(calldata, msg_sender)
            }
            IProtocolBridge::getChallengeStatusCall::SELECTOR => {
                self.get_challenge_status(calldata)
            }
            IProtocolBridge::withdrawChallengeBondCall::SELECTOR => {
                self.withdraw_challenge_bond(calldata, msg_sender)
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
    use call_precompile::{slot_balance, u128_to_u256, u256_to_u128, StatefulPrecompile};
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

        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            call_precompile::storage::StorageCtx::sstore(
                BRIDGE_ADDRESS,
                U256::from(0),
                u128_to_u256(5000),
            );
            call_precompile::storage::StorageCtx::sstore(
                BRIDGE_ADDRESS,
                U256::from(1),
                u128_to_u256(2000),
            );

            let mut precompile = BridgePrecompile;

            let input = IProtocolBridge::getTotalDepositsCall {}.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let deposits = u256_to_u128(U256::from_be_bytes::<32>(
                result.bytes.as_ref().try_into().unwrap(),
            ));
            assert_eq!(deposits, 5000);

            let input = IProtocolBridge::getTotalWithdrawalsCall {}.abi_encode();
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let withdrawals = u256_to_u128(U256::from_be_bytes::<32>(
                result.bytes.as_ref().try_into().unwrap(),
            ));
            assert_eq!(withdrawals, 2000);
        });
    }

    #[test]
    fn test_external_deposit_records_metadata() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            // Register asset_id=1
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                U256::from(1u8),
            );

            let mut precompile = BridgePrecompile;

            let input = IProtocolBridge::externalDepositCall {
                sourceTxHash: source_tx_hash.into(),
                assetId: 1,
                recipient,
                amount: 1000,
            }
            .abi_encode();

            let result = precompile.call(&input, validator);
            assert!(result.is_ok(), "external_deposit failed: {:?}", result.err());

            let stored_asset_id =
                u256_to_u64(call_precompile::storage::StorageCtx::sload(
                    BRIDGE_ADDRESS,
                    slot_bridge_deposit_asset_id(source_tx_hash),
                ).unwrap_or(U256::ZERO));
            assert_eq!(stored_asset_id, 1);

            let stored_recipient = u256_to_address(
                call_precompile::storage::StorageCtx::sload(
                    BRIDGE_ADDRESS,
                    slot_bridge_deposit_recipient(source_tx_hash),
                ).unwrap_or(U256::ZERO),
            );
            assert_eq!(stored_recipient, recipient);

            let stored_amount = u256_to_u128(
                call_precompile::storage::StorageCtx::sload(
                    BRIDGE_ADDRESS,
                    slot_bridge_deposit_amount(source_tx_hash),
                ).unwrap_or(U256::ZERO),
            );
            assert_eq!(stored_amount, 1000);

            let stored_height = u256_to_u64(
                call_precompile::storage::StorageCtx::sload(
                    BRIDGE_ADDRESS,
                    slot_bridge_deposit_block_height(source_tx_hash),
                ).unwrap_or(U256::ZERO),
            );
            assert_eq!(stored_height, 10);

            let stored_validator = u256_to_address(
                call_precompile::storage::StorageCtx::sload(
                    BRIDGE_ADDRESS,
                    slot_bridge_challenge_original_validator(source_tx_hash),
                ).unwrap_or(U256::ZERO),
            );
            assert_eq!(stored_validator, validator);
        });
    }

    #[test]
    fn test_initiate_challenge_success() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            // Register asset_id=1 and seed challenger balance
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                U256::from(1u8),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            // 1. externalDeposit
            let input = IProtocolBridge::externalDepositCall {
                sourceTxHash: source_tx_hash.into(),
                assetId: 1,
                recipient,
                amount: 1000,
            }
            .abi_encode();
            precompile.call(&input, validator).unwrap();

            // 2. initiateChallenge at block 10 (within period)
            let input = IProtocolBridge::initiateChallengeCall {
                sourceTxHash: source_tx_hash.into(),
                proof: alloy_primitives::Bytes::from_static(b"proof"),
            }
            .abi_encode();

            let result = precompile.call(&input, challenger);
            assert!(result.is_ok(), "initiate_challenge failed: {:?}", result.err());

            let status = call_precompile::storage::StorageCtx::sload(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_status(source_tx_hash),
            )
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
            assert_eq!(status, 1); // Pending

            let stored_challenger = u256_to_address(
                call_precompile::storage::StorageCtx::sload(
                    BRIDGE_ADDRESS,
                    slot_bridge_challenge_challenger(source_tx_hash),
                )
                .unwrap_or(U256::ZERO),
            );
            assert_eq!(stored_challenger, challenger);

            let deadline = u256_to_u64(
                call_precompile::storage::StorageCtx::sload(
                    BRIDGE_ADDRESS,
                    slot_bridge_challenge_deadline(source_tx_hash),
                )
                .unwrap_or(U256::ZERO),
            );
            assert_eq!(deadline, 110); // block 10 + period 100

            // Bond deducted from challenger
            let challenger_bal = u256_to_u128(
                call_precompile::storage::StorageCtx::sload(
                    ASSET_ADDRESS,
                    slot_balance(CALL_ASSET_ID, challenger),
                )
                .unwrap_or(U256::ZERO),
            );
            assert_eq!(challenger_bal, 4000); // 5000 - 1000 bond
        });
    }

    #[test]
    fn test_initiate_challenge_fails_not_processed() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let challenger = Address::repeat_byte(0x33);
        let source_tx_hash = [0xABu8; 32];

        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = BridgePrecompile;

            let input = IProtocolBridge::initiateChallengeCall {
                sourceTxHash: source_tx_hash.into(),
                proof: alloy_primitives::Bytes::from_static(b"proof"),
            }
            .abi_encode();

            let result = precompile.call(&input, challenger);
            assert!(result.is_err(), "should fail: source tx not processed");
        });
    }

    #[test]
    fn test_initiate_challenge_fails_period_expired() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        // externalDeposit at block 10
        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                U256::from(1u8),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            let input = IProtocolBridge::externalDepositCall {
                sourceTxHash: source_tx_hash.into(),
                assetId: 1,
                recipient,
                amount: 1000,
            }
            .abi_encode();
            precompile.call(&input, validator).unwrap();
        });

        // initiateChallenge at block 200 (deposit at block 10, period = 100, so expired)
        provider.set_block_number(200);
        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = BridgePrecompile;

            let input = IProtocolBridge::initiateChallengeCall {
                sourceTxHash: source_tx_hash.into(),
                proof: alloy_primitives::Bytes::from_static(b"proof"),
            }
            .abi_encode();

            let result = precompile.call(&input, challenger);
            assert!(result.is_err(), "should fail: challenge period expired");
        });
    }

    #[test]
    fn test_initiate_challenge_fails_already_challenged() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                U256::from(1u8),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            // externalDeposit
            let input = IProtocolBridge::externalDepositCall {
                sourceTxHash: source_tx_hash.into(),
                assetId: 1,
                recipient,
                amount: 1000,
            }
            .abi_encode();
            precompile.call(&input, validator).unwrap();

            // First challenge
            let input = IProtocolBridge::initiateChallengeCall {
                sourceTxHash: source_tx_hash.into(),
                proof: alloy_primitives::Bytes::from_static(b"proof"),
            }
            .abi_encode();
            precompile.call(&input, challenger).unwrap();

            // Second challenge should fail
            let result = precompile.call(&input, challenger);
            assert!(result.is_err(), "should fail: already challenged");
        });
    }

    #[test]
    fn test_resolve_challenge_false_proof_bond_forfeited() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        // Setup state and externalDeposit + initiateChallenge at block 10
        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                U256::from(1u8),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, validator),
                u128_to_u256(100),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            // externalDeposit
            let input = IProtocolBridge::externalDepositCall {
                sourceTxHash: source_tx_hash.into(),
                assetId: 1,
                recipient,
                amount: 1000,
            }
            .abi_encode();
            precompile.call(&input, validator).unwrap();

            // initiateChallenge
            let input = IProtocolBridge::initiateChallengeCall {
                sourceTxHash: source_tx_hash.into(),
                proof: alloy_primitives::Bytes::from_static(b"proof"),
            }
            .abi_encode();
            precompile.call(&input, challenger).unwrap();
        });

        // resolveChallenge at block 120 (deadline = 10 + 100 = 110)
        provider.set_block_number(120);
        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = BridgePrecompile;

            let input = IProtocolBridge::resolveChallengeCall {
                sourceTxHash: source_tx_hash.into(),
            }
            .abi_encode();

            let result = precompile.call(&input, Address::ZERO);
            assert!(result.is_ok(), "resolve_challenge failed: {:?}", result.err());

            // Status should be Failed (3)
            let status = call_precompile::storage::StorageCtx::sload(
                BRIDGE_ADDRESS,
                slot_bridge_challenge_status(source_tx_hash),
            )
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);
            assert_eq!(status, 3);

            // Validator should have received the bond
            let validator_bal = u256_to_u128(
                call_precompile::storage::StorageCtx::sload(
                    ASSET_ADDRESS,
                    slot_balance(CALL_ASSET_ID, validator),
                )
                .unwrap_or(U256::ZERO),
            );
            assert_eq!(validator_bal, 1100); // 100 + 1000 bond
        });
    }

    #[test]
    fn test_get_challenge_status() {
        let mut provider = HashMapStorageProvider::with_block(1_000_000, 1, 10);
        let validator = Address::repeat_byte(0x11);
        let challenger = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x22);
        let source_tx_hash = [0xABu8; 32];

        call_precompile::storage::StorageCtx::enter(&mut provider, || {
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
                U256::from(1u8),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, challenger),
                u128_to_u256(5000),
            );
            call_precompile::storage::StorageCtx::sstore(
                ASSET_ADDRESS,
                slot_balance(1, recipient),
                u128_to_u256(0),
            );

            let mut precompile = BridgePrecompile;

            // externalDeposit
            let input = IProtocolBridge::externalDepositCall {
                sourceTxHash: source_tx_hash.into(),
                assetId: 1,
                recipient,
                amount: 1000,
            }
            .abi_encode();
            precompile.call(&input, validator).unwrap();

            // initiateChallenge
            let input = IProtocolBridge::initiateChallengeCall {
                sourceTxHash: source_tx_hash.into(),
                proof: alloy_primitives::Bytes::from_static(b"proof"),
            }
            .abi_encode();
            precompile.call(&input, challenger).unwrap();

            // getChallengeStatus
            let input = IProtocolBridge::getChallengeStatusCall {
                sourceTxHash: source_tx_hash.into(),
            }
            .abi_encode();

            let result = precompile.call(&input, Address::ZERO).unwrap();
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
        });
    }
}
