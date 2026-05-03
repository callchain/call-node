//! External bridge deposit logic.

use alloy_primitives::{Address, B256};
use crate::{BridgeConfig, BridgeError};
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

// ── EVM bridge state helpers (inline to avoid circular dep on call-consensus) ──

const BRIDGE_ADDRESS: alloy_primitives::Address = alloy_primitives::address!("0000000000000000000000000000000000000103");

fn slot_bridge_processed(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[b"processed", &tx_hash])
}

fn slot_bridge_pending_count() -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[b"pending_count"])
}

fn slot_bridge_pending_hash(index: u64) -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[b"pending_list"]) + alloy_primitives::U256::from(index)
}

fn slot_bridge_pending_status(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[b"pending_status", &tx_hash])
}

fn slot_bridge_pending_recipient(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[b"pending_recipient", &tx_hash])
}

fn slot_bridge_pending_asset(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[b"pending_asset", &tx_hash])
}

fn slot_bridge_pending_amount(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[b"pending_amount", &tx_hash])
}

fn slot_bridge_pending_block(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[b"pending_block", &tx_hash])
}

fn slot_bridge_daily_used(asset_id: u64) -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"daily"])
}

fn slot_bridge_daily_day(asset_id: u64) -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"daily_day"])
}

fn slot_bridge_external_paused() -> alloy_primitives::U256 {
    call_precompiles::storage::storage_slot(&[b"external_paused"])
}

fn read_bridge_processed(evm: &call_evm::EvmState, tx_hash: [u8; 32]) -> bool {
    evm.get_storage(&BRIDGE_ADDRESS, slot_bridge_processed(tx_hash)) != alloy_primitives::U256::ZERO
}

fn read_bridge_pending_count(evm: &call_evm::EvmState) -> u64 {
    call_precompiles::u256_to_u64(evm.get_storage(&BRIDGE_ADDRESS, slot_bridge_pending_count()))
}

fn read_bridge_daily_used(evm: &call_evm::EvmState, asset_id: u64) -> u128 {
    call_precompiles::u256_to_u128(evm.get_storage(&BRIDGE_ADDRESS, slot_bridge_daily_used(asset_id)))
}

fn read_bridge_daily_day(evm: &call_evm::EvmState, asset_id: u64) -> u64 {
    call_precompiles::u256_to_u64(evm.get_storage(&BRIDGE_ADDRESS, slot_bridge_daily_day(asset_id)))
}

fn read_bridge_external_paused(evm: &call_evm::EvmState) -> bool {
    evm.get_storage(&BRIDGE_ADDRESS, slot_bridge_external_paused()) != alloy_primitives::U256::ZERO
}

fn seed_bridge_pending(
    evm: &mut call_evm::EvmState,
    tx_hash: [u8; 32],
    recipient: Address,
    asset_id: u64,
    amount: u128,
    block: u64,
) {
    let count = read_bridge_pending_count(evm);
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_hash(count), alloy_primitives::U256::from_be_slice(&tx_hash));
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_count(), call_precompiles::u64_to_u256(count + 1));
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_status(tx_hash), alloy_primitives::U256::from(1u8));
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_recipient(tx_hash), call_precompiles::address_to_u256(recipient));
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_asset(tx_hash), call_precompiles::u64_to_u256(asset_id));
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_amount(tx_hash), call_precompiles::u128_to_u256(amount));
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_pending_block(tx_hash), call_precompiles::u64_to_u256(block));
}

fn seed_bridge_processed(evm: &mut call_evm::EvmState, tx_hash: [u8; 32], block_height: u64) {
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_processed(tx_hash), call_precompiles::u64_to_u256(block_height));
}

fn update_bridge_daily(evm: &mut call_evm::EvmState, asset_id: u64, used: u128, day: u64) {
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_daily_used(asset_id), call_precompiles::u128_to_u256(used));
    evm.set_storage(BRIDGE_ADDRESS, slot_bridge_daily_day(asset_id), call_precompiles::u64_to_u256(day));
}

/// Check and update daily limit using EVM storage.
fn check_and_update_daily_limit_evm(
    evm: &mut call_evm::EvmState,
    asset_id: u64,
    amount: u128,
    daily_limit: u128,
    current_block: u64,
    blocks_per_day: u64,
) -> Result<(), BridgeError> {
    let reset_at = read_bridge_daily_day(evm, asset_id);
    let used = if current_block >= reset_at + blocks_per_day {
        0
    } else {
        read_bridge_daily_used(evm, asset_id)
    };
    if used + amount > daily_limit {
        return Err(BridgeError::ExceedsDailyLimit(asset_id, used, daily_limit));
    }
    let new_day = if current_block >= reset_at + blocks_per_day { current_block } else { reset_at };
    update_bridge_daily(evm, asset_id, used + amount, new_day);
    Ok(())
}

/// EVM-based version of external deposit processing.
/// Writes all state (replay protection, daily limit, pending queue) to EVM storage.
pub fn process_external_deposit_evm(
    op: &ExternalBridgeOp,
    evm: &mut call_evm::EvmState,
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

    if read_bridge_external_paused(evm) {
        return Err(BridgeError::ExternalBridgePaused);
    }

    if let Some(contract) = source_contract {
        super::types::verify_bridge_contract(config, source_chain.chain_id(), &contract)?;
    }

    if !config.allowed_assets.contains(asset_id) {
        return Err(BridgeError::ExternalAssetNotAllowed(*asset_id));
    }

    if read_bridge_processed(evm, **source_tx_hash) {
        return Err(BridgeError::EvmExecutionFailed(
            "source tx already processed".into(),
        ));
    }

    super::types::verify_bridge_signatures(op, validators, config.min_validator_signatures)?;

    if *amount > config.max_per_tx {
        return Err(BridgeError::ExceedsMaxPerTx(*asset_id, *amount, config.max_per_tx));
    }

    check_and_update_daily_limit_evm(evm, *asset_id, *amount, config.daily_limit_per_asset, current_block, config.blocks_per_day)?;

    let fee = config.bridge_fee;
    let net_amount = if fee >= *amount {
        return Err(BridgeError::BridgeFeeExceedsAmount(fee, *amount));
    } else {
        amount - fee
    };

    seed_bridge_pending(evm, **source_tx_hash, *recipient, *asset_id, net_amount, current_block);
    seed_bridge_processed(evm, **source_tx_hash, current_block);

    Ok(ExternalDepositResult::Queued {
        source_tx_hash: *source_tx_hash,
        challenge_period_blocks: config.challenge_period_blocks,
        finalized_at_block: current_block + config.challenge_period_blocks,
    })
}

/// EVM-based version of light client deposit processing.
#[cfg(feature = "light-client-bridge")]
pub fn process_light_client_deposit_evm(
    light_client: &mut call_light_client::EthLightClient,
    op: &ExternalBridgeOp,
    evm: &mut call_evm::EvmState,
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

    light_client
        .submit_header(header.clone())
        .map_err(|e| BridgeError::MptProofError(e.to_string()))?;

    let block_number = header.number().ok_or_else(|| {
        BridgeError::MptProofError("header missing block number".into())
    })?;

    let tx_hash = header.block_hash;
    light_client
        .verify_tx_inclusion(block_number, tx_hash, tx_proof)
        .map_err(|e| BridgeError::MptProofError(e.to_string()))?;

    let bridge_event = light_client
        .verify_receipt_and_parse_bridge_event(block_number, receipt_proof)
        .map_err(|e| BridgeError::MptProofError(e.to_string()))?;

    if bridge_event.recipient != *recipient {
        return Err(BridgeError::MptProofError("recipient mismatch".into()));
    }
    if bridge_event.asset_id != *asset_id {
        return Err(BridgeError::MptProofError("asset_id mismatch".into()));
    }
    if bridge_event.amount != *amount {
        return Err(BridgeError::MptProofError("amount mismatch".into()));
    }

    let source_tx_hash = bridge_event.source_tx_hash;

    if !config.allowed_assets.contains(asset_id) {
        return Err(BridgeError::ExternalAssetNotAllowed(*asset_id));
    }

    if read_bridge_processed(evm, *source_tx_hash) {
        return Err(BridgeError::EvmExecutionFailed(
            "source tx already processed".into(),
        ));
    }

    if *amount > config.max_per_tx {
        return Err(BridgeError::ExceedsMaxPerTx(*asset_id, *amount, config.max_per_tx));
    }

    check_and_update_daily_limit_evm(evm, *asset_id, *amount, config.daily_limit_per_asset, current_block, config.blocks_per_day)?;

    seed_bridge_pending(evm, *source_tx_hash, *recipient, *asset_id, *amount, current_block);
    seed_bridge_processed(evm, *source_tx_hash, current_block);

    Ok(ExternalDepositResult::Queued {
        source_tx_hash,
        challenge_period_blocks: config.challenge_period_blocks,
        finalized_at_block: current_block + config.challenge_period_blocks,
    })
}
