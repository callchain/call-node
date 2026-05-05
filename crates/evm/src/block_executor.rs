//! Block-level execution backed by MDBX via StateProviderDatabase.
//!
//! This module provides `execute_block_transactions`, the preferred execution
//! path for the P0 migration. It reads initial state from MDBX (through an
//! `InMemoryStateProvider`), accumulates all changes in a `CacheDB`, and
//! returns the raw revm delta for the caller to commit.
//!
//! Protocol-level settlement (base-fee update, oracle reward) is intentionally
//! **not** performed here — it is the caller's responsibility.

use std::collections::HashSet;

use alloy_primitives::{keccak256, U256};
use revm::database::CacheDB;
use revm::database_interface::{DatabaseCommit, DatabaseRef};

use crate::{
    provider::InMemoryStateProvider,
    BlockGasTracker, EvmError, EvmTransaction, EvmExecutor,
};

/// Result of executing all EVM transactions in a block.
#[derive(Debug, Default)]
pub struct BlockTxResult {
    pub evm_tx_count: usize,
    pub evm_gas_used: u64,
    pub evm_tx_results: Vec<BlockTxEntry>,
}

/// Per-transaction result (mirrors `EvmTxResult` from consensus).
#[derive(Debug, Clone)]
pub struct BlockTxEntry {
    pub tx_hash: alloy_primitives::B256,
    pub gas_used: u64,
    pub status: bool,
    pub caller: call_primitives::Address,
    pub to: Option<call_primitives::Address>,
    pub contract_address: Option<call_primitives::Address>,
    pub logs: Vec<call_protocol::LogEntry>,
    pub gas_price: u128,
}

/// Execute every EVM transaction in `evm_txs` against a `CacheDB` backed by
/// `StateProviderDatabase<InMemoryStateProvider>`.
///
/// # Flow
/// 1. Create `InMemoryStateProvider` from MDBX (loads full state snapshot).
/// 2. Wrap it in `StateProviderDatabase` → `CacheDB`.
/// 3. Decode, validate, and execute each tx on the `CacheDB`.
/// 4. Return the collected results **and** the raw revm `EvmState` delta.
///
/// The caller must commit the delta to persistent storage (MDBX) and/or an
/// in-memory `EvmState` overlay.
pub fn execute_block_transactions(
    evm_txs: &[Vec<u8>],
    provider: InMemoryStateProvider,
    block_number: u64,
    base_fee: u128,
) -> Result<(BlockTxResult, revm::state::EvmState), EvmError> {
    let executor = EvmExecutor::new(1);
    let max_evm_gas = 30_000_000u64;
    let mut gas_tracker = BlockGasTracker::new(max_evm_gas);
    let mut used_evm_nonces: HashSet<(call_primitives::Address, u64)> = HashSet::new();

    let db_wrapper = reth_revm::database::StateProviderDatabase::new(provider);
    let mut cache_db = CacheDB::new(db_wrapper);

    let mut result = BlockTxResult::default();
    let mut accumulated_state = revm::state::EvmState::default();

    for raw_tx in evm_txs {
        let tx = match decode_evm_tx(raw_tx) {
            Ok(t) => t,
            Err(()) => {
                tracing::warn!("block_executor: decode_evm_tx failed, skipping tx");
                continue;
            }
        };
        let caller = tx.caller;
        let nonce = tx.nonce;

        // Duplicate-nonce guard (block-scoped)
        if !used_evm_nonces.insert((caller, nonce)) {
            tracing::warn!(?caller, nonce, "block_executor: duplicate evm nonce in same block, skipping");
            continue;
        }

        // Validate nonce and balance against CacheDB
        if let Err(e) = validate_evm_tx_cached(&tx, &cache_db) {
            tracing::warn!(?caller, tx_nonce = nonce, error = ?e, "block_executor: validate_evm_tx failed, skipping");
            continue;
        }

        let tx_to = tx.to;
        let tx_gas_price = tx.gas_price;

        match executor.execute_tx_cached(tx, &mut cache_db, block_number, base_fee) {
            Ok((exec_result, tx_delta)) => {
                tracing::info!(?caller, nonce, gas_used = exec_result.gas_used, success = exec_result.success, "block_executor: evm tx executed");
                if gas_tracker.add_gas(exec_result.gas_used).is_err() {
                    tracing::warn!(?caller, gas_used = exec_result.gas_used, "block_executor: block gas limit exceeded, skipping");
                    continue;
                }
                result.evm_tx_count += 1;
                result.evm_gas_used += exec_result.gas_used;

                // Update CacheDB so subsequent transactions see the latest state.
                cache_db.commit(tx_delta.clone());

                // Accumulate state delta from this transaction
                for (addr, account) in tx_delta {
                    accumulated_state.insert(addr, account);
                }

                let contract_address = if tx_to.is_none() {
                    Some(crate::derive_create_address(caller, nonce))
                } else {
                    None
                };

                let logs: Vec<call_protocol::LogEntry> = exec_result
                    .logs
                    .iter()
                    .map(|log| call_protocol::LogEntry {
                        address: log.address,
                        topics: log
                            .data
                            .topics()
                            .iter()
                            .map(|t| call_primitives::Hash::from(t.0))
                            .collect(),
                        data: log.data.data.to_vec(),
                    })
                    .collect();

                let tx_hash = keccak256(raw_tx);
                result.evm_tx_results.push(BlockTxEntry {
                    tx_hash,
                    gas_used: exec_result.gas_used,
                    status: exec_result.success,
                    caller,
                    to: tx_to,
                    contract_address,
                    logs,
                    gas_price: tx_gas_price,
                });
            }
            Err(e) => {
                tracing::warn!(?caller, error = ?e, "block_executor: executor.execute_tx_cached failed, skipping");
            }
        }
    }

    Ok((result, accumulated_state))
}

/// Validate an EVM transaction before execution, reading state from a `CacheDB`.
fn validate_evm_tx_cached<DB: revm::DatabaseRef>(
    tx: &EvmTransaction,
    cache_db: &CacheDB<DB>,
) -> Result<(), &'static str>
where
    DB::Error: core::fmt::Debug,
{
    let expected_nonce = cache_db
        .basic_ref(tx.caller)
        .map(|a| a.map(|i| i.nonce).unwrap_or(0))
        .map_err(|_| "db error reading nonce")?;
    if tx.nonce != expected_nonce {
        return Err("invalid nonce");
    }

    let balance = cache_db
        .basic_ref(tx.caller)
        .map(|a| a.map(|i| i.balance).unwrap_or_default())
        .map_err(|_| "db error reading balance")?;
    let max_gas_cost = U256::from(tx.gas_limit) * U256::from(tx.gas_price);
    let required = tx.value + max_gas_cost;
    if balance < required {
        return Err("insufficient balance");
    }

    Ok(())
}

/// Decode raw EVM transaction bytes (duplicated from consensus/block.rs).
fn decode_evm_tx(raw: &[u8]) -> Result<EvmTransaction, ()> {
    use alloy_consensus::{Transaction, TxEnvelope};
    use alloy_rlp::Decodable;

    if raw.is_empty() {
        return Err(());
    }

    // 1) Try RLP / EIP-2718 enveloped decoding
    if let Ok(envelope) = TxEnvelope::decode(&mut &raw[..]) {
        match envelope {
            TxEnvelope::Legacy(signed) => {
                let tx = signed.tx();
                let caller = signed.recover_signer().map_err(|_| ())?;
                return Ok(EvmTransaction {
                    caller,
                    nonce: tx.nonce(),
                    gas_limit: tx.gas_limit(),
                    gas_price: tx.gas_price().unwrap_or(0),
                    to: tx.to(),
                    value: tx.value(),
                    data: tx.input().clone(),
                    chain_id: tx.chain_id().unwrap_or(1),
                });
            }
            TxEnvelope::Eip1559(signed) => {
                let tx = signed.tx();
                let caller = signed.recover_signer().map_err(|_| ())?;
                return Ok(EvmTransaction {
                    caller,
                    nonce: tx.nonce(),
                    gas_limit: tx.gas_limit(),
                    gas_price: tx.max_fee_per_gas(),
                    to: tx.to(),
                    value: tx.value(),
                    data: tx.input().clone(),
                    chain_id: tx.chain_id().unwrap_or(1),
                });
            }
            _ => return Err(()),
        }
    }

    // 2) Fallback: JSON-serialized EvmTransaction
    if let Ok(tx) = serde_json::from_slice::<EvmTransaction>(raw) {
        return Ok(tx);
    }

    Err(())
}
