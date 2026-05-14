pub mod precompile;

pub use precompile::CompliancePrecompile;

use call_precompile::{
    slot_compliance, u8_to_u256, COMPLIANCE_ADDRESS, GOVERNANCE_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;

/// Error type for compliance operations.
#[derive(Debug)]
pub enum ComplianceError {
    NotGovernance,
    InvalidStatus,
}

impl std::fmt::Display for ComplianceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComplianceError::NotGovernance => write!(f, "not governance"),
            ComplianceError::InvalidStatus => write!(f, "invalid compliance status value"),
        }
    }
}

impl std::error::Error for ComplianceError {}

// ── Storage slot helpers ──────────────────────────────────────────────

/// Compute the EVM storage slot for the compliance admin address.
pub fn slot_compliance_admin() -> U256 {
    call_precompile::storage::storage_slot(&[b"admin"])
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

    pub fn read_status(&mut self, addr: Address) -> u8 {
        self.backend
            .load(COMPLIANCE_ADDRESS, slot_compliance(addr))
            .to_be_bytes::<32>()[31]
    }

    pub fn read_admin(&mut self) -> Address {
        let v = self.backend.load(COMPLIANCE_ADDRESS, slot_compliance_admin());
        Address::from_slice(&v.to_be_bytes::<32>()[12..32])
    }

    pub fn check_compliance(&mut self, target: Address) -> bool {
        let status = self.read_status(target);
        // Clear (0) = pass, anything else = fail
        status == 0
    }

    // ── Write operations ──────────────────────────────────────────────

    pub fn update_compliance(
        &mut self,
        target: Address,
        status_u8: u8,
        caller: Address,
    ) -> Result<(), ComplianceError> {
        if caller != GOVERNANCE_ADDRESS {
            return Err(ComplianceError::NotGovernance);
        }

        let slot = slot_compliance(target);
        self.backend
            .store(COMPLIANCE_ADDRESS, slot, u8_to_u256(status_u8));
        Ok(())
    }

    pub fn set_admin(
        &mut self,
        new_admin: Address,
        caller: Address,
    ) -> Result<(), ComplianceError> {
        let current_admin = self.read_admin();
        // If admin is not set (zero address), allow GOVERNANCE_ADDRESS to set it
        let expected = if current_admin == Address::ZERO {
            GOVERNANCE_ADDRESS
        } else {
            current_admin
        };
        if caller != expected {
            return Err(ComplianceError::NotGovernance);
        }

        let mut bytes = [0u8; 32];
        bytes[12..32].copy_from_slice(new_admin.as_slice());
        self.backend.store(
            COMPLIANCE_ADDRESS,
            slot_compliance_admin(),
            U256::from_be_bytes::<32>(bytes),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn address_to_u256_word(addr: Address) -> U256 {
        let mut bytes = [0u8; 32];
        bytes[12..32].copy_from_slice(addr.as_slice());
        U256::from_be_bytes::<32>(bytes)
    }

    #[test]
    fn test_check_compliance_default_clear() {
        let backend = TestBackend::new();
        let mut store = ComplianceStorage::new(backend);
        // No status set => defaults to 0 (clear)
        assert!(store.check_compliance(Address::repeat_byte(0x22)));
    }

    #[test]
    fn test_check_compliance_restricted() {
        let mut backend = TestBackend::new();
        let target = Address::repeat_byte(0x22);

        backend.store(
            COMPLIANCE_ADDRESS,
            slot_compliance(target),
            u8_to_u256(3),
        );

        let mut store = ComplianceStorage::new(backend);
        assert!(!store.check_compliance(target));
    }

    #[test]
    fn test_update_compliance_by_governance() {
        let mut backend = TestBackend::new();
        let target = Address::repeat_byte(0x22);

        let mut store = ComplianceStorage::new(backend);
        store.update_compliance(target, 1, GOVERNANCE_ADDRESS).unwrap();
        assert!(!store.check_compliance(target));
    }

    #[test]
    fn test_update_compliance_not_governance_fails() {
        let backend = TestBackend::new();
        let target = Address::repeat_byte(0x22);

        let mut store = ComplianceStorage::new(backend);
        let result = store.update_compliance(target, 1, Address::repeat_byte(0x99));
        assert!(matches!(result, Err(ComplianceError::NotGovernance)));
    }

    #[test]
    fn test_update_compliance_then_clear() {
        let mut backend = TestBackend::new();
        let target = Address::repeat_byte(0x22);

        let mut store = ComplianceStorage::new(backend.clone());
        store.update_compliance(target, 3, GOVERNANCE_ADDRESS).unwrap();
        assert!(!store.check_compliance(target));

        let mut store = ComplianceStorage::new(backend);
        store.update_compliance(target, 0, GOVERNANCE_ADDRESS).unwrap();
        assert!(store.check_compliance(target));
    }

    #[test]
    fn test_set_admin_initial_by_governance() {
        let backend = TestBackend::new();
        let new_admin = Address::repeat_byte(0x44);

        let mut store = ComplianceStorage::new(backend);
        store.set_admin(new_admin, GOVERNANCE_ADDRESS).unwrap();
        assert_eq!(store.read_admin(), new_admin);
    }

    #[test]
    fn test_set_admin_by_current_admin() {
        let mut backend = TestBackend::new();
        let admin = Address::repeat_byte(0x44);
        let new_admin = Address::repeat_byte(0x55);

        backend.store(
            COMPLIANCE_ADDRESS,
            slot_compliance_admin(),
            address_to_u256_word(admin),
        );

        let mut store = ComplianceStorage::new(backend);
        store.set_admin(new_admin, admin).unwrap();
        assert_eq!(store.read_admin(), new_admin);
    }

    #[test]
    fn test_set_admin_unauthorized_fails() {
        let mut backend = TestBackend::new();
        let admin = Address::repeat_byte(0x44);

        backend.store(
            COMPLIANCE_ADDRESS,
            slot_compliance_admin(),
            address_to_u256_word(admin),
        );

        let mut store = ComplianceStorage::new(backend);
        let result = store.set_admin(Address::repeat_byte(0x55), Address::repeat_byte(0x99));
        assert!(matches!(result, Err(ComplianceError::NotGovernance)));
    }

    #[test]
    fn test_read_status_default_zero() {
        let backend = TestBackend::new();
        let target = Address::repeat_byte(0x22);

        let mut store = ComplianceStorage::new(backend);
        // Unset status defaults to 0 (clear/compliant)
        assert_eq!(store.read_status(target), 0);
    }

    #[test]
    fn test_various_status_values() {
        let mut backend = TestBackend::new();
        let target = Address::repeat_byte(0x22);

        // Status 0 = clear (compliant)
        backend.store(
            COMPLIANCE_ADDRESS,
            slot_compliance(target),
            u8_to_u256(0),
        );
        let mut store = ComplianceStorage::new(backend.clone());
        assert!(store.check_compliance(target));

        // Status 1 = fail
        backend.store(
            COMPLIANCE_ADDRESS,
            slot_compliance(target),
            u8_to_u256(1),
        );
        let mut store = ComplianceStorage::new(backend.clone());
        assert!(!store.check_compliance(target));

        // Status 255 = fail (any non-zero)
        backend.store(
            COMPLIANCE_ADDRESS,
            slot_compliance(target),
            u8_to_u256(255),
        );
        let mut store = ComplianceStorage::new(backend);
        assert!(!store.check_compliance(target));
    }
}
