//! T1.3 — Compliance Engine (per spec §3.4)
//!
//! Policy enforcement, blacklist, KYC, whitelist, custom handlers.

use call_primitives::Address;
use crate::{ProtocolError, ProtocolResult};
use std::collections::{HashMap, HashSet};

/// Compliance policy types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompliancePolicy {
    /// No compliance checks required
    None,
    /// OFAC-sanctioned address blacklist
    OfacBlacklist,
    /// KYC verification required
    KycRequired,
    /// Whitelist-only access control
    Whitelist,
    /// Custom policy via precompile callback
    Custom,
}

/// Compliance status for a specific address and asset
pub use crate::instructions::ComplianceStatus;

/// Per-address compliance state keyed by (address, policy_id)
#[derive(Debug, Default, Clone, Copy)]
struct AddressComplianceState {
    status: ComplianceStatus,
}

/// Compliance engine state
#[derive(Default)]
pub struct ComplianceEngine {
    sanctioned: HashSet<Address>,
    kyc_verified: HashSet<Address>,
    whitelisted: HashSet<Address>,
    custom_handlers: HashMap<u8, Box<dyn CustomComplianceHandler + Send + Sync>>,
    /// Per-address compliance status: (address, policy_id) → state
    address_states: HashMap<(Address, u8), AddressComplianceState>,
}

impl Clone for ComplianceEngine {
    fn clone(&self) -> Self {
        Self {
            sanctioned: self.sanctioned.clone(),
            kyc_verified: self.kyc_verified.clone(),
            whitelisted: self.whitelisted.clone(),
            // Custom handlers are registration-time only; they don't change
            // during transaction execution, so we can share them via pointer.
            // For simplicity we clear them in the clone — handlers are re-registered
            // on engine reconstruction. This is a temporary measure until
            // ComplianceEngine persistence is implemented (Gap 7).
            custom_handlers: HashMap::new(),
            address_states: self.address_states.clone(),
        }
    }
}

/// Trait for custom compliance handlers
pub trait CustomComplianceHandler {
    fn check(&self, address: &Address) -> bool;
}

impl ComplianceEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Main compliance check dispatcher
    pub fn check_compliance(
        &self,
        address: &Address,
        policy: CompliancePolicy,
    ) -> ProtocolResult<()> {
        match policy {
            CompliancePolicy::None => Ok(()),
            CompliancePolicy::OfacBlacklist => {
                if self.is_sanctioned(address) {
                    Err(ProtocolError::Compliance(
                        "address is sanctioned".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            CompliancePolicy::KycRequired => {
                if self.is_kyc_verified(address) {
                    Ok(())
                } else {
                    Err(ProtocolError::Compliance(
                        "KYC not verified".into(),
                    ))
                }
            }
            CompliancePolicy::Whitelist => {
                if self.is_whitelisted(address) {
                    Ok(())
                } else {
                    Err(ProtocolError::Compliance(
                        "address not whitelisted".into(),
                    ))
                }
            }
            CompliancePolicy::Custom => {
                // Gap 8 — Invoke registered custom handler if one exists
                if self.custom_handlers.is_empty() {
                    // No handlers registered: default to pass (backward compat)
                    Ok(())
                } else {
                    // Use policy_id to look up the handler; policy_id 4 = Custom
                    // The handler id is derived from the asset's compliance_policy field
                    // which maps to the handler id in the registry.
                    // For simplicity, we check all registered handlers;
                    // an address passes only if ALL registered custom handlers approve.
                    for (id, handler) in &self.custom_handlers {
                        if !handler.check(address) {
                            return Err(ProtocolError::Compliance(
                                format!("custom compliance handler {id} rejected address",)
                            ));
                        }
                    }
                    Ok(())
                }
            }
        }
    }

    // ── Blacklist ──

    pub fn is_sanctioned(&self, address: &Address) -> bool {
        self.sanctioned.contains(address)
    }

    pub fn add_to_blacklist(&mut self, address: Address) {
        self.sanctioned.insert(address);
    }

    pub fn remove_from_blacklist(&mut self, address: &Address) {
        self.sanctioned.remove(address);
    }

    // ── KYC ──

    pub fn is_kyc_verified(&self, address: &Address) -> bool {
        self.kyc_verified.contains(address)
    }

    pub fn set_kyc_status(&mut self, address: Address, verified: bool) {
        if verified {
            self.kyc_verified.insert(address);
        } else {
            self.kyc_verified.remove(&address);
        }
    }

    // ── Whitelist ──

    pub fn is_whitelisted(&self, address: &Address) -> bool {
        self.whitelisted.contains(address)
    }

    pub fn add_to_whitelist(&mut self, address: Address) {
        self.whitelisted.insert(address);
    }

    pub fn remove_from_whitelist(&mut self, address: &Address) {
        self.whitelisted.remove(address);
    }

    // ── Custom handlers ──

    pub fn register_custom_handler(
        &mut self,
        id: u8,
        handler: Box<dyn CustomComplianceHandler + Send + Sync>,
    ) {
        self.custom_handlers.insert(id, handler);
    }

    pub fn check_custom(&self, id: u8, address: &Address) -> bool {
        self.custom_handlers
            .get(&id)
            .map(|h| h.check(address))
            .unwrap_or(false)
    }

    // ── Per-address compliance status ──

    /// Set compliance status for an address under a specific policy
    pub fn set_address_compliance(
        &mut self,
        address: Address,
        policy_id: u8,
        status: ComplianceStatus,
    ) -> ProtocolResult<()> {
        self.address_states.insert(
            (address, policy_id),
            AddressComplianceState { status },
        );
        Ok(())
    }

    /// Get compliance status for an address under a specific policy
    pub fn get_address_compliance(
        &self,
        address: &Address,
        policy_id: u8,
    ) -> ComplianceStatus {
        self.address_states
            .get(&(*address, policy_id))
            .map(|s| s.status)
            .unwrap_or(ComplianceStatus::Clear)
    }

    /// Check compliance using raw policy ID (u8), used by instruction execution
    pub fn check_compliance_by_policy_id(
        &self,
        address: &Address,
        policy_id: u8,
    ) -> ProtocolResult<()> {
        // Convert u8 to CompliancePolicy enum
        let policy = match policy_id {
            0 => CompliancePolicy::None,
            1 => CompliancePolicy::OfacBlacklist,
            2 => CompliancePolicy::KycRequired,
            3 => CompliancePolicy::Whitelist,
            4 => CompliancePolicy::Custom,
            _ => CompliancePolicy::None,
        };
        // Check if address is restricted
        let state = self.get_address_compliance(address, policy_id);
        if state == ComplianceStatus::Restricted {
            return Err(ProtocolError::Compliance(
                "address compliance status is restricted".into(),
            ));
        }
        self.check_compliance(address, policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_compliance_none() {
        let engine = ComplianceEngine::new();
        assert!(engine.check_compliance(&test_addr(1), CompliancePolicy::None).is_ok());
    }

    #[test]
    fn test_compliance_ofac_blacklist_blocked() {
        let mut engine = ComplianceEngine::new();
        engine.add_to_blacklist(test_addr(1));
        assert!(engine
            .check_compliance(&test_addr(1), CompliancePolicy::OfacBlacklist)
            .is_err());
    }

    #[test]
    fn test_compliance_ofac_blacklist_allowed() {
        let engine = ComplianceEngine::new();
        assert!(engine
            .check_compliance(&test_addr(1), CompliancePolicy::OfacBlacklist)
            .is_ok());
    }

    #[test]
    fn test_compliance_kyc_required() {
        let mut engine = ComplianceEngine::new();
        // Not verified → blocked
        assert!(engine
            .check_compliance(&test_addr(1), CompliancePolicy::KycRequired)
            .is_err());
        // Verified → allowed
        engine.set_kyc_status(test_addr(1), true);
        assert!(engine
            .check_compliance(&test_addr(1), CompliancePolicy::KycRequired)
            .is_ok());
    }

    #[test]
    fn test_compliance_whitelist_blocked() {
        let engine = ComplianceEngine::new();
        assert!(engine
            .check_compliance(&test_addr(1), CompliancePolicy::Whitelist)
            .is_err());
    }

    #[test]
    fn test_compliance_custom_handler() {
        struct AlwaysPass;
        impl CustomComplianceHandler for AlwaysPass {
            fn check(&self, _address: &Address) -> bool {
                true
            }
        }

        let mut engine = ComplianceEngine::new();
        engine.register_custom_handler(1, Box::new(AlwaysPass));
        assert!(engine.check_custom(1, &test_addr(1)));
    }
}
