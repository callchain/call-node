//! External bridge challenge logic.

use alloy_primitives::B256;
use crate::{BridgeConfig, BridgeStateManager};

/// Revoke a pending external deposit during the challenge period.
///
/// Permissionless — anyone can call this to challenge a suspicious deposit.
/// This is the "fraud proof" mechanism for Phase 1.
///
/// Returns `true` if a deposit was found and revoked.
pub fn challenge_pending_deposit(
    bridge_state: &mut BridgeStateManager,
    source_tx_hash: &B256,
    current_block: u64,
) -> bool {
    let revoked = bridge_state.revoke_pending_external_deposit(source_tx_hash);
    if revoked {
        // Find the deposit details for event recording (scan pending first, then fall back)
        bridge_state.record_bridge_event(
            crate::BridgeEventType::ExternalDepositChallenged,
            Some(*source_tx_hash),
            0, // asset_id unknown after removal
            0,
            0,
            None,
            current_block,
        );
    }
    revoked
}

/// Check if bridge signature collection is complete for a pending deposit
pub fn is_signature_complete(
    signatures_received: u64,
    config: &BridgeConfig,
) -> bool {
    signatures_received >= config.min_validator_signatures
}
