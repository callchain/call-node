pub mod precompile;

pub use precompile::ValidatorPrecompile;

use call_asset::AssetStorage;
use call_precompile::storage::storage_slot;
use call_precompile::{
    address_to_u256, u128_to_u256, u256_to_address, u256_to_u128, u256_to_u64, u64_to_u256,
    VALIDATOR_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;

/// Error type for validator operations.
#[derive(Debug)]
pub enum ValidatorError {
    InsufficientBalance,
    BalanceOverflow,
    EscrowOverflow,
    EscrowUnderflow,
    BelowMinimumStake,
    AlreadyStaked,
    NotAValidator,
    IdMismatch,
    AlreadyUnbonding,
    NotUnbonding,
    UnbondingPeriodNotElapsed,
    NoUnbondingRequest,
}

impl std::fmt::Display for ValidatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidatorError::InsufficientBalance => write!(f, "insufficient balance"),
            ValidatorError::BalanceOverflow => write!(f, "balance overflow"),
            ValidatorError::EscrowOverflow => write!(f, "escrow overflow"),
            ValidatorError::EscrowUnderflow => write!(f, "escrow underflow"),
            ValidatorError::BelowMinimumStake => write!(f, "below minimum self-stake"),
            ValidatorError::AlreadyStaked => write!(f, "already staked"),
            ValidatorError::NotAValidator => write!(f, "not a validator"),
            ValidatorError::IdMismatch => write!(f, "validator id mismatch"),
            ValidatorError::AlreadyUnbonding => write!(f, "already unbonding"),
            ValidatorError::NotUnbonding => write!(f, "not unbonding"),
            ValidatorError::UnbondingPeriodNotElapsed => write!(f, "unbonding period not elapsed"),
            ValidatorError::NoUnbondingRequest => write!(f, "no unbonding request found"),
        }
    }
}

impl std::error::Error for ValidatorError {}

/// Asset ID for the native CALL token used for staking.
pub const CALL_ASSET_ID: u64 = 1;

/// Minimum self-stake required to become a validator.
pub const MIN_SELF_STAKE: u128 = 1_000_000;

/// Number of blocks before unbonded stake can be claimed.
pub const UNBONDING_PERIOD_BLOCKS: u64 = 120_960;

/// Escrow address where staked CALL tokens are held.
pub const STAKING_ESCROW: Address =
    alloy_primitives::address!("0000000000000000000000000000000000000ACE");

// ── Storage slot helpers ──────────────────────────────────────────────

pub fn slot_validator_count() -> U256 {
    U256::ZERO
}

pub fn slot_validator_addr(index: u64) -> U256 {
    storage_slot(&[b"validators"]) + U256::from(index)
}

pub fn slot_validator_stake(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"stake"])
}

pub fn slot_validator_pubkey(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"pubkey"])
}

pub fn slot_validator_status(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"status"])
}

pub fn slot_validator_unbond_height(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"unbond_at"])
}

pub fn slot_unbonding_count() -> U256 {
    U256::from(1)
}

pub fn slot_unbonding(index: u64) -> U256 {
    storage_slot(&[b"unbonding"]) + U256::from(index)
}

// ── ValidatorStorage ──────────────────────────────────────────────────

/// Business logic for validator operations backed by any StorageBackend.
pub struct ValidatorStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> ValidatorStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    // ── Read operations ───────────────────────────────────────────────

    pub fn read_validator_count(&self) -> u64 {
        self.backend
            .load(VALIDATOR_ADDRESS, slot_validator_count())
            .try_into()
            .map(|v: u128| v as u64)
            .unwrap_or(0)
    }

    pub fn read_validator_id(&self, addr: Address) -> u64 {
        let slot = call_precompile::slot_validator_by_addr(addr);
        self.backend
            .load(VALIDATOR_ADDRESS, slot)
            .try_into()
            .map(|v: u128| v as u64)
            .unwrap_or(0)
    }

    pub fn read_validator_by_index(&self, index: u64) -> Address {
        u256_to_address(
            self.backend
                .load(VALIDATOR_ADDRESS, slot_validator_addr(index)),
        )
    }

    pub fn read_stake(&self, addr: Address) -> u128 {
        u256_to_u128(
            self.backend
                .load(VALIDATOR_ADDRESS, slot_validator_stake(addr)),
        )
    }

    pub fn read_status(&self, addr: Address) -> u8 {
        self.backend
            .load(VALIDATOR_ADDRESS, slot_validator_status(addr))
            .to_be_bytes::<32>()[31]
    }

    pub fn read_pubkey(&self, addr: Address) -> [u8; 32] {
        self.backend
            .load(VALIDATOR_ADDRESS, slot_validator_pubkey(addr))
            .to_be_bytes::<32>()
    }

    pub fn read_unbond_height(&self, addr: Address) -> u64 {
        u256_to_u64(
            self.backend
                .load(VALIDATOR_ADDRESS, slot_validator_unbond_height(addr)),
        )
    }

    pub fn read_unbonding_count(&self) -> u64 {
        u256_to_u64(self.backend.load(VALIDATOR_ADDRESS, slot_unbonding_count()))
    }

    // ── Write operations ──────────────────────────────────────────────

    pub fn stake(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        pubkey: [u8; 32],
        amount: u128,
        caller: Address,
    ) -> Result<(), ValidatorError> {
        if amount < MIN_SELF_STAKE {
            return Err(ValidatorError::BelowMinimumStake);
        }

        let existing_id = self.read_validator_id(caller);
        if existing_id != 0 {
            return Err(ValidatorError::AlreadyStaked);
        }

        // Deduct CALL from sender, credit escrow
        asset_store
            .deduct_balance(CALL_ASSET_ID, caller, amount)
            .map_err(|_| ValidatorError::InsufficientBalance)?;
        asset_store
            .add_balance(CALL_ASSET_ID, STAKING_ESCROW, amount)
            .map_err(|_| ValidatorError::EscrowOverflow)?;

        // Register validator
        let count = self.read_validator_count();
        let validator_id = count + 1;
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_count(),
            u64_to_u256(validator_id),
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(caller),
            u64_to_u256(validator_id),
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_addr(validator_id),
            address_to_u256(caller),
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_stake(caller),
            u128_to_u256(amount),
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_pubkey(caller),
            U256::from_be_slice(&pubkey),
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_status(caller),
            U256::from(1u8),
        );

        Ok(())
    }

    pub fn unstake(
        &mut self,
        validator_id: u64,
        caller: Address,
        current_block: u64,
    ) -> Result<(), ValidatorError> {
        let stored_id = self.read_validator_id(caller);
        if stored_id == 0 {
            return Err(ValidatorError::NotAValidator);
        }
        if stored_id != validator_id {
            return Err(ValidatorError::IdMismatch);
        }

        let status = self.read_status(caller);
        if status != 1 {
            return Err(ValidatorError::AlreadyUnbonding);
        }

        let stake = self.read_stake(caller);

        // Set status to unbonding (2)
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_status(caller),
            U256::from(2u8),
        );

        // Record unbond height
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_unbond_height(caller),
            u64_to_u256(current_block),
        );

        // Add to unbonding queue
        let unbonding_count = self.read_unbonding_count();
        let mut packed = [0u8; 32];
        packed[8..16].copy_from_slice(&stored_id.to_be_bytes());
        packed[16..32].copy_from_slice(&stake.to_be_bytes());
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_unbonding(unbonding_count),
            U256::from_be_slice(&packed),
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_unbonding_count(),
            u64_to_u256(unbonding_count + 1),
        );

        Ok(())
    }

    #[allow(clippy::expect_used)]
    pub fn claim_unbonded(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        validator_id: u64,
        caller: Address,
        current_block: u64,
    ) -> Result<(), ValidatorError> {
        let stored_id = self.read_validator_id(caller);
        if stored_id == 0 {
            return Err(ValidatorError::NotAValidator);
        }
        if stored_id != validator_id {
            return Err(ValidatorError::IdMismatch);
        }

        let status = self.read_status(caller);
        if status != 2 {
            return Err(ValidatorError::NotUnbonding);
        }

        let unbond_height = self.read_unbond_height(caller);
        if current_block < unbond_height + UNBONDING_PERIOD_BLOCKS {
            return Err(ValidatorError::UnbondingPeriodNotElapsed);
        }

        // Find and remove unbonding request from queue
        let unbonding_count = self.read_unbonding_count();
        let mut amount = 0u128;
        let mut found_idx = None;
        for i in 0..unbonding_count {
            let packed = self
                .backend
                .load(VALIDATOR_ADDRESS, slot_unbonding(i))
                .to_be_bytes::<32>();
            let entry_id = u64::from_be_bytes(packed[8..16].try_into().expect("fixed slice"));
            if entry_id == stored_id {
                amount = u128::from_be_bytes(packed[16..32].try_into().expect("fixed slice"));
                found_idx = Some(i);
                break;
            }
        }
        let found_idx = found_idx.ok_or(ValidatorError::NoUnbondingRequest)?;

        // Swap-and-pop
        if unbonding_count > 1 && found_idx != unbonding_count - 1 {
            let last = self
                .backend
                .load(VALIDATOR_ADDRESS, slot_unbonding(unbonding_count - 1));
            self.backend
                .store(VALIDATOR_ADDRESS, slot_unbonding(found_idx), last);
        }
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_unbonding(unbonding_count - 1),
            U256::ZERO,
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_unbonding_count(),
            u64_to_u256(unbonding_count - 1),
        );

        // Return stake from escrow to sender
        asset_store
            .deduct_balance(CALL_ASSET_ID, STAKING_ESCROW, amount)
            .map_err(|_| ValidatorError::EscrowUnderflow)?;
        asset_store
            .add_balance(CALL_ASSET_ID, caller, amount)
            .map_err(|_| ValidatorError::BalanceOverflow)?;

        // Clear validator state
        self.backend.store(
            VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(caller),
            U256::ZERO,
        );
        self.backend
            .store(VALIDATOR_ADDRESS, slot_validator_stake(caller), U256::ZERO);
        self.backend
            .store(VALIDATOR_ADDRESS, slot_validator_status(caller), U256::ZERO);
        self.backend
            .store(VALIDATOR_ADDRESS, slot_validator_pubkey(caller), U256::ZERO);
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_unbond_height(caller),
            U256::ZERO,
        );

        Ok(())
    }

    /// Slash a validator's stake. Deducts staked amount from escrow and clears
    /// all validator state. Returns the slashed amount (0 if not a validator).
    pub fn slash_stake(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        validator: Address,
    ) -> Result<u128, ValidatorError> {
        let validator_id = self.read_validator_id(validator);
        if validator_id == 0 {
            return Ok(0);
        }

        let stake = self.read_stake(validator);

        if stake > 0 {
            asset_store
                .deduct_balance(CALL_ASSET_ID, STAKING_ESCROW, stake)
                .map_err(|_| ValidatorError::EscrowUnderflow)?;
        }

        // Clear validator state
        self.backend.store(
            VALIDATOR_ADDRESS,
            call_precompile::slot_validator_by_addr(validator),
            U256::ZERO,
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_stake(validator),
            U256::ZERO,
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_status(validator),
            U256::ZERO,
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_pubkey(validator),
            U256::ZERO,
        );
        self.backend.store(
            VALIDATOR_ADDRESS,
            slot_validator_unbond_height(validator),
            U256::ZERO,
        );

        Ok(stake)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_asset::AssetStorage;
    use call_precompile::storage::{HashMapStorageProvider, StorageProvider};
    use call_precompile::{
        journal_backend::JournalBackend, slot_balance, u128_to_u256, u256_to_u128, ASSET_ADDRESS,
    };
    use call_primitives::Address;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn seed_balance(provider: &mut HashMapStorageProvider, addr: Address, amount: u128) {
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, addr),
                u128_to_u256(amount),
            )
            .unwrap();
    }

    // ── Stake tests ────────────────────────────────────────────────────

    #[test]
    fn test_stake_success() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        let result = validator_store.stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller);
        assert!(result.is_ok(), "stake failed: {:?}", result.err());

        assert_eq!(validator_store.read_stake(caller), 5_000_000);
        assert_eq!(validator_store.read_status(caller), 1);
        assert_eq!(validator_store.read_validator_id(caller), 1);
        assert_eq!(validator_store.read_validator_count(), 1);
        assert_eq!(validator_store.read_validator_by_index(1), caller);
        assert_eq!(validator_store.read_pubkey(caller), [0xAAu8; 32]);
    }

    #[test]
    fn test_stake_below_minimum() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        let result = validator_store.stake(
            &mut asset_store,
            [0xAAu8; 32],
            500, // below MIN_SELF_STAKE
            caller,
        );
        assert!(
            matches!(result, Err(ValidatorError::BelowMinimumStake)),
            "expected BelowMinimumStake, got {:?}",
            result
        );
    }

    #[test]
    fn test_stake_already_staked() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        validator_store
            .stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller)
            .unwrap();

        let result = validator_store.stake(&mut asset_store, [0xBBu8; 32], 5_000_000, caller);
        assert!(
            matches!(result, Err(ValidatorError::AlreadyStaked)),
            "expected AlreadyStaked, got {:?}",
            result
        );
    }

    #[test]
    fn test_stake_insufficient_balance() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 500); // less than MIN_SELF_STAKE

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        let result = validator_store.stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller);
        assert!(
            matches!(result, Err(ValidatorError::InsufficientBalance)),
            "expected InsufficientBalance, got {:?}",
            result
        );
    }

    // ── Unstake tests ──────────────────────────────────────────────────

    #[test]
    fn test_unstake_success() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        validator_store
            .stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller)
            .unwrap();

        let result = validator_store.unstake(1, caller, 100);
        assert!(result.is_ok(), "unstake failed: {:?}", result.err());

        assert_eq!(validator_store.read_status(caller), 2); // unbonding
        assert_eq!(validator_store.read_unbond_height(caller), 100);
        assert_eq!(validator_store.read_unbonding_count(), 1);
    }

    #[test]
    fn test_unstake_not_validator() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        let result = validator_store.unstake(1, caller, 100);
        assert!(
            matches!(result, Err(ValidatorError::NotAValidator)),
            "expected NotAValidator, got {:?}",
            result
        );
    }

    #[test]
    fn test_unstake_already_unbonding() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        validator_store
            .stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller)
            .unwrap();
        validator_store.unstake(1, caller, 100).unwrap();

        let result = validator_store.unstake(1, caller, 200);
        assert!(
            matches!(result, Err(ValidatorError::AlreadyUnbonding)),
            "expected AlreadyUnbonding, got {:?}",
            result
        );
    }

    #[test]
    fn test_unstake_id_mismatch() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        validator_store
            .stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller)
            .unwrap();

        let result = validator_store.unstake(99, caller, 100);
        assert!(
            matches!(result, Err(ValidatorError::IdMismatch)),
            "expected IdMismatch, got {:?}",
            result
        );
    }

    // ── Claim tests ────────────────────────────────────────────────────

    #[test]
    fn test_claim_unbonded_success() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        validator_store
            .stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller)
            .unwrap();
        validator_store.unstake(1, caller, 100).unwrap();

        let result = validator_store.claim_unbonded(
            &mut asset_store,
            1,
            caller,
            100 + UNBONDING_PERIOD_BLOCKS,
        );
        assert!(result.is_ok(), "claim failed: {:?}", result.err());

        // State cleared
        assert_eq!(validator_store.read_stake(caller), 0);
        assert_eq!(validator_store.read_status(caller), 0);
        assert_eq!(validator_store.read_validator_id(caller), 0);
        assert_eq!(validator_store.read_unbonding_count(), 0);
        // Balance restored
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, caller), 10_000_000);
    }

    #[test]
    fn test_claim_unbonded_too_early() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        validator_store
            .stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller)
            .unwrap();
        validator_store.unstake(1, caller, 100).unwrap();

        let result = validator_store.claim_unbonded(
            &mut asset_store,
            1,
            caller,
            100 + UNBONDING_PERIOD_BLOCKS - 1,
        );
        assert!(
            matches!(result, Err(ValidatorError::UnbondingPeriodNotElapsed)),
            "expected UnbondingPeriodNotElapsed, got {:?}",
            result
        );
    }

    #[test]
    fn test_claim_unbonded_not_unbonding() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        validator_store
            .stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller)
            .unwrap();

        let result = validator_store.claim_unbonded(
            &mut asset_store,
            1,
            caller,
            100 + UNBONDING_PERIOD_BLOCKS,
        );
        assert!(
            matches!(result, Err(ValidatorError::NotUnbonding)),
            "expected NotUnbonding, got {:?}",
            result
        );
    }

    // ── Slash tests ────────────────────────────────────────────────────

    #[test]
    fn test_slash_stake_success() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);
        seed_balance(&mut provider, caller, 10_000_000);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        validator_store
            .stake(&mut asset_store, [0xAAu8; 32], 5_000_000, caller)
            .unwrap();

        let result = validator_store.slash_stake(&mut asset_store, caller);
        assert_eq!(result.unwrap(), 5_000_000);

        // State cleared
        assert_eq!(validator_store.read_stake(caller), 0);
        assert_eq!(validator_store.read_status(caller), 0);
        assert_eq!(validator_store.read_validator_id(caller), 0);
    }

    #[test]
    fn test_slash_non_validator() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = test_addr(0x11);

        let backend = JournalBackend::new(&mut provider);
        let mut asset_store = AssetStorage::new(backend);
        let mut validator_store = ValidatorStorage::new(backend);

        let result = validator_store.slash_stake(&mut asset_store, caller);
        assert_eq!(result.unwrap(), 0);
    }

    // ── Read tests ─────────────────────────────────────────────────────

    #[test]
    fn test_read_validator_by_index_out_of_range() {
        let mut provider = HashMapStorageProvider::new(1_000_000);

        let backend = JournalBackend::new(&mut provider);
        let validator_store = ValidatorStorage::new(backend);

        assert_eq!(validator_store.read_validator_by_index(1), Address::ZERO);
    }
}
