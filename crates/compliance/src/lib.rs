pub mod precompile;

pub use precompile::CompliancePrecompile;

use call_precompiles::{
    storage::storage_slot, u256_to_address, u8_to_u256,
    ASSET_ADDRESS, COMPLIANCE_ADDRESS,
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
