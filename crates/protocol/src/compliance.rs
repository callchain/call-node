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

/// Compliance engine state
#[derive(Default)]
pub struct ComplianceEngine {
    sanctioned: HashSet<Address>,
    kyc_verified: HashSet<Address>,
    whitelisted: HashSet<Address>,
    custom_handlers: HashMap<u8, Box<dyn CustomComplianceHandler + Send + Sync>>,
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
                // Custom handler always passes; actual logic via callback
                Ok(())
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
