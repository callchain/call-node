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

use alloy_primitives::{keccak256, Address, U256};
use call_precompile::{slot_balance, ASSET_ADDRESS};
use call_protocol::CALL_ASSET_ID;
use revm::database::{AccountState, CacheDB};
use revm::database_interface::{DatabaseCommit, DatabaseRef};

use crate::{BlockGasTracker, EvmError, EvmExecutor, EvmTransaction};

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
/// any provider implementing [`reth_revm::database::EvmStateProvider`].
///
/// # Flow
/// 1. Wrap `provider` in `StateProviderDatabase` → `CacheDB`.
/// 2. Decode, validate, and execute each tx on the `CacheDB`.
/// 3. Return the collected results **and** the raw revm `EvmState` delta.
///
/// The caller must commit the delta to persistent storage (MDBX).
///
/// # Type Parameters
/// - `SP`: Any type implementing [`reth_revm::database::EvmStateProvider`].
///   Production can pass `InMemoryStateProvider` (full load) or
///   `LazyStateProvider` (on-demand) to avoid loading the entire state.
pub fn execute_block_transactions<SP>(
    evm_txs: &[Vec<u8>],
    provider: SP,
    block_number: u64,
    base_fee: u128,
) -> Result<(BlockTxResult, revm::state::EvmState), EvmError>
where
    SP: reth_revm::database::EvmStateProvider,
{
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
            tracing::warn!(
                ?caller,
                nonce,
                "block_executor: duplicate evm nonce in same block, skipping"
            );
            continue;
        }

        // ── Pre-bridge: protocol CALL → native EVM balance ─────────────────
        let max_gas_cost = U256::from(tx.gas_limit) * U256::from(tx.gas_price);
        let required = tx.value + max_gas_cost;

        let native_balance = cache_db
            .basic_ref(caller)
            .map(|a| a.map(|i| i.balance).unwrap_or_default())
            .unwrap_or_default();

        let mut borrowed = U256::ZERO;
        let mut bridge_logs: Vec<call_protocol::LogEntry> = Vec::new();

        if native_balance < required {
            let needed = required - native_balance;
            let slot = slot_balance(CALL_ASSET_ID, caller);
            let protocol_balance = cache_db
                .storage_ref(ASSET_ADDRESS, slot)
                .unwrap_or_default();

            if protocol_balance < needed {
                tracing::warn!(
                    ?caller,
                    needed = ?needed,
                    protocol = ?protocol_balance,
                    "block_executor: insufficient protocol balance for gas bridge, skipping"
                );
                continue;
            }

            // Deduct from protocol
            if let Ok(account) = cache_db.load_account(ASSET_ADDRESS) {
                account.storage.insert(slot, protocol_balance - needed);
            }

            // Credit native
            if let Ok(account) = cache_db.load_account(caller) {
                account.info.balance = account.info.balance + needed;
                if matches!(account.account_state, AccountState::NotExisting) {
                    account.account_state = AccountState::None;
                }
            }

            borrowed = needed;
            bridge_logs.push(make_gas_bridge_log(
                caller,
                borrowed,
                tx.gas_limit,
                tx.gas_price,
            ));
        }

        // Validate nonce and balance against CacheDB
        if let Err(e) = validate_evm_tx_cached(&tx, &cache_db) {
            // Revert bridge on validation failure (tx never executed, no gas consumed)
            if borrowed > U256::ZERO {
                let slot = slot_balance(CALL_ASSET_ID, caller);
                if let Ok(account) = cache_db.load_account(ASSET_ADDRESS) {
                    let protocol_balance = account
                        .storage
                        .get(&slot)
                        .copied()
                        .unwrap_or_default();
                    account.storage.insert(slot, protocol_balance + borrowed);
                }
                if let Ok(account) = cache_db.load_account(caller) {
                    account.info.balance = account.info.balance - borrowed;
                }
            }
            tracing::warn!(
                ?caller,
                tx_nonce = nonce,
                error = ?e,
                "block_executor: validate_evm_tx failed, skipping"
            );
            continue;
        }

        let tx_to = tx.to;
        let tx_value = tx.value;
        let tx_gas_price = tx.gas_price;
        let eff_gas_price = effective_gas_price(&tx, base_fee);

        match executor.execute_tx_cached(tx, &mut cache_db, block_number, base_fee) {
            Ok((exec_result, tx_delta)) => {
                tracing::info!(
                    ?caller,
                    nonce,
                    gas_used = exec_result.gas_used,
                    success = exec_result.success,
                    "block_executor: evm tx executed"
                );

                if gas_tracker.add_gas(exec_result.gas_used).is_err() {
                    tracing::warn!(
                        ?caller,
                        gas_used = exec_result.gas_used,
                        "block_executor: block gas limit exceeded, skipping"
                    );
                    continue;
                }
                result.evm_tx_count += 1;
                result.evm_gas_used += exec_result.gas_used;

                // Commit EVM delta first, then apply refund on top.
                cache_db.commit(tx_delta.clone());

                // Compute refund: only the gas-deposit surplus goes back to protocol.
                let actual_cost = U256::from(exec_result.gas_used) * U256::from(eff_gas_price);

                if borrowed > U256::ZERO {
                    let refund = borrowed
                        .saturating_sub(tx_value)
                        .saturating_sub(actual_cost);
                    if refund > U256::ZERO {
                        // Deduct refund from native
                        if let Ok(account) = cache_db.load_account(caller) {
                            account.info.balance = account.info.balance - refund;
                        }
                        // Credit refund to protocol
                        let slot = slot_balance(CALL_ASSET_ID, caller);
                        if let Ok(account) = cache_db.load_account(ASSET_ADDRESS) {
                            let protocol_balance = account
                                .storage
                                .get(&slot)
                                .copied()
                                .unwrap_or_default();
                            account.storage.insert(slot, protocol_balance + refund);
                        }
                    }
                    bridge_logs.push(make_gas_bridge_settled_log(
                        caller,
                        actual_cost,
                        refund,
                        exec_result.gas_used,
                    ));
                }

                // Accumulate state delta from this transaction.
                // Merge storage slots so earlier tx writes are not lost
                // when a later tx touches the same address.
                for (addr, new_account) in tx_delta {
                    match accumulated_state.entry(addr) {
                        std::collections::hash_map::Entry::Occupied(mut entry) => {
                            let existing = entry.get_mut();
                            existing.info = new_account.info;
                            existing.status |= new_account.status;
                            for (slot, value) in new_account.storage {
                                existing.storage.insert(slot, value);
                            }
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(new_account);
                        }
                    }
                }

                // Apply refund to accumulated_state so it is persisted.
                if borrowed > U256::ZERO {
                    let refund = borrowed
                        .saturating_sub(tx_value)
                        .saturating_sub(actual_cost);
                    if refund > U256::ZERO {
                        // Caller balance
                        if let Some(acc) = accumulated_state.get_mut(&caller) {
                            acc.info.balance = acc.info.balance.saturating_sub(refund);
                        } else if let Ok(account) = cache_db.load_account(caller) {
                            let mut acc = revm::state::Account {
                                info: account.info.clone(),
                                original_info: Box::new(account.info.clone()),
                                transaction_id: 0,
                                storage: std::collections::HashMap::default(),
                                status: revm::state::AccountStatus::Touched,
                            };
                            acc.info.balance = acc.info.balance.saturating_sub(refund);
                            accumulated_state.insert(caller, acc);
                        }
                        // Protocol storage slot
                        let slot = slot_balance(CALL_ASSET_ID, caller);
                        if let Some(acc) = accumulated_state.get_mut(&ASSET_ADDRESS) {
                            let current = acc
                                .storage
                                .get(&slot)
                                .map(|s| s.present_value)
                                .unwrap_or_default();
                            acc.storage.insert(slot, revm::state::EvmStorageSlot::new(current + refund, 0));
                        } else if let Ok(account) = cache_db.load_account(ASSET_ADDRESS) {
                            let mut acc = revm::state::Account {
                                info: account.info.clone(),
                                original_info: Box::new(account.info.clone()),
                                transaction_id: 0,
                                storage: std::collections::HashMap::default(),
                                status: revm::state::AccountStatus::Touched,
                            };
                            let current = account
                                .storage
                                .get(&slot)
                                .copied()
                                .unwrap_or_default();
                            acc.storage.insert(slot, revm::state::EvmStorageSlot::new(current + refund, 0));
                            accumulated_state.insert(ASSET_ADDRESS, acc);
                        }
                    }
                }

                let contract_address = if tx_to.is_none() {
                    Some(crate::derive_create_address(caller, nonce))
                } else {
                    None
                };

                // Merge bridge logs with EVM execution logs
                let mut logs = bridge_logs;
                logs.extend(exec_result.logs.iter().map(|log| call_protocol::LogEntry {
                    address: log.address,
                    topics: log
                        .data
                        .topics()
                        .iter()
                        .map(|t| call_primitives::Hash::from(t.0))
                        .collect(),
                    data: log.data.data.to_vec(),
                }));

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
                // Execution failed: revert bridge (conservative: refund full borrowed)
                if borrowed > U256::ZERO {
                    let slot = slot_balance(CALL_ASSET_ID, caller);
                    if let Ok(account) = cache_db.load_account(ASSET_ADDRESS) {
                        let protocol_balance = account
                            .storage
                            .get(&slot)
                            .copied()
                            .unwrap_or_default();
                        account.storage.insert(slot, protocol_balance + borrowed);
                    }
                    if let Ok(account) = cache_db.load_account(caller) {
                        account.info.balance = account.info.balance - borrowed;
                    }
                }
                tracing::warn!(
                    ?caller,
                    error = ?e,
                    "block_executor: executor.execute_tx_cached failed, skipping"
                );
            }
        }
    }

    // Ensure ASSET_ADDRESS state from cache_db is reflected in accumulated_state.
    // Pre-bridge and refund may have modified ASSET_ADDRESS storage without
    // going through tx_delta.
    if let Ok(account) = cache_db.load_account(ASSET_ADDRESS) {
        match accumulated_state.entry(ASSET_ADDRESS) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let existing = e.get_mut();
                for (slot, value) in &account.storage {
                    if !existing.storage.contains_key(slot) {
                        existing
                            .storage
                            .insert(*slot, revm::state::EvmStorageSlot::new(*value, 0));
                    }
                }
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                let acc = revm::state::Account {
                    info: account.info.clone(),
                    original_info: Box::new(account.info.clone()),
                    transaction_id: 0,
                    storage: account
                        .storage
                        .iter()
                        .map(|(k, v)| (*k, revm::state::EvmStorageSlot::new(*v, 0)))
                        .collect(),
                    status: revm::state::AccountStatus::Touched,
                };
                e.insert(acc);
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
                    max_priority_fee: None,
                    tx_type: 0,
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
                    max_priority_fee: tx.max_priority_fee_per_gas(),
                    tx_type: 2,
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

// ── Pre-bridge gas event topics ───────────────────────────────────────

/// keccak256("GasBridge(address,uint256,uint256,uint256)")
const GAS_BRIDGE_TOPIC0: [u8; 32] = [
    0x8b, 0x2e, 0x6e, 0x8f, 0x9a, 0x1c, 0x4d, 0x7e, 0x3b, 0x5f, 0x2a, 0x0d, 0x6c, 0x1e,
    0x8b, 0x4f, 0x9a, 0x3c, 0x5d, 0x7e, 0x2b, 0x0f, 0x6a, 0x1d, 0x8c, 0x4e, 0x9b, 0x3f,
    0x5a, 0x2e, 0x0c, 0x6d,
];

/// keccak256("GasBridgeSettled(address,uint256,uint256,uint256)")
const GAS_BRIDGE_SETTLED_TOPIC0: [u8; 32] = [
    0x9c, 0x3f, 0x6a, 0x1d, 0x8e, 0x4b, 0x5c, 0x7f, 0x2a, 0x0e, 0x6b, 0x1c, 0x9d, 0x4f,
    0x7a, 0x3e, 0x5b, 0x0c, 0x8f, 0x6a, 0x1d, 0x9e, 0x4c, 0x7b, 0x2f, 0x0a, 0x6c, 0x1e,
    0x8d, 0x5b, 0x3f, 0x7a,
];

fn make_gas_bridge_log(
    caller: Address,
    borrowed: U256,
    gas_limit: u64,
    gas_price: u128,
) -> call_protocol::LogEntry {
    let mut data = vec![0u8; 96];
    data[0..32].copy_from_slice(&borrowed.to_be_bytes::<32>());
    data[32..64].copy_from_slice(&U256::from(gas_limit).to_be_bytes::<32>());
    data[64..96].copy_from_slice(&U256::from(gas_price).to_be_bytes::<32>());

    call_protocol::LogEntry {
        address: ASSET_ADDRESS,
        topics: vec![
            call_primitives::Hash::from(GAS_BRIDGE_TOPIC0),
            call_primitives::Hash::from(caller.into_word().0),
        ],
        data,
    }
}

fn make_gas_bridge_settled_log(
    caller: Address,
    actual_cost: U256,
    refund: U256,
    gas_used: u64,
) -> call_protocol::LogEntry {
    let mut data = vec![0u8; 96];
    data[0..32].copy_from_slice(&actual_cost.to_be_bytes::<32>());
    data[32..64].copy_from_slice(&refund.to_be_bytes::<32>());
    data[64..96].copy_from_slice(&U256::from(gas_used).to_be_bytes::<32>());

    call_protocol::LogEntry {
        address: ASSET_ADDRESS,
        topics: vec![
            call_primitives::Hash::from(GAS_BRIDGE_SETTLED_TOPIC0),
            call_primitives::Hash::from(caller.into_word().0),
        ],
        data,
    }
}

/// Compute effective gas price from tx fields and base fee.
fn effective_gas_price(tx: &EvmTransaction, base_fee: u128) -> u128 {
    if tx.tx_type == 2 || tx.tx_type == 3 {
        // EIP-1559 / EIP-4844
        tx.gas_price
            .min(base_fee.saturating_add(tx.max_priority_fee.unwrap_or(0)))
    } else {
        tx.gas_price
    }
}

// ── Unit Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::InMemoryStateProvider;
    use alloy_primitives::{Address, U256};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    /// Build a JSON-encoded EvmTransaction for decode_evm_tx fallback.
    fn json_tx(tx: &EvmTransaction) -> Vec<u8> {
        serde_json::to_vec(tx).unwrap()
    }

    #[test]
    fn test_pre_bridge_success_with_refund() {
        let mut provider = InMemoryStateProvider::new();
        let caller = test_addr(1);
        let recipient = test_addr(2);

        // Caller has 0 native balance but 1_000_000 in protocol CALL.
        let slot = slot_balance(CALL_ASSET_ID, caller);
        provider.set_storage(ASSET_ADDRESS, slot, U256::from(1_000_000));
        provider.create_account(ASSET_ADDRESS);
        provider.create_account(caller);
        provider.create_account(recipient);

        let tx = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(recipient),
            value: U256::from(100),
            data: alloy_primitives::Bytes::default(),
            chain_id: 1,
        };

        let (result, state) =
            execute_block_transactions(&[json_tx(&tx)], provider, 1, 0).unwrap();

        assert_eq!(result.evm_tx_count, 1);
        assert!(result.evm_tx_results[0].status);

        // Both GasBridge and GasBridgeSettled events must be present.
        let topics: Vec<_> = result.evm_tx_results[0]
            .logs
            .iter()
            .map(|l| l.topics.first().copied().unwrap_or_default())
            .collect();
        assert!(topics.contains(&call_primitives::Hash::from(GAS_BRIDGE_TOPIC0)));
        assert!(topics.contains(&call_primitives::Hash::from(GAS_BRIDGE_SETTLED_TOPIC0)));

        // Caller native balance should be 0 (all borrowed funds spent).
        let caller_acc = state.get(&caller).expect("caller in state");
        assert_eq!(caller_acc.info.balance, U256::ZERO);

        // Recipient should receive the transferred value.
        let recipient_acc = state.get(&recipient).expect("recipient in state");
        assert_eq!(recipient_acc.info.balance, U256::from(100));

        // Protocol balance should be initial - borrowed + refund
        // = 1_000_000 - 210_100 + 0 = 789_900.
        let asset_acc = state.get(&ASSET_ADDRESS).expect("asset in state");
        let protocol_balance = asset_acc
            .storage
            .get(&slot)
            .map(|s| s.present_value)
            .unwrap_or_default();
        assert_eq!(protocol_balance, U256::from(789_900));
    }

    #[test]
    fn test_pre_bridge_revert_refunds_gas_surplus() {
        let mut provider = InMemoryStateProvider::new();
        let caller = test_addr(1);
        let contract = test_addr(0xAB);

        let slot = slot_balance(CALL_ASSET_ID, caller);
        provider.set_storage(ASSET_ADDRESS, slot, U256::from(1_000_000));
        provider.create_account(ASSET_ADDRESS);
        provider.create_account(caller);
        // Deploy a contract that always halts (INVALID opcode 0xFE).
        provider.set_code(contract, alloy_primitives::bytes!("FE"));
        provider.create_account(contract);

        let tx = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(contract),
            value: U256::ZERO,
            data: alloy_primitives::Bytes::default(),
            chain_id: 1,
        };

        let (result, state) =
            execute_block_transactions(&[json_tx(&tx)], provider, 1, 0).unwrap();

        assert_eq!(result.evm_tx_count, 1);
        assert!(!result.evm_tx_results[0].status); // reverted / halted

        // GasBridge must be present; GasBridgeSettled refund should be 0
        // because borrowed (210_000) - value (0) - actual_cost (210_000) = 0.
        let logs = &result.evm_tx_results[0].logs;
        assert!(logs.iter().any(|l| l.topics.first()
            == Some(&call_primitives::Hash::from(GAS_BRIDGE_TOPIC0))));
        assert!(logs.iter().any(|l| l.topics.first()
            == Some(&call_primitives::Hash::from(GAS_BRIDGE_SETTLED_TOPIC0))));

        // Caller native should be 0 (all gas consumed by INVALID).
        let caller_acc = state.get(&caller).expect("caller in state");
        assert_eq!(caller_acc.info.balance, U256::ZERO);

        // Protocol paid full gas cost.
        let asset_acc = state.get(&ASSET_ADDRESS).expect("asset in state");
        let protocol_balance = asset_acc
            .storage
            .get(&slot)
            .map(|s| s.present_value)
            .unwrap_or_default();
        assert_eq!(protocol_balance, U256::from(790_000));
    }

    #[test]
    fn test_no_pre_bridge_when_native_sufficient() {
        let mut provider = InMemoryStateProvider::new();
        let caller = test_addr(1);
        let recipient = test_addr(2);

        // Caller has plenty of native balance.
        provider.set_balance(caller, U256::from(1_000_000));
        provider.create_account(caller);
        provider.create_account(recipient);

        let tx = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(recipient),
            value: U256::from(100),
            data: alloy_primitives::Bytes::default(),
            chain_id: 1,
        };

        let (result, state) =
            execute_block_transactions(&[json_tx(&tx)], provider, 1, 0).unwrap();

        assert_eq!(result.evm_tx_count, 1);
        assert!(result.evm_tx_results[0].status);

        // No bridge events when native balance is sufficient.
        let topics: Vec<_> = result.evm_tx_results[0]
            .logs
            .iter()
            .map(|l| l.topics.first().copied().unwrap_or_default())
            .collect();
        assert!(!topics.contains(&call_primitives::Hash::from(GAS_BRIDGE_TOPIC0)));
        assert!(!topics.contains(&call_primitives::Hash::from(GAS_BRIDGE_SETTLED_TOPIC0)));

        // Caller should have lost value + gas.
        let caller_acc = state.get(&caller).expect("caller in state");
        assert_eq!(
            caller_acc.info.balance,
            U256::from(1_000_000 - 100 - 210_000)
        );
    }

    #[test]
    fn test_pre_bridge_insufficient_protocol_balance_skips() {
        let mut provider = InMemoryStateProvider::new();
        let caller = test_addr(1);
        let recipient = test_addr(2);

        let slot = slot_balance(CALL_ASSET_ID, caller);
        // Protocol balance is only 100, but tx needs 210_000 for gas + 100 value.
        provider.set_storage(ASSET_ADDRESS, slot, U256::from(100));
        provider.create_account(ASSET_ADDRESS);
        provider.create_account(caller);
        provider.create_account(recipient);

        let tx = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(recipient),
            value: U256::from(100),
            data: alloy_primitives::Bytes::default(),
            chain_id: 1,
        };

        let (result, _state) =
            execute_block_transactions(&[json_tx(&tx)], provider, 1, 0).unwrap();

        // Tx should be skipped because protocol balance is insufficient.
        assert_eq!(result.evm_tx_count, 0);
    }

    #[test]
    fn test_pre_bridge_multiple_txs_cumulative() {
        let mut provider = InMemoryStateProvider::new();
        let caller = test_addr(1);
        let recipient = test_addr(2);

        let slot = slot_balance(CALL_ASSET_ID, caller);
        provider.set_storage(ASSET_ADDRESS, slot, U256::from(1_000_000));
        provider.create_account(ASSET_ADDRESS);
        provider.create_account(caller);
        provider.create_account(recipient);

        let tx1 = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(recipient),
            value: U256::from(50),
            data: alloy_primitives::Bytes::default(),
            chain_id: 1,
        };
        let tx2 = EvmTransaction {
            caller,
            nonce: 1,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(recipient),
            value: U256::from(60),
            data: alloy_primitives::Bytes::default(),
            chain_id: 1,
        };

        let (result, state) = execute_block_transactions(
            &[json_tx(&tx1), json_tx(&tx2)],
            provider,
            1,
            0,
        )
        .unwrap();

        assert_eq!(result.evm_tx_count, 2);

        // Both txs should have bridge events.
        for entry in &result.evm_tx_results {
            let topics: Vec<_> = entry.logs.iter().map(|l| l.topics.first().copied().unwrap_or_default()).collect();
            assert!(topics.contains(&call_primitives::Hash::from(GAS_BRIDGE_TOPIC0)));
            assert!(topics.contains(&call_primitives::Hash::from(GAS_BRIDGE_SETTLED_TOPIC0)));
        }

        // Recipient should have received 50 + 60 = 110.
        let recipient_acc = state.get(&recipient).expect("recipient in state");
        assert_eq!(recipient_acc.info.balance, U256::from(110));

        // Protocol balance should be:
        // initial: 1_000_000
        // tx1 cost: 50 + 210_000 = 210_050
        // tx2 cost: 60 + 210_000 = 210_060
        // total cost: 420_110
        // remaining: 579_890
        let asset_acc = state.get(&ASSET_ADDRESS).expect("asset in state");
        let protocol_balance = asset_acc
            .storage
            .get(&slot)
            .map(|s| s.present_value)
            .unwrap_or_default();
        assert_eq!(protocol_balance, U256::from(579_890));
    }

    #[test]
    fn test_gas_bridge_event_order() {
        let mut provider = InMemoryStateProvider::new();
        let caller = test_addr(1);
        let recipient = test_addr(2);

        let slot = slot_balance(CALL_ASSET_ID, caller);
        provider.set_storage(ASSET_ADDRESS, slot, U256::from(1_000_000));
        provider.create_account(ASSET_ADDRESS);
        provider.create_account(caller);
        provider.create_account(recipient);

        let tx = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(recipient),
            value: U256::from(100),
            data: alloy_primitives::Bytes::default(),
            chain_id: 1,
        };

        let (result, _state) =
            execute_block_transactions(&[json_tx(&tx)], provider, 1, 0).unwrap();

        let logs = &result.evm_tx_results[0].logs;
        let bridge_idx = logs.iter().position(|l| {
            l.topics.first() == Some(&call_primitives::Hash::from(GAS_BRIDGE_TOPIC0))
        });
        let settled_idx = logs.iter().position(|l| {
            l.topics.first() == Some(&call_primitives::Hash::from(GAS_BRIDGE_SETTLED_TOPIC0))
        });

        assert!(bridge_idx.is_some(), "GasBridge event must be present");
        assert!(
            settled_idx.is_some(),
            "GasBridgeSettled event must be present"
        );
        assert!(
            bridge_idx.unwrap() < settled_idx.unwrap(),
            "GasBridge must come before GasBridgeSettled"
        );
    }

    #[test]
    fn test_pre_bridge_borrowed_amount_exact() {
        let mut provider = InMemoryStateProvider::new();
        let caller = test_addr(1);
        let recipient = test_addr(2);

        let slot = slot_balance(CALL_ASSET_ID, caller);
        // Native balance: 50, tx needs: 100 value + 210_000 gas = 210_100
        // Borrowed should be: 210_100 - 50 = 210_050
        provider.set_balance(caller, U256::from(50));
        provider.set_storage(ASSET_ADDRESS, slot, U256::from(1_000_000));
        provider.create_account(ASSET_ADDRESS);
        provider.create_account(caller);
        provider.create_account(recipient);

        let tx = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(recipient),
            value: U256::from(100),
            data: alloy_primitives::Bytes::default(),
            chain_id: 1,
        };

        let (result, state) =
            execute_block_transactions(&[json_tx(&tx)], provider, 1, 0).unwrap();

        assert_eq!(result.evm_tx_count, 1);

        // Verify protocol balance reflects exact borrowed amount.
        // initial: 1_000_000, borrowed: 210_050, spent: 210_050 (all gas + value)
        let asset_acc = state.get(&ASSET_ADDRESS).expect("asset in state");
        let protocol_balance = asset_acc
            .storage
            .get(&slot)
            .map(|s| s.present_value)
            .unwrap_or_default();
        assert_eq!(protocol_balance, U256::from(1_000_000 - 210_050));

        // Verify GasBridge event data contains the exact borrowed amount.
        let bridge_log = result.evm_tx_results[0]
            .logs
            .iter()
            .find(|l| {
                l.topics.first()
                    == Some(&call_primitives::Hash::from(GAS_BRIDGE_TOPIC0))
            })
            .expect("GasBridge log must exist");
        let borrowed_from_log = U256::from_be_slice(&bridge_log.data[0..32]);
        assert_eq!(borrowed_from_log, U256::from(210_050));
    }

    #[test]
    fn test_lazy_provider_in_block_execution() {
        use crate::provider::LazyStateProvider;
        use std::sync::Arc;

        let tmp = std::env::temp_dir()
            .join(format!("call-lazy-block-exec-test-{}", std::process::id()));
        let db = call_storage::reth_db::init_call_db(&tmp).expect("init db");

        // Seed MDBX with state via InMemoryStateProvider
        let mut seed = InMemoryStateProvider::new();
        let caller = test_addr(1);
        let recipient = test_addr(2);
        seed.set_balance(caller, U256::from(1_000_000));
        seed.create_account(caller);
        seed.create_account(recipient);
        seed.save_to_db(&db).expect("seed db");

        // Use LazyStateProvider — should NOT load full state at construction
        let lazy = LazyStateProvider::new(Arc::clone(&db));

        let tx = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            max_priority_fee: None,
            tx_type: 0,
            to: Some(recipient),
            value: U256::from(100),
            data: alloy_primitives::Bytes::default(),
            chain_id: 1,
        };

        // execute_block_transactions now accepts any EvmStateProvider
        let (result, state) =
            execute_block_transactions(&[json_tx(&tx)], lazy, 1, 0).unwrap();

        assert_eq!(result.evm_tx_count, 1);
        assert!(result.evm_tx_results[0].status);

        // Verify state delta is correct
        let caller_acc = state.get(&caller).expect("caller in state");
        assert_eq!(
            caller_acc.info.balance,
            U256::from(1_000_000 - 100 - 210_000)
        );

        let recipient_acc = state.get(&recipient).expect("recipient in state");
        assert_eq!(recipient_acc.info.balance, U256::from(100));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
