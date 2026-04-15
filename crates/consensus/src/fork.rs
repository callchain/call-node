//! Fork Management (per spec §19)
//!
//! Height-activated protocol upgrades, governance proposal trigger with timelock,
//! version checks on block processing, and emergency rollback via 2/3 validator signatures.

use call_crypto::ed25519_verify;
use call_primitives::{Ed25519PublicKey, ProtocolVersion, ValidatorId};
use std::collections::HashMap;

// ─── Constants ─────────────────────────────────────────────────────────

/// Minimum 2/3 quorum for emergency rollback (computed as ceil(2n/3))
pub fn rollback_quorum(total_validators: u32) -> u32 {
    (2 * total_validators + 2) / 3
}
/// Timelock duration in blocks before upgrade activates
pub const DEFAULT_TIMELOCK_BLOCKS: u64 = 1000;
/// Minimum timelock blocks (safety floor)
pub const MIN_TIMELOCK_BLOCKS: u64 = 100;

// ─── Types ─────────────────────────────────────────────────────────────

/// A height-activated protocol upgrade entry
#[derive(Debug, Clone)]
pub struct UpgradeEntry {
    pub version: ProtocolVersion,
    pub activation_height: u64,
    /// Whether this upgrade has been applied
    pub applied: bool,
    /// Governance proposal ID that triggered this upgrade, if any
    pub proposal_id: Option<u64>,
    /// Timelock: block at which the upgrade was approved (None = immediate)
    pub approved_at_height: Option<u64>,
}

/// Emergency rollback state
#[derive(Debug, Clone)]
pub struct EmergencyRollback {
    /// Target height to roll back to
    pub target_height: u64,
    /// Target version to roll back to
    pub target_version: ProtocolVersion,
    /// Validator signatures collected so far
    pub signatures: HashMap<ValidatorId, [u8; 64]>,
    /// Total validator weight represented (simplified: 1 vote per validator)
    pub total_validators: u32,
}

/// Fork manager state
#[derive(Debug)]
pub struct ForkManager {
    /// Current protocol version
    current_version: ProtocolVersion,
    /// Scheduled upgrades sorted by activation height
    scheduled_upgrades: Vec<UpgradeEntry>,
    /// Active emergency rollback (if any)
    active_rollback: Option<EmergencyRollback>,
    /// Total number of registered validators
    total_validators: u32,
    /// Validator public keys for signature verification
    validator_keys: HashMap<ValidatorId, Ed25519PublicKey>,
    /// Timelock duration in blocks
    timelock_blocks: u64,
}

impl ForkManager {
    pub fn new(initial_version: ProtocolVersion, total_validators: u32) -> Self {
        Self {
            current_version: initial_version,
            scheduled_upgrades: Vec::new(),
            active_rollback: None,
            total_validators,
            validator_keys: HashMap::new(),
            timelock_blocks: DEFAULT_TIMELOCK_BLOCKS,
        }
    }

    /// Register a validator's public key (for rollback signature verification)
    pub fn register_validator(&mut self, validator_id: ValidatorId, public_key: Ed25519PublicKey) {
        self.validator_keys.insert(validator_id, public_key);
    }

    /// Schedule a height-activated upgrade
    pub fn schedule_upgrade(&mut self, entry: UpgradeEntry) {
        self.scheduled_upgrades.push(entry);
        self.scheduled_upgrades
            .sort_by_key(|e| e.activation_height);
    }

    /// Schedule an upgrade via governance proposal with timelock
    pub fn schedule_governance_upgrade(
        &mut self,
        version: ProtocolVersion,
        activation_height: u64,
        proposal_id: u64,
        current_height: u64,
    ) -> Result<(), ForkError> {
        // Activation must be at least timelock blocks after approval
        let min_activation = current_height + self.timelock_blocks;
        if activation_height < min_activation {
            return Err(ForkError::TimelockViolation(
                activation_height,
                min_activation,
            ));
        }

        self.schedule_upgrade(UpgradeEntry {
            version,
            activation_height,
            applied: false,
            proposal_id: Some(proposal_id),
            approved_at_height: Some(current_height),
        });

        Ok(())
    }

    /// Check and apply any pending upgrades at the given block height.
    /// Returns Some(new_version) if an upgrade was applied.
    pub fn check_upgrades_at_height(&mut self, height: u64) -> Option<ProtocolVersion> {
        for entry in &mut self.scheduled_upgrades {
            if !entry.applied && height >= entry.activation_height {
                entry.applied = true;
                self.current_version = entry.version;
                return Some(entry.version);
            }
        }
        None
    }

    /// Get the expected protocol version for a given height
    pub fn version_at_height(&self, height: u64) -> ProtocolVersion {
        // Find the highest scheduled upgrade at or below the given height
        let mut version = self.current_version;
        for entry in &self.scheduled_upgrades {
            if entry.activation_height <= height {
                version = entry.version;
            }
        }
        version
    }

    /// Validate that a block's version matches the expected version at its height
    pub fn validate_block_version(
        &self,
        block_height: u64,
        block_version: ProtocolVersion,
    ) -> Result<(), ForkError> {
        let expected = self.version_at_height(block_height);
        if block_version != expected {
            return Err(ForkError::VersionMismatch {
                block_height,
                expected,
                actual: block_version,
            });
        }
        Ok(())
    }

    /// Get the current protocol version
    pub fn current_version(&self) -> ProtocolVersion {
        self.current_version
    }

    /// Set the timelock duration
    pub fn set_timelock_blocks(&mut self, blocks: u64) {
        self.timelock_blocks = blocks.max(MIN_TIMELOCK_BLOCKS);
    }

    /// Submit an emergency rollback signature
    pub fn submit_rollback_signature(
        &mut self,
        validator_id: ValidatorId,
        target_height: u64,
        target_version: ProtocolVersion,
        signature: [u8; 64],
    ) -> Result<Option<EmergencyRollbackResult>, ForkError> {
        let public_key = self
            .validator_keys
            .get(&validator_id)
            .ok_or(ForkError::ValidatorNotFound(validator_id))?;

        // Verify Ed25519 signature
        let message = rollback_message_hash(validator_id, target_height, target_version);
        ed25519_verify(public_key, &signature, &message)
            .map_err(|_| ForkError::InvalidSignature)?;

        let rollback = self.active_rollback.get_or_insert_with(|| EmergencyRollback {
            target_height,
            target_version,
            signatures: HashMap::new(),
            total_validators: self.total_validators,
        });

        // Validate the rollback target matches
        if rollback.target_height != target_height || rollback.target_version != target_version {
            return Err(ForkError::RollbackTargetMismatch);
        }

        // Dedup
        if rollback.signatures.contains_key(&validator_id) {
            return Err(ForkError::DuplicateRollbackSignature);
        }

        rollback.signatures.insert(validator_id, signature);

        // Check quorum: 2/3 of validators
        let sig_count = rollback.signatures.len() as u32;
        let quorum = rollback_quorum(self.total_validators);

        if sig_count >= quorum {
            // Quorum reached
            let result = EmergencyRollbackResult {
                target_height: rollback.target_height,
                target_version: rollback.target_version,
                signature_count: sig_count as u32,
                total_validators: self.total_validators,
            };
            self.active_rollback = None; // Clear active rollback (consumed)
            Ok(Some(result))
        } else {
            Ok(None)
        }
    }

    /// Get the next scheduled upgrade
    pub fn next_upgrade(&self, current_height: u64) -> Option<&UpgradeEntry> {
        self.scheduled_upgrades
            .iter()
            .find(|e| !e.applied && e.activation_height > current_height)
    }

    /// Get all scheduled upgrades
    pub fn scheduled_upgrades(&self) -> &[UpgradeEntry] {
        &self.scheduled_upgrades
    }

    /// Check if emergency rollback is in progress
    pub fn rollback_progress(&self) -> Option<(u32, u32)> {
        self.active_rollback.as_ref().map(|r| {
            (r.signatures.len() as u32, rollback_quorum(self.total_validators))
        })
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────

/// Canonical message for rollback signatures
pub fn rollback_message_hash(
    validator_id: ValidatorId,
    target_height: u64,
    target_version: ProtocolVersion,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(4 + 8 + 2 + 2 + 2);
    msg.extend_from_slice(&validator_id.to_le_bytes());
    msg.extend_from_slice(&target_height.to_le_bytes());
    msg.extend_from_slice(&target_version.major.to_le_bytes());
    msg.extend_from_slice(&target_version.minor.to_le_bytes());
    msg.extend_from_slice(&target_version.patch.to_le_bytes());
    msg
}

/// Result when emergency rollback quorum is reached
#[derive(Debug, Clone)]
pub struct EmergencyRollbackResult {
    pub target_height: u64,
    pub target_version: ProtocolVersion,
    pub signature_count: u32,
    pub total_validators: u32,
}

// ─── Errors ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ForkError {
    #[error("validator not found: {0}")]
    ValidatorNotFound(ValidatorId),
    #[error("invalid signature")]
    InvalidSignature,
    #[error("version mismatch at height {block_height}: expected {expected:?}, got {actual:?}")]
    VersionMismatch {
        block_height: u64,
        expected: ProtocolVersion,
        actual: ProtocolVersion,
    },
    #[error("timelock violation: activation at {0} but minimum is {1}")]
    TimelockViolation(u64, u64),
    #[error("duplicate rollback signature")]
    DuplicateRollbackSignature,
    #[error("rollback target mismatch")]
    RollbackTargetMismatch,
    #[error("no active rollback in progress")]
    NoActiveRollback,
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_crypto::ed25519_generate_keypair;
    use call_crypto::ed25519_sign;
    use ed25519_dalek::SigningKey;

    fn make_fork_manager(validator_count: u32) -> (ForkManager, Vec<(ValidatorId, Ed25519PublicKey, SigningKey)>) {
        let mut fm = ForkManager::new(ProtocolVersion::new(1, 0, 0), validator_count);
        let mut validators = Vec::new();
        for i in 0..validator_count {
            let (pubkey, signing_key) = ed25519_generate_keypair();
            fm.register_validator(i, pubkey);
            validators.push((i, pubkey, signing_key));
        }
        (fm, validators)
    }

    #[test]
    fn test_height_activated_upgrade() {
        let (mut fm, _validators) = make_fork_manager(21);

        // Schedule an upgrade at height 1000
        fm.schedule_upgrade(UpgradeEntry {
            version: ProtocolVersion::new(2, 0, 0),
            activation_height: 1000,
            applied: false,
            proposal_id: None,
            approved_at_height: None,
        });

        // Before activation: version is still 1.0.0
        assert_eq!(fm.version_at_height(999), ProtocolVersion::new(1, 0, 0));

        // At and after activation: version is 2.0.0
        assert_eq!(fm.version_at_height(1000), ProtocolVersion::new(2, 0, 0));
        assert_eq!(fm.version_at_height(2000), ProtocolVersion::new(2, 0, 0));

        // Apply the upgrade
        let new_version = fm.check_upgrades_at_height(1000);
        assert_eq!(new_version, Some(ProtocolVersion::new(2, 0, 0)));
        assert_eq!(fm.current_version(), ProtocolVersion::new(2, 0, 0));

        // Should only apply once
        assert!(fm.check_upgrades_at_height(1001).is_none());
    }

    #[test]
    fn test_version_check_mismatch_rejects() {
        let (mut fm, _validators) = make_fork_manager(21);

        // Schedule upgrade at height 1000
        fm.schedule_upgrade(UpgradeEntry {
            version: ProtocolVersion::new(2, 0, 0),
            activation_height: 1000,
            applied: false,
            proposal_id: None,
            approved_at_height: None,
        });

        // Block at height 1000 with correct version should pass
        assert!(fm.validate_block_version(1000, ProtocolVersion::new(2, 0, 0)).is_ok());

        // Block at height 1000 with old version should be rejected
        let err = fm.validate_block_version(1000, ProtocolVersion::new(1, 0, 0)).unwrap_err();
        assert!(matches!(err, ForkError::VersionMismatch { .. }));

        // Block at height 500 with new version should also be rejected
        let err = fm.validate_block_version(500, ProtocolVersion::new(2, 0, 0)).unwrap_err();
        assert!(matches!(err, ForkError::VersionMismatch { .. }));
    }

    #[test]
    fn test_governance_trigger_upgrade() {
        let (mut fm, _validators) = make_fork_manager(21);
        fm.set_timelock_blocks(500);

        // Schedule via governance at current height 100, activation at 700 (>= 100 + 500)
        fm.schedule_governance_upgrade(
            ProtocolVersion::new(1, 1, 0),
            700,
            42,
            100,
        )
        .unwrap();

        // Activation too early should fail
        assert!(fm
            .schedule_governance_upgrade(
                ProtocolVersion::new(1, 2, 0),
                550, // 100 + 500 = 600 minimum
                43,
                100,
            )
            .is_err());

        // Check the upgrade was registered
        let next = fm.next_upgrade(100).unwrap();
        assert_eq!(next.version, ProtocolVersion::new(1, 1, 0));
        assert_eq!(next.proposal_id, Some(42));
        assert_eq!(next.approved_at_height, Some(100));
        assert_eq!(next.activation_height, 700);
    }

    #[test]
    fn test_emergency_rollback() {
        let (mut fm, validators) = make_fork_manager(21);

        let target_height = 500u64;
        let target_version = ProtocolVersion::new(1, 0, 0);

        // Need 2/3 of 21 = 14 signatures
        // Submit signatures from 14 validators
        for i in 0..13 {
            let (vid, _, signing_key) = &validators[i as usize];
            let sig = ed25519_sign(signing_key, &rollback_message_hash(*vid, target_height, target_version));
            let result = fm.submit_rollback_signature(*vid, target_height, target_version, sig).unwrap();
            assert!(result.is_none(), "quorum should not be reached yet");
        }

        // 14th signature reaches quorum
        let (vid14, _, signing_key14) = &validators[13];
        let sig14 = ed25519_sign(signing_key14, &rollback_message_hash(*vid14, target_height, target_version));
        let result = fm
            .submit_rollback_signature(*vid14, target_height, target_version, sig14)
            .unwrap();

        let result = result.expect("quorum should be reached at 14 signatures");
        assert_eq!(result.target_height, target_height);
        assert_eq!(result.target_version, target_version);
        assert_eq!(result.signature_count, 14);
        assert_eq!(result.total_validators, 21);

        // After quorum, active rollback is cleared
        assert!(fm.rollback_progress().is_none());
    }
}
