pub mod precompile;

pub use precompile::AgentPrecompile;

use call_asset::AssetStorage;
use call_precompile::{
    address_to_u256, storage::storage_slot, u128_to_u256, u256_to_address, u256_to_u128,
    u256_to_u64, u64_to_u256, write_string32, AGENT_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;

/// Error type for agent operations.
#[derive(Debug)]
pub enum AgentError {
    NotFound,
    NotOwner,
    PermissionsExpired,
    AssetNotAllowed,
    AmountExceedsLimit,
    InsufficientBalance,
    BalanceOverflow,
    ArrayLengthMismatch,
    EmptyBatch,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::NotFound => write!(f, "agent: not found"),
            AgentError::NotOwner => write!(f, "agent: sender is not owner"),
            AgentError::PermissionsExpired => write!(f, "agent: permissions expired"),
            AgentError::AssetNotAllowed => write!(f, "agent: asset not allowed"),
            AgentError::AmountExceedsLimit => write!(f, "agent: amount exceeds per-tx limit"),
            AgentError::InsufficientBalance => write!(f, "agent: insufficient balance"),
            AgentError::BalanceOverflow => write!(f, "agent: balance overflow"),
            AgentError::ArrayLengthMismatch => write!(f, "array length mismatch"),
            AgentError::EmptyBatch => write!(f, "empty batch"),
        }
    }
}

impl std::error::Error for AgentError {}

/// Asset ID for the native CALL token.
pub const CALL_ASSET_ID: u64 = 1;

// ── Storage slot helpers ──────────────────────────────────────────────

pub fn slot_agent_count() -> U256 {
    U256::ZERO
}

pub fn slot_agent_owner(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"owner"])
}

pub fn slot_agent_pubkey(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"pubkey"])
}

pub fn slot_agent_name(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"name"])
}

pub fn slot_agent_url(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"url"])
}

pub fn slot_agent_perms(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"perms"])
}

pub fn slot_agent_registered_at(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"block"])
}

pub fn slot_agent_balance(agent_id: u64, asset_id: u64) -> U256 {
    storage_slot(&[
        b"abalance",
        &agent_id.to_be_bytes()[..],
        &asset_id.to_be_bytes()[..],
    ])
}

// ── Permission packing ────────────────────────────────────────────────

/// Pack agent permissions into a single U256:
/// bytes 0..16  = per_tx_limit (u128)
/// bytes 16..24 = expires_at (u64)
/// byte 31      = flags (bit 0 = allow asset 1, bit 1 = allow all protocols)
pub fn pack_agent_perms(per_tx_limit: u128, expires_at: u64, flags: u8) -> U256 {
    let mut packed = [0u8; 32];
    packed[0..16].copy_from_slice(&per_tx_limit.to_be_bytes());
    packed[16..24].copy_from_slice(&expires_at.to_be_bytes());
    packed[31] = flags;
    U256::from_be_slice(&packed)
}

#[allow(clippy::unwrap_used)]
pub fn unpack_agent_perms(perms: U256) -> (u128, u64, u8) {
    let bytes = perms.to_be_bytes::<32>();
    let per_tx_limit = u128::from_be_bytes(bytes[0..16].try_into().expect("invariant: 16-byte slice"));
    let expires_at = u64::from_be_bytes(bytes[16..24].try_into().expect("invariant: 8-byte slice"));
    let flags = bytes[31];
    (per_tx_limit, expires_at, flags)
}

// ── AgentStorage ──────────────────────────────────────────────────────

/// Business logic for agent operations backed by any StorageBackend.
pub struct AgentStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> AgentStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    // ── Read operations ───────────────────────────────────────────────

    pub fn read_count(&mut self) -> u64 {
        u256_to_u64(self.backend.load(AGENT_ADDRESS, slot_agent_count()))
    }

    pub fn read_owner(&mut self, agent_id: u64) -> Address {
        u256_to_address(self.backend.load(AGENT_ADDRESS, slot_agent_owner(agent_id)))
    }

    pub fn read_pubkey(&mut self, agent_id: u64) -> [u8; 32] {
        self.backend
            .load(AGENT_ADDRESS, slot_agent_pubkey(agent_id))
            .to_be_bytes::<32>()
    }

    pub fn read_name(&mut self, agent_id: u64) -> [u8; 32] {
        self.backend
            .load(AGENT_ADDRESS, slot_agent_name(agent_id))
            .to_be_bytes::<32>()
    }

    pub fn read_url(&mut self, agent_id: u64) -> [u8; 32] {
        self.backend
            .load(AGENT_ADDRESS, slot_agent_url(agent_id))
            .to_be_bytes::<32>()
    }

    pub fn read_perms(&mut self, agent_id: u64) -> U256 {
        self.backend.load(AGENT_ADDRESS, slot_agent_perms(agent_id))
    }

    pub fn read_registered_at(&mut self, agent_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_agent_registered_at(agent_id)),
        )
    }

    pub fn read_agent_balance(&mut self, agent_id: u64, asset_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id)),
        )
    }

    pub fn agent_exists(&mut self, agent_id: u64) -> bool {
        self.read_owner(agent_id) != Address::ZERO
    }

    // ── Permission checks ─────────────────────────────────────────────

    pub fn check_owner(&mut self, agent_id: u64, caller: Address) -> Result<(), AgentError> {
        if self.read_owner(agent_id) != caller {
            return Err(AgentError::NotOwner);
        }
        Ok(())
    }

    pub fn require_perms(
        &mut self,
        agent_id: u64,
        asset_id: u64,
        current_block: u64,
    ) -> Result<(u128, u64, u8), AgentError> {
        let perms = self.read_perms(agent_id);
        let (per_tx_limit, expires_at, flags) = unpack_agent_perms(perms);
        if expires_at != 0 && current_block > expires_at {
            return Err(AgentError::PermissionsExpired);
        }
        if asset_id != CALL_ASSET_ID && (flags & 1) == 0 {
            return Err(AgentError::AssetNotAllowed);
        }
        Ok((per_tx_limit, expires_at, flags))
    }

    // ── Write operations ──────────────────────────────────────────────

    pub fn register_agent(
        &mut self,
        name: &str,
        url: &str,
        pubkey_hash: [u8; 32],
        caller: Address,
        current_block: u64,
    ) -> Result<(), AgentError> {
        let count = self.read_count();
        let agent_id = count;
        self.backend
            .store(AGENT_ADDRESS, slot_agent_count(), u64_to_u256(count + 1));
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_owner(agent_id),
            address_to_u256(caller),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_pubkey(agent_id),
            U256::from_be_slice(&pubkey_hash),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_name(agent_id),
            write_string32(name),
        );
        self.backend
            .store(AGENT_ADDRESS, slot_agent_url(agent_id), write_string32(url));
        // Default perms: per_tx_limit=1_000, expires_at=0, flags=1 (allow asset 1)
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_perms(agent_id),
            pack_agent_perms(1_000, 0, 1),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_registered_at(agent_id),
            u64_to_u256(current_block),
        );
        Ok(())
    }

    pub fn grant_balance(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        agent_id: u64,
        asset_id: u64,
        amount: u128,
        caller: Address,
    ) -> Result<(), AgentError> {
        if !self.agent_exists(agent_id) {
            return Err(AgentError::NotFound);
        }
        self.check_owner(agent_id, caller)?;

        asset_store
            .deduct_balance(asset_id, caller, amount)
            .map_err(|_| AgentError::InsufficientBalance)?;

        let agent_bal = self
            .read_agent_balance(agent_id, asset_id)
            .checked_add(amount)
            .ok_or(AgentError::BalanceOverflow)?;
        self.write_agent_balance(agent_id, asset_id, agent_bal);
        Ok(())
    }

    pub fn revoke_balance(
        &mut self,
        agent_id: u64,
        asset_id: u64,
        caller: Address,
    ) -> Result<(), AgentError> {
        if !self.agent_exists(agent_id) {
            return Err(AgentError::NotFound);
        }
        self.check_owner(agent_id, caller)?;

        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_balance(agent_id, asset_id),
            U256::ZERO,
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn pay(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        agent_id: u64,
        asset_id: u64,
        to: Address,
        amount: u128,
        caller: Address,
        current_block: u64,
    ) -> Result<(), AgentError> {
        if !self.agent_exists(agent_id) {
            return Err(AgentError::NotFound);
        }
        self.check_owner(agent_id, caller)?;

        let (per_tx_limit, _, _) = self.require_perms(agent_id, asset_id, current_block)?;
        if amount > per_tx_limit {
            return Err(AgentError::AmountExceedsLimit);
        }

        let agent_bal = self
            .read_agent_balance(agent_id, asset_id)
            .checked_sub(amount)
            .ok_or(AgentError::InsufficientBalance)?;
        self.write_agent_balance(agent_id, asset_id, agent_bal);

        asset_store
            .add_balance(asset_id, to, amount)
            .map_err(|_| AgentError::BalanceOverflow)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn batch_pay(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        agent_id: u64,
        asset_id: u64,
        recipients: &[Address],
        amounts: &[u128],
        caller: Address,
        current_block: u64,
    ) -> Result<(), AgentError> {
        if recipients.len() != amounts.len() {
            return Err(AgentError::ArrayLengthMismatch);
        }
        if recipients.is_empty() {
            return Err(AgentError::EmptyBatch);
        }

        if !self.agent_exists(agent_id) {
            return Err(AgentError::NotFound);
        }
        self.check_owner(agent_id, caller)?;

        let (per_tx_limit, _, _) = self.require_perms(agent_id, asset_id, current_block)?;

        let total_amount: u128 = amounts.iter().copied().sum();
        for amount in amounts.iter() {
            if *amount > per_tx_limit {
                return Err(AgentError::AmountExceedsLimit);
            }
        }

        let agent_bal = self
            .read_agent_balance(agent_id, asset_id)
            .checked_sub(total_amount)
            .ok_or(AgentError::InsufficientBalance)?;
        self.write_agent_balance(agent_id, asset_id, agent_bal);

        for (recipient, amount) in recipients.iter().zip(amounts.iter()) {
            asset_store
                .add_balance(asset_id, *recipient, *amount)
                .map_err(|_| AgentError::BalanceOverflow)?;
        }
        Ok(())
    }

    pub fn withdraw_balance(
        &mut self,
        agent_id: u64,
        asset_id: u64,
        amount: u128,
        caller: Address,
        current_block: u64,
    ) -> Result<(), AgentError> {
        if !self.agent_exists(agent_id) {
            return Err(AgentError::NotFound);
        }
        self.check_owner(agent_id, caller)?;

        self.require_perms(agent_id, asset_id, current_block)?;

        let agent_bal = self
            .read_agent_balance(agent_id, asset_id)
            .checked_sub(amount)
            .ok_or(AgentError::InsufficientBalance)?;
        self.write_agent_balance(agent_id, asset_id, agent_bal);
        Ok(())
    }

    pub fn revoke_agent(&mut self, agent_id: u64, caller: Address) -> Result<(), AgentError> {
        if !self.agent_exists(agent_id) {
            return Err(AgentError::NotFound);
        }
        self.check_owner(agent_id, caller)?;

        self.backend
            .store(AGENT_ADDRESS, slot_agent_owner(agent_id), U256::ZERO);
        self.backend
            .store(AGENT_ADDRESS, slot_agent_pubkey(agent_id), U256::ZERO);
        self.backend
            .store(AGENT_ADDRESS, slot_agent_name(agent_id), U256::ZERO);
        self.backend
            .store(AGENT_ADDRESS, slot_agent_url(agent_id), U256::ZERO);
        self.backend
            .store(AGENT_ADDRESS, slot_agent_perms(agent_id), U256::ZERO);
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_registered_at(agent_id),
            U256::ZERO,
        );
        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────

    fn write_agent_balance(&mut self, agent_id: u64, asset_id: u64, amount: u128) {
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_balance(agent_id, asset_id),
            u128_to_u256(amount),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_asset::AssetStorage;
    use call_primitives::Address;
    use call_protocol::storage_backend::StorageBackend;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    #[derive(Clone)]
    struct TestBackend {
        storage: Rc<RefCell<HashMap<(Address, U256), U256>>>,
    }

    impl TestBackend {
        fn new() -> Self {
            Self {
                storage: Rc::new(RefCell::new(HashMap::new())),
            }
        }
    }

    impl StorageBackend for TestBackend {
        fn load(&mut self, address: Address, slot: U256) -> U256 {
            self.storage
                .borrow()
                .get(&(address, slot))
                .copied()
                .unwrap_or_default()
        }
        fn store(&mut self, address: Address, slot: U256, value: U256) {
            self.storage.borrow_mut().insert((address, slot), value);
        }
    }

    #[test]
    fn test_register_and_read_agent() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("TestAgent", "http://test.com", [0xBBu8; 32], caller, 100)
            .unwrap();

        assert_eq!(store.read_count(), 1);
        assert_eq!(store.read_owner(0), caller);
        assert_eq!(&store.read_name(0)[0..9], b"TestAgent");
        assert_eq!(&store.read_url(0)[0..15], b"http://test.com");
        assert_eq!(store.read_pubkey(0), [0xBBu8; 32]);
        assert_eq!(store.read_registered_at(0), 100);
        assert!(store.agent_exists(0));
    }

    #[test]
    fn test_check_owner() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("A", "url", [0u8; 32], caller, 1)
            .unwrap();

        assert!(store.check_owner(0, caller).is_ok());
        assert!(matches!(
            store.check_owner(0, Address::repeat_byte(0x99)),
            Err(AgentError::NotOwner)
        ));
    }

    #[test]
    fn test_require_perms_expired() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("A", "url", [0u8; 32], caller, 1)
            .unwrap();
        // Override perms: expires_at=50
        store.backend.store(
            AGENT_ADDRESS,
            slot_agent_perms(0),
            pack_agent_perms(1000, 50, 1),
        );

        assert!(matches!(
            store.require_perms(0, CALL_ASSET_ID, 100),
            Err(AgentError::PermissionsExpired)
        ));
    }

    #[test]
    fn test_require_perms_asset_not_allowed() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("A", "url", [0u8; 32], caller, 1)
            .unwrap();
        // Override perms: flags=0 (bit 0 clear = asset 1 not allowed)
        store.backend.store(
            AGENT_ADDRESS,
            slot_agent_perms(0),
            pack_agent_perms(1000, 0, 0),
        );
        assert!(matches!(
            store.require_perms(0, 2, 1),
            Err(AgentError::AssetNotAllowed)
        ));
    }

    #[test]
    fn test_grant_pay_withdraw_revoke_balance() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);
        let recipient = Address::repeat_byte(0x33);

        agent_store
            .register_agent("A", "url", [0u8; 32], caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);

        // Grant
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, CALL_ASSET_ID), 5_000);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, caller), 5_000);

        // Pay
        agent_store
            .pay(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                recipient,
                1_000,
                caller,
                1,
            )
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, CALL_ASSET_ID), 4_000);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, recipient), 1_000);

        // Withdraw
        agent_store
            .withdraw_balance(0, CALL_ASSET_ID, 500, caller, 1)
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, CALL_ASSET_ID), 3_500);

        // Revoke balance
        agent_store
            .revoke_balance(0, CALL_ASSET_ID, caller)
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, CALL_ASSET_ID), 0);
    }

    #[test]
    fn test_batch_pay() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);
        let r1 = Address::repeat_byte(0x33);
        let r2 = Address::repeat_byte(0x44);

        agent_store
            .register_agent("A", "url", [0u8; 32], caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();

        agent_store
            .batch_pay(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                &[r1, r2],
                &[500, 800],
                caller,
                1,
            )
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, CALL_ASSET_ID), 3_700);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, r1), 500);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, r2), 800);
    }

    #[test]
    fn test_batch_pay_array_mismatch() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        agent_store
            .register_agent("A", "url", [0u8; 32], caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();

        assert!(matches!(
            agent_store.batch_pay(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                &[Address::repeat_byte(0x33)],
                &[1_000, 2_000],
                caller,
                1,
            ),
            Err(AgentError::ArrayLengthMismatch)
        ));
    }

    #[test]
    fn test_batch_pay_empty() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        agent_store
            .register_agent("A", "url", [0u8; 32], caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();

        assert!(matches!(
            agent_store.batch_pay(&mut asset_store, 0, CALL_ASSET_ID, &[], &[], caller, 1,),
            Err(AgentError::EmptyBatch)
        ));
    }

    #[test]
    fn test_pay_amount_exceeds_limit() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        agent_store
            .register_agent("A", "url", [0u8; 32], caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();

        // Default per_tx_limit is 1_000
        assert!(matches!(
            agent_store.pay(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                Address::repeat_byte(0x33),
                2_000,
                caller,
                1,
            ),
            Err(AgentError::AmountExceedsLimit)
        ));
    }

    #[test]
    fn test_revoke_agent() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("A", "url", [0u8; 32], caller, 1)
            .unwrap();
        assert!(store.agent_exists(0));

        store.revoke_agent(0, caller).unwrap();
        assert!(!store.agent_exists(0));
        assert_eq!(store.read_owner(0), Address::ZERO);
    }

    #[test]
    fn test_not_found_errors() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        assert!(matches!(
            agent_store.grant_balance(&mut asset_store, 0, 1, 100, caller),
            Err(AgentError::NotFound)
        ));
        assert!(matches!(
            agent_store.revoke_balance(0, 1, caller),
            Err(AgentError::NotFound)
        ));
        assert!(matches!(
            agent_store.pay(&mut asset_store, 0, 1, Address::ZERO, 100, caller, 1,),
            Err(AgentError::NotFound)
        ));
        assert!(matches!(
            agent_store.withdraw_balance(0, 1, 100, caller, 1),
            Err(AgentError::NotFound)
        ));
        assert!(matches!(
            agent_store.revoke_agent(0, caller),
            Err(AgentError::NotFound)
        ));
    }

    #[test]
    fn test_pack_unpack_perms() {
        let per_tx_limit = 12345u128;
        let expires_at = 67890u64;
        let flags = 0b101u8;

        let packed = pack_agent_perms(per_tx_limit, expires_at, flags);
        let (limit, expiry, f) = unpack_agent_perms(packed);

        assert_eq!(limit, per_tx_limit);
        assert_eq!(expiry, expires_at);
        assert_eq!(f, flags);
    }
}
