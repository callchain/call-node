pub mod precompile;

pub use precompile::CompliancePrecompile;

use call_precompile::{
    storage::storage_slot, u256_to_address, u8_to_u256, ASSET_ADDRESS, COMPLIANCE_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;

/// Error type for compliance operations.
#[derive(Debug)]
pub enum ComplianceError {
    NotIssuer,
    InvalidStatus,
}

impl std::fmt::Display for ComplianceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComplianceError::NotIssuer => write!(f, "not asset issuer"),
            ComplianceError::InvalidStatus => write!(f, "invalid compliance status value"),
        }
    }
}

impl std::error::Error for ComplianceError {}

// ── Storage slot helpers ──────────────────────────────────────────────

pub fn slot_compliance(addr: Address, policy_id: u8) -> U256 {
    storage_slot(&[addr.as_slice(), &[policy_id]])
}

// ── ComplianceStorage ─────────────────────────────────────────────────

/// Business logic for compliance operations backed by any StorageBackend.
pub struct ComplianceStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> ComplianceStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    // ── Read operations ───────────────────────────────────────────────

    pub fn read_status(&self, addr: Address, policy_id: u8) -> u8 {
        self.backend
            .load(COMPLIANCE_ADDRESS, slot_compliance(addr, policy_id))
            .to_be_bytes::<32>()[31]
    }

    pub fn read_asset_policy_id(&self, asset_id: u64) -> u8 {
        let policy_slot = storage_slot(&[&asset_id.to_be_bytes()[..], b"compliance"]);
        self.backend
            .load(ASSET_ADDRESS, policy_slot)
            .to_be_bytes::<32>()[31]
    }

    pub fn read_asset_issuer(&self, asset_id: u64) -> Address {
        let issuer_slot = storage_slot(&[&asset_id.to_be_bytes()[..], b"issuer"]);
        u256_to_address(self.backend.load(ASSET_ADDRESS, issuer_slot))
    }

    pub fn check_compliance(&self, asset_id: u64, target: Address) -> bool {
        let policy_id = self.read_asset_policy_id(asset_id);
        if policy_id == 0 {
            return true;
        }
        let status = self.read_status(target, policy_id);
        // Clear (0) = pass, anything else = fail
        status == 0
    }

    // ── Write operations ──────────────────────────────────────────────

    pub fn update_compliance(
        &mut self,
        asset_id: u64,
        target: Address,
        status_u8: u8,
        caller: Address,
    ) -> Result<(), ComplianceError> {
        let issuer = self.read_asset_issuer(asset_id);
        if issuer != caller {
            return Err(ComplianceError::NotIssuer);
        }

        let policy_id = self.read_asset_policy_id(asset_id);
        let slot = slot_compliance(target, policy_id);
        self.backend
            .store(COMPLIANCE_ADDRESS, slot, u8_to_u256(status_u8));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::storage_slot;
    use call_precompile::u8_to_u256;
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
        fn load(&self, address: Address, slot: U256) -> U256 {
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

    fn address_to_u256_word(addr: Address) -> U256 {
        let mut bytes = [0u8; 32];
        bytes[12..32].copy_from_slice(addr.as_slice());
        U256::from_be_bytes::<32>(bytes)
    }

    #[test]
    fn test_check_compliance_no_policy() {
        let backend = TestBackend::new();
        let store = ComplianceStorage::new(backend);
        // No policy registered for asset_id=999 => always compliant
        assert!(store.check_compliance(999, Address::repeat_byte(0x22)));
    }

    #[test]
    fn test_check_compliance_clear_and_restricted() {
        let mut backend = TestBackend::new();
        let issuer = Address::repeat_byte(0x11);
        let target = Address::repeat_byte(0x22);

        // Seed asset with issuer and policy_id=1
        backend.store(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            address_to_u256_word(issuer),
        );
        backend.store(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"compliance"]),
            u8_to_u256(1),
        );

        let store = ComplianceStorage::new(backend.clone());

        // Default status = 0 (clear) => compliant
        assert!(store.check_compliance(1, target));

        // Update status to restricted (3)
        let mut store = ComplianceStorage::new(backend.clone());
        store.update_compliance(1, target, 3, issuer).unwrap();

        let store = ComplianceStorage::new(backend);
        assert!(!store.check_compliance(1, target));
    }

    #[test]
    fn test_update_compliance_not_issuer() {
        let mut backend = TestBackend::new();
        let issuer = Address::repeat_byte(0x11);

        backend.store(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"issuer"]),
            address_to_u256_word(issuer),
        );
        backend.store(
            ASSET_ADDRESS,
            storage_slot(&[&1u64.to_be_bytes()[..], b"compliance"]),
            u8_to_u256(1),
        );

        let mut store = ComplianceStorage::new(backend);
        let result =
            store.update_compliance(1, Address::repeat_byte(0x22), 3, Address::repeat_byte(0x99));
        assert!(matches!(result, Err(ComplianceError::NotIssuer)));
    }

    #[test]
    fn test_read_asset_issuer() {
        let mut backend = TestBackend::new();
        let issuer = Address::repeat_byte(0x11);

        backend.store(
            ASSET_ADDRESS,
            storage_slot(&[&42u64.to_be_bytes()[..], b"issuer"]),
            address_to_u256_word(issuer),
        );

        let store = ComplianceStorage::new(backend);
        assert_eq!(store.read_asset_issuer(42), issuer);
    }

    #[test]
    fn test_read_asset_policy_id() {
        let mut backend = TestBackend::new();

        backend.store(
            ASSET_ADDRESS,
            storage_slot(&[&42u64.to_be_bytes()[..], b"compliance"]),
            u8_to_u256(7),
        );

        let store = ComplianceStorage::new(backend);
        assert_eq!(store.read_asset_policy_id(42), 7);
    }
}
