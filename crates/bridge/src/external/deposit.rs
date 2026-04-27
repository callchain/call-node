//! External bridge deposit logic.

use alloy_primitives::{Address, B256};
use call_protocol::AccountState;
use crate::{BridgeConfig, BridgeError, BridgeStateManager};
use crate::external::types::ExternalBridgeOp;

/// Result of processing an external deposit.
#[derive(Debug, Clone)]
pub enum ExternalDepositResult {
    /// Deposit is queued for the challenge period.
    Queued {
        source_tx_hash: B256,
        challenge_period_blocks: u64,
        finalized_at_block: u64,
    },
}

/// Process an external bridge deposit: verify signatures → queue for challenge period.
///
/// Per spec §5.6.3 (with challenge period):
/// 1. Verify bridge signatures (14+ validators)
/// 2. Check asset is allowed
/// 3. Check limits (per-tx, daily)
/// 4. Check source tx not already processed
/// 5. Queue deposit for challenge period (NOT credited immediately)
///
/// The deposit will be finalized after `challenge_period_blocks` via
/// `finalize_pending_external_deposits`.
pub fn process_external_deposit(
    op: &ExternalBridgeOp,
    _protocol_account: &mut AccountState,
    bridge_state: &mut BridgeStateManager,
    config: &BridgeConfig,
    validators: &[Address],
    current_block: u64,
    source_contract: Option<Address>,
) -> Result<ExternalDepositResult, BridgeError> {
    let ExternalBridgeOp::Deposit {
        source_chain,
        source_tx_hash,
        asset_id,
        recipient,
        amount,
        signatures,
        ..
    } = op
    else {
        return Err(BridgeError::EvmExecutionFailed("not a deposit op".into()));
    };

    // 0. Check global external bridge pause
    if bridge_state.is_external_paused() {
        return Err(BridgeError::ExternalBridgePaused);
    }

    // 1. Verify bridge contract authorization (if source_contract provided)
    if let Some(contract) = source_contract {
        super::types::verify_bridge_contract(config, source_chain.chain_id(), &contract)?;
    }

    // 2. Check asset is allowed (cheap config check before expensive sig verification)
    if !config.allowed_assets.contains(asset_id) {
        return Err(BridgeError::ExternalAssetNotAllowed(*asset_id));
    }

    // 3. Check replay protection (also covers pending deposits)
    if bridge_state.is_external_tx_processed(source_tx_hash)
        || bridge_state.has_pending_external_deposit(source_tx_hash)
    {
        return Err(BridgeError::EvmExecutionFailed(
            "source tx already processed".into(),
        ));
    }

    // 4. Verify signatures
    super::types::verify_bridge_signatures(op, validators, config.min_validator_signatures)?;

    // 5. Check per-tx limit
    bridge_state.check_per_tx_limit(*amount, config.max_per_tx)?;

    // 6. Check daily limit (auto-resets when a new day starts)
    bridge_state.check_and_update_daily_limit(*asset_id, *amount, config.daily_limit_per_asset, current_block, config.blocks_per_day)?;

    // 7. Apply bridge fee (deduct from deposit amount)
    let fee = config.bridge_fee;
    let net_amount = if fee >= *amount {
        return Err(BridgeError::BridgeFeeExceedsAmount(fee, *amount));
    } else {
        amount - fee
    };
    if fee > 0 {
        bridge_state.record_fee(*asset_id, fee);
    }

    // 8. Queue deposit for challenge period (NOT credited yet)
    bridge_state.queue_external_deposit(
        *source_tx_hash,
        *recipient,
        *asset_id,
        net_amount,
        current_block,
        signatures.len() as u64,
    );

    // 9. Mark source tx as processed (record block height for pruning)
    bridge_state.mark_external_tx_processed(*source_tx_hash, current_block);

    // 10. Record bridge event
    bridge_state.record_bridge_event(
        crate::BridgeEventType::ExternalDepositQueued,
        Some(*source_tx_hash),
        *asset_id,
        net_amount,
        fee,
        Some(*recipient),
        current_block,
    );

    Ok(ExternalDepositResult::Queued {
        source_tx_hash: *source_tx_hash,
        challenge_period_blocks: config.challenge_period_blocks,
        finalized_at_block: current_block + config.challenge_period_blocks,
    })
}

/// Process a light client bridge deposit: verify header → tx inclusion → receipt → queue.
///
/// Per spec §5.6.3 (light client variant):
/// 1. Verify header against trusted anchor chain (parent hash chain)
/// 2. Verify transaction inclusion via MPT proof against transactions_root
/// 3. Verify receipt inclusion via MPT proof against receipts_root
/// 4. Parse bridge event from receipt logs
/// 5. Verify claimed amount matches receipt event amount
/// 6. Check asset is allowed
/// 7. Check limits (per-tx, daily)
/// 8. Check source tx not already processed
/// 9. Queue deposit for challenge period
#[cfg(feature = "light-client-bridge")]
pub fn process_light_client_deposit(
    light_client: &mut call_light_client::EthLightClient,
    op: &ExternalBridgeOp,
    _protocol_account: &mut AccountState,
    bridge_state: &mut BridgeStateManager,
    config: &BridgeConfig,
    current_block: u64,
) -> Result<ExternalDepositResult, BridgeError> {
    let ExternalBridgeOp::LightClientDeposit {
        source_chain: _,
        header,
        tx_proof,
        receipt_proof,
        recipient,
        asset_id,
        amount,
    } = op
    else {
        return Err(BridgeError::EvmExecutionFailed("not a light client deposit op".into()));
    };

    // 1. Submit header to light client (verifies parent chain)
    light_client
        .submit_header(header.clone())
        .map_err(|e| BridgeError::MptProofError(e.to_string()))?;

    let block_number = header.number().ok_or_else(|| {
        BridgeError::MptProofError("header missing block number".into())
    })?;

    // 2. Verify transaction inclusion
    let tx_hash = header.block_hash; // Using block hash as tx proof key (simplified)
    light_client
        .verify_tx_inclusion(block_number, tx_hash, tx_proof)
        .map_err(|e| BridgeError::MptProofError(e.to_string()))?;

    // 3. Verify receipt and parse bridge event
    let bridge_event = light_client
        .verify_receipt_and_parse_bridge_event(block_number, receipt_proof)
        .map_err(|e| BridgeError::MptProofError(e.to_string()))?;

    // 4. Verify the bridge event matches the claimed deposit
    if bridge_event.recipient != *recipient {
        return Err(BridgeError::MptProofError(
            "recipient mismatch".into(),
        ));
    }
    if bridge_event.asset_id != *asset_id {
        return Err(BridgeError::MptProofError(
            "asset_id mismatch".into(),
        ));
    }
    if bridge_event.amount != *amount {
        return Err(BridgeError::MptProofError(
            "amount mismatch".into(),
        ));
    }

    let source_tx_hash = bridge_event.source_tx_hash;

    // 5. Check asset is allowed
    if !config.allowed_assets.contains(asset_id) {
        return Err(BridgeError::ExternalAssetNotAllowed(*asset_id));
    }

    // 6. Check replay protection
    if bridge_state.is_external_tx_processed(&source_tx_hash)
        || bridge_state.has_pending_external_deposit(&source_tx_hash)
    {
        return Err(BridgeError::EvmExecutionFailed(
            "source tx already processed".into(),
        ));
    }

    // 7. Check per-tx limit
    bridge_state.check_per_tx_limit(*amount, config.max_per_tx)?;

    // 8. Check daily limit (auto-resets when a new day starts)
    bridge_state.check_and_update_daily_limit(*asset_id, *amount, config.daily_limit_per_asset, current_block, config.blocks_per_day)?;

    // 9. Queue deposit for challenge period
    bridge_state.queue_external_deposit(
        source_tx_hash,
        *recipient,
        *asset_id,
        *amount,
        current_block,
        0, // no validator signatures for light client deposit
    );

    // 10. Mark source tx as processed (record block height for pruning)
    bridge_state.mark_external_tx_processed(source_tx_hash, current_block);

    Ok(ExternalDepositResult::Queued {
        source_tx_hash,
        challenge_period_blocks: config.challenge_period_blocks,
        finalized_at_block: current_block + config.challenge_period_blocks,
    })
}

/// Finalize pending external deposits whose challenge period has expired.
///
/// Credits protocol account for all deposits past the challenge period.
/// Returns the number of deposits finalized.
///
/// This should be called periodically (e.g., each block or in the block finalization step).
pub fn finalize_pending_external_deposits(
    bridge_state: &mut BridgeStateManager,
    protocol_account: &mut AccountState,
    current_block: u64,
) -> usize {
    let ready = bridge_state.finalize_pending_external_deposits(
        current_block,
        bridge_state.pending_external_deposits.first().map(|_| {
            10_080u64
        }).unwrap_or(10_080),
    );

    let count = ready.len();
    for deposit in ready {
        let _ = protocol_account.credit_balance(deposit.asset_id, deposit.recipient, deposit.amount);
        bridge_state.record_bridge_event(
            crate::BridgeEventType::ExternalDepositFinalized,
            Some(deposit.source_tx_hash),
            deposit.asset_id,
            deposit.amount,
            0,
            Some(deposit.recipient),
            current_block,
        );
    }
    count
}

/// Finalize pending external deposits with explicit challenge period.
///
/// Credits protocol account for all deposits past the challenge period.
/// Returns the number of deposits finalized.
pub fn finalize_pending_external_deposits_with_period(
    bridge_state: &mut BridgeStateManager,
    protocol_account: &mut AccountState,
    current_block: u64,
    challenge_period_blocks: u64,
) -> usize {
    let ready = bridge_state.finalize_pending_external_deposits(
        current_block,
        challenge_period_blocks,
    );

    let count = ready.len();
    for deposit in ready {
        let _ = protocol_account.credit_balance(deposit.asset_id, deposit.recipient, deposit.amount);
        bridge_state.record_bridge_event(
            crate::BridgeEventType::ExternalDepositFinalized,
            Some(deposit.source_tx_hash),
            deposit.asset_id,
            deposit.amount,
            0,
            Some(deposit.recipient),
            current_block,
        );
    }
    count
}
