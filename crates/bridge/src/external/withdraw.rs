//! External bridge withdrawal and signing logic.

use alloy_primitives::{Address, B256};
use call_primitives::{AssetId, Signature};
use call_crypto::secp256k1_sign;
use call_protocol::AccountState;
use crate::{BridgeConfig, BridgeError, BridgeStateManager};
use crate::external::types::{ExternalBridgeOp, ExternalChain, bridge_event_hash};

/// Sign a bridge event as a validator (per spec §5.6.2)
///
/// Returns the secp256k1 signature (65 bytes: r || s || v) over the bridge event hash.
pub fn sign_bridge_event(
    secret_key: &[u8; 32],
    source_chain: &ExternalChain,
    source_tx_hash: B256,
    source_block_number: u64,
    sender: &[u8],
    recipient: Address,
    asset_id: AssetId,
    amount: u128,
) -> Signature {
    let event_hash = bridge_event_hash(
        source_chain,
        source_tx_hash,
        source_block_number,
        sender,
        recipient,
        asset_id,
        amount,
    );

    secp256k1_sign(secret_key, &event_hash.0)
}

/// Process an external bridge withdrawal: burn protocol → emit event for validators to sign
///
/// Per spec §5.6.4:
/// 1. Check asset is allowed
/// 2. Check limits (per-tx, daily, per-period)
/// 3. Deduct protocol balance
/// 4. Return withdraw event for validator signing
pub fn process_external_withdraw(
    op: &ExternalBridgeOp,
    protocol_account: &mut AccountState,
    bridge_state: &mut BridgeStateManager,
    config: &BridgeConfig,
    current_block: u64,
) -> Result<(), BridgeError> {
    let ExternalBridgeOp::Withdraw {
        asset_id,
        sender,
        amount,
        target_address,
        ..
    } = op
    else {
        return Err(BridgeError::EvmExecutionFailed("not a withdraw op".into()));
    };

    // 0. Check global external bridge pause
    if bridge_state.is_external_paused() {
        return Err(BridgeError::ExternalBridgePaused);
    }

    // 1. Check asset is allowed
    if !config.allowed_assets.contains(asset_id) {
        return Err(BridgeError::ExternalAssetNotAllowed(*asset_id));
    }

    // 2. Check per-tx limit (on gross amount)
    bridge_state.check_per_tx_limit(*amount, config.max_per_tx)?;

    // 3. Check daily limit (on gross amount, auto-resets when a new day starts)
    bridge_state.check_and_update_daily_limit(*asset_id, *amount, config.daily_limit_per_asset, current_block, config.blocks_per_day)?;

    // 4. Check per-period withdrawal limit (limits blast radius of compromised keys)
    bridge_state.check_and_update_external_withdrawal_limit(
        current_block,
        config.challenge_period_blocks,
        *asset_id,
        *amount,
        config.max_external_withdraw_per_period,
    )?;

    // 5. Apply bridge fee: total deduction = amount + fee
    let fee = config.bridge_fee;
    let total_deduction = amount.checked_add(fee)
        .ok_or_else(|| BridgeError::BridgeFeeExceedsAmount(fee, *amount))?;

    // 6. Check protocol balance
    let balance = protocol_account.get_balance(*asset_id, sender);
    if balance < total_deduction {
        return Err(BridgeError::InsufficientProtocolBalance(*asset_id, total_deduction));
    }

    // 7. Deduct protocol balance (amount + fee)
    protocol_account.deduct_balance(*asset_id, *sender, total_deduction)?;

    // 8. Record fee
    if fee > 0 {
        bridge_state.record_fee(*asset_id, fee);
    }

    // 9. Record withdrawal
    bridge_state.record_withdrawal(*asset_id, *amount);

    // 10. Record bridge event
    let recipient_addr = Address::try_from(target_address.as_slice()).ok();
    bridge_state.record_bridge_event(
        crate::BridgeEventType::ExternalWithdraw,
        None,
        *asset_id,
        *amount,
        fee,
        recipient_addr,
        current_block,
    );

    Ok(())
}
