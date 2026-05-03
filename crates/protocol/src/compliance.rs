//! T1.3 — Compliance Types (per spec §3.4)
//!
//! Policy types and status enums used by the compliance precompile.
//! The in-memory ComplianceEngine has been removed — all compliance state
//! lives in EVM storage and is enforced by the protocol execution layer.

use call_primitives::Address;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ComplianceStatus {
    #[default]
    Clear,
    UnderReview,
    Flagged,
    Restricted,
}

/// Trait for custom compliance handlers
pub trait CustomComplianceHandler {
    fn check(&self, address: &Address) -> bool;
}
