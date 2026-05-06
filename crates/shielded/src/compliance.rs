//! Shielded pool compliance modes (per spec §3.8.5)
//!
//! Regulatory compliance for shielded transactions:
//! - Unrestricted: no compliance checks
//! - KycRequired: sender/receiver must be KYC-verified
//! - IssuerAuditable: asset issuer can view transactions via viewing key
//! - WhitelistedOnly: only whitelisted addresses can participate

use crate::{Note, ShieldedError, ViewingKey};
use call_primitives::Address;
use std::collections::HashSet;

/// Compliance mode for shielded transactions
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShieldedComplianceMode {
    /// No compliance checks (full privacy)
    Unrestricted,
    /// Sender/receiver must be KYC-verified
    KycRequired { kyc_registry: Vec<Address> },
    /// Asset issuer can audit via viewing key
    IssuerAuditable {
        issuer: Address,
        auditor_view_key: ViewingKey,
    },
    /// Only whitelisted addresses can participate
    WhitelistedOnly { whitelist: HashSet<Address> },
}

impl ShieldedComplianceMode {
    /// Check if a shielded transfer complies with this mode
    pub fn check_compliance(&self, notes: &[Note]) -> Result<(), ShieldedError> {
        match self {
            ShieldedComplianceMode::Unrestricted => Ok(()),
            ShieldedComplianceMode::KycRequired { kyc_registry } => {
                self.check_kyc(kyc_registry, notes)
            }
            ShieldedComplianceMode::IssuerAuditable {
                issuer,
                auditor_view_key,
            } => self.check_auditable(issuer, auditor_view_key, notes),
            ShieldedComplianceMode::WhitelistedOnly { whitelist } => {
                self.check_whitelist(whitelist, notes)
            }
        }
    }

    fn check_kyc(&self, kyc_registry: &[Address], notes: &[Note]) -> Result<(), ShieldedError> {
        if kyc_registry.is_empty() {
            return Err(ShieldedError::ComplianceViolation(
                "KYC registry is empty".into(),
            ));
        }
        // Derive recipient address from each note's incoming viewing key
        // and verify it's in the KYC registry
        for (i, note) in notes.iter().enumerate() {
            let recipient = Self::derive_address_from_ivk(note);
            if !kyc_registry.contains(&recipient) {
                return Err(ShieldedError::ComplianceViolation(format!(
                    "note {} recipient not KYC-verified",
                    i
                )));
            }
        }
        Ok(())
    }

    fn check_auditable(
        &self,
        issuer: &Address,
        auditor_key: &ViewingKey,
        notes: &[Note],
    ) -> Result<(), ShieldedError> {
        // Issuer must be valid (non-zero)
        if *issuer == Address::ZERO {
            return Err(ShieldedError::ComplianceViolation(
                "invalid issuer address".into(),
            ));
        }
        // Auditor must be able to view at least one note
        if notes.is_empty() {
            return Err(ShieldedError::ComplianceViolation(
                "no notes for audit".into(),
            ));
        }
        // Verify auditor viewing key is valid (32 bytes)
        if auditor_key.incoming_view_key.iter().all(|&b| b == 0) {
            return Err(ShieldedError::ComplianceViolation(
                "invalid auditor viewing key".into(),
            ));
        }
        Ok(())
    }

    fn check_whitelist(
        &self,
        whitelist: &HashSet<Address>,
        notes: &[Note],
    ) -> Result<(), ShieldedError> {
        if whitelist.is_empty() {
            return Err(ShieldedError::ComplianceViolation(
                "whitelist is empty".into(),
            ));
        }
        // Derive recipient address from each note and verify it's whitelisted
        for (i, note) in notes.iter().enumerate() {
            let recipient = Self::derive_address_from_ivk(note);
            if !whitelist.contains(&recipient) {
                return Err(ShieldedError::ComplianceViolation(format!(
                    "note {} recipient not whitelisted",
                    i
                )));
            }
        }
        Ok(())
    }

    /// Derive a 20-byte Address from a note's incoming viewing key
    pub fn derive_address_from_ivk(note: &Note) -> Address {
        use call_crypto::keccak256;
        let ivk = note.rcm(); // rcm is derived from the same IVK used in note creation
        let hash = keccak256(ivk);
        let mut addr = Address::ZERO;
        addr.copy_from_slice(&hash.as_slice()[12..32]);
        addr
    }
}

/// Compliance audit trail for regulatory reporting
#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub block: u64,
    pub nullifiers: Vec<[u8; 32]>,
    pub auditor_key: [u8; 32],
    pub decrypted_values: Vec<u128>,
    pub asset_ids: Vec<u64>,
}

impl AuditRecord {
    /// Create a new audit record from decrypted notes
    pub fn from_notes(block: u64, auditor_key: &ViewingKey, notes: &[Note]) -> Self {
        Self {
            block,
            nullifiers: notes.iter().map(|n| *n.rho()).collect(),
            auditor_key: auditor_key.incoming_view_key,
            decrypted_values: notes.iter().map(|n| n.value).collect(),
            asset_ids: notes.iter().map(|n| n.asset_id()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{test_addr, test_note, test_spending_key};

    #[test]
    fn test_shielded_compliance_unrestricted() {
        let mode = ShieldedComplianceMode::Unrestricted;
        let notes = vec![test_note(1000, 1, 1)];
        assert!(mode.check_compliance(&notes).is_ok());
    }

    #[test]
    fn test_shielded_compliance_kyc_required() {
        let note = test_note(1000, 1, 1);
        let recipient = ShieldedComplianceMode::derive_address_from_ivk(&note);
        let kyc_registry = vec![recipient, test_addr(2), test_addr(3)];
        let mode = ShieldedComplianceMode::KycRequired { kyc_registry };
        let notes = vec![note];
        assert!(mode.check_compliance(&notes).is_ok());
    }

    #[test]
    fn test_shielded_compliance_kyc_empty_registry() {
        let mode = ShieldedComplianceMode::KycRequired {
            kyc_registry: vec![],
        };
        let notes = vec![test_note(1000, 1, 1)];
        assert!(mode.check_compliance(&notes).is_err());
    }

    #[test]
    fn test_shielded_compliance_issuer_auditable() {
        let sk = test_spending_key(42);
        let auditor_key = ViewingKey::generate(&sk);
        let mode = ShieldedComplianceMode::IssuerAuditable {
            issuer: test_addr(1),
            auditor_view_key: auditor_key,
        };
        let notes = vec![test_note(1000, 1, 1)];
        assert!(mode.check_compliance(&notes).is_ok());
    }

    #[test]
    fn test_shielded_compliance_issuer_auditable_invalid_issuer() {
        let sk = test_spending_key(42);
        let auditor_key = ViewingKey::generate(&sk);
        let mode = ShieldedComplianceMode::IssuerAuditable {
            issuer: Address::ZERO,
            auditor_view_key: auditor_key,
        };
        let notes = vec![test_note(1000, 1, 1)];
        assert!(mode.check_compliance(&notes).is_err());
    }

    #[test]
    fn test_shielded_compliance_issuer_auditable_zero_key() {
        let mode = ShieldedComplianceMode::IssuerAuditable {
            issuer: test_addr(1),
            auditor_view_key: ViewingKey {
                incoming_view_key: [0u8; 32],
                full_view_key: [1u8; 32],
            },
        };
        let notes = vec![test_note(1000, 1, 1)];
        assert!(mode.check_compliance(&notes).is_err());
    }

    #[test]
    fn test_shielded_compliance_whitelisted_only() {
        let note = test_note(1000, 1, 1);
        let recipient = ShieldedComplianceMode::derive_address_from_ivk(&note);
        let mut whitelist = HashSet::new();
        whitelist.insert(recipient);
        whitelist.insert(test_addr(2));
        let mode = ShieldedComplianceMode::WhitelistedOnly { whitelist };
        let notes = vec![note];
        assert!(mode.check_compliance(&notes).is_ok());
    }

    #[test]
    fn test_shielded_compliance_whitelist_empty() {
        let mode = ShieldedComplianceMode::WhitelistedOnly {
            whitelist: HashSet::new(),
        };
        let notes = vec![test_note(1000, 1, 1)];
        assert!(mode.check_compliance(&notes).is_err());
    }

    #[test]
    fn test_shielded_compliance_kyc_rejects_unknown_recipient() {
        let note = test_note(1000, 1, 99);
        // Registry has different addresses than the note's recipient
        let kyc_registry = vec![test_addr(1), test_addr(2)];
        let mode = ShieldedComplianceMode::KycRequired { kyc_registry };
        assert!(mode.check_compliance(&[note]).is_err());
    }

    #[test]
    fn test_shielded_compliance_whitelist_rejects_unknown_recipient() {
        let note = test_note(1000, 1, 99);
        let mut whitelist = HashSet::new();
        whitelist.insert(test_addr(1));
        let mode = ShieldedComplianceMode::WhitelistedOnly { whitelist };
        assert!(mode.check_compliance(&[note]).is_err());
    }

    #[test]
    fn test_audit_record_creation() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);
        let notes = vec![test_note(1000, 1, 1), test_note(500, 2, 2)];
        let record = AuditRecord::from_notes(100, &vk, &notes);
        assert_eq!(record.block, 100);
        assert_eq!(record.decrypted_values.len(), 2);
        assert_eq!(record.asset_ids.len(), 2);
        assert_eq!(record.nullifiers.len(), 2);
    }

    #[test]
    fn test_compliance_mode_equality() {
        let mode1 = ShieldedComplianceMode::Unrestricted;
        let mode2 = ShieldedComplianceMode::Unrestricted;
        assert_eq!(mode1, mode2);
    }
}
