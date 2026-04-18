//! reth-db (MDBX) integration for Callchain state persistence.
//!
//! Defines custom database tables and provides initialization/persistence
//! for all state types (balances, EVM, bridge, shielded, agents, validators).
//!
//! Uses raw byte keys/values with serde_json serialization to avoid
//! the complexity of reth-codecs trait implementations for every type.
//!
//! Generic CRUD helpers are provided here. Type-specific save/load functions
//! live in the `call-node` crate to avoid cyclic dependencies.

use std::path::Path;
use std::sync::Arc;

use reth_db::mdbx::{init_db_for, DatabaseArguments};
use reth_db::DatabaseEnv;
use reth_db_api::table::{Table, TableInfo};
use reth_db_api::{TableSet, DatabaseError};
use reth_db_api::database::Database;
use reth_db_api::transaction::{DbTx, DbTxMut};
use reth_db::cursor::DbCursorRW;
use reth_db_api::cursor::DbCursorRO;

use crate::StorageError;

// ── Custom Table Definitions ──────────────────────────────────────────

/// Protocol balances: serialized (asset_id, address) -> serialized balance
#[derive(Debug)]
pub struct CallProtocolBalances;
impl Table for CallProtocolBalances {
    const NAME: &'static str = "call_protocol_balances";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Protocol allowances: serialized (asset_id, owner, spender) -> serialized allowance
#[derive(Debug)]
pub struct CallProtocolAllowances;
impl Table for CallProtocolAllowances {
    const NAME: &'static str = "call_protocol_allowances";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// EVM accounts: serialized Address -> serialized EvmAccount
#[derive(Debug)]
pub struct CallEvmAccounts;
impl Table for CallEvmAccounts {
    const NAME: &'static str = "call_evm_accounts";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// EVM storage slots: serialized (address, slot) -> serialized value
#[derive(Debug)]
pub struct CallEvmStorage;
impl Table for CallEvmStorage {
    const NAME: &'static str = "call_evm_storage";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Bridge pending ops: serialized op_id -> serialized PendingBridgeOp
#[derive(Debug)]
pub struct CallBridgeOps;
impl Table for CallBridgeOps {
    const NAME: &'static str = "call_bridge_ops";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Shielded nullifiers: serialized Hash -> ()
#[derive(Debug)]
pub struct CallShieldedNullifiers;
impl Table for CallShieldedNullifiers {
    const NAME: &'static str = "call_shielded_nullifiers";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Shielded commitments: serialized NoteCommitment -> serialized Note
#[derive(Debug)]
pub struct CallShieldedCommitments;
impl Table for CallShieldedCommitments {
    const NAME: &'static str = "call_shielded_commitments";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Validator set: serialized ValidatorId -> serialized ValidatorStake
#[derive(Debug)]
pub struct CallValidators;
impl Table for CallValidators {
    const NAME: &'static str = "call_validators";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Agent registrations: serialized agent_id -> serialized AgentRegistration
#[derive(Debug)]
pub struct CallAgents;
impl Table for CallAgents {
    const NAME: &'static str = "call_agents";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Oracle state: single entry () -> serialized OracleManager
#[derive(Debug)]
pub struct CallOracleState;
impl Table for CallOracleState {
    const NAME: &'static str = "call_oracle_state";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Prune state: single entry () -> serialized PruneState
#[derive(Debug)]
pub struct CallPruneState;
impl Table for CallPruneState {
    const NAME: &'static str = "call_prune_state";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Governance state: single entry () -> serialized GovernanceManager snapshot
#[derive(Debug)]
pub struct CallGovernanceState;
impl Table for CallGovernanceState {
    const NAME: &'static str = "call_governance_state";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Consensus state: single entry () -> serialized PersistedConsensusState
#[derive(Debug)]
pub struct CallConsensusState;
impl Table for CallConsensusState {
    const NAME: &'static str = "call_consensus_state";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// All Callchain tables
pub struct CallTables;
impl TableSet for CallTables {
    fn tables() -> Box<dyn Iterator<Item = Box<dyn TableInfo>>> {
        fn box_info<T: Table>() -> Box<dyn TableInfo> {
            Box::new(TableInfoWrapper(std::marker::PhantomData::<T>))
        }
        Box::new(
            [
                box_info::<CallProtocolBalances>,
                box_info::<CallProtocolAllowances>,
                box_info::<CallEvmAccounts>,
                box_info::<CallEvmStorage>,
                box_info::<CallBridgeOps>,
                box_info::<CallShieldedNullifiers>,
                box_info::<CallShieldedCommitments>,
                box_info::<CallValidators>,
                box_info::<CallAgents>,
                box_info::<CallOracleState>,
                box_info::<CallPruneState>,
                box_info::<CallGovernanceState>,
                box_info::<CallConsensusState>,
            ]
            .into_iter()
            .map(|f| f()),
        )
    }
}

/// Wrapper to make table types implement TableInfo
#[derive(Debug)]
struct TableInfoWrapper<T>(std::marker::PhantomData<T>);
impl<T: Table> TableInfo for TableInfoWrapper<T> {
    fn name(&self) -> &'static str {
        <T as Table>::NAME
    }
    fn is_dupsort(&self) -> bool {
        <T as Table>::DUPSORT
    }
}

// ── Database Initialization ───────────────────────────────────────────

/// Initialize or open the Callchain MDBX database at the given path.
pub fn init_call_db(data_dir: &Path) -> Result<Arc<DatabaseEnv>, StorageError> {
    let db_path = data_dir.join("mdbx");
    let args = DatabaseArguments::default();
    let db = init_db_for::<_, CallTables>(&db_path, args)
        .map_err(|e| StorageError::Database(e.to_string()))?;
    Ok(Arc::new(db))
}

// ── Helper Functions ──────────────────────────────────────────────────

fn db_err(e: DatabaseError) -> StorageError {
    StorageError::Database(e.to_string())
}

/// Write a key-value pair to a table within a transaction.
pub fn db_put<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(
    db: &DatabaseEnv,
    key: Vec<u8>,
    value: Vec<u8>,
) -> Result<(), StorageError> {
    let tx = db.tx_mut().map_err(db_err)?;
    let mut cursor = tx.cursor_write::<T>().map_err(db_err)?;
    cursor.upsert(key, &value).map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(())
}

/// Read a value from a table by key within a transaction.
pub fn db_get<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(db: &DatabaseEnv, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
    let tx = db.tx().map_err(db_err)?;
    let mut cursor = tx.cursor_read::<T>().map_err(db_err)?;
    let value = cursor.seek_exact(key.to_vec()).map_err(db_err)?;
    Ok(value.map(|(_, v)| v))
}

/// Delete a key from a table within a transaction.
pub fn db_del<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(db: &DatabaseEnv, key: &[u8]) -> Result<(), StorageError> {
    let tx = db.tx_mut().map_err(db_err)?;
    let mut cursor = tx.cursor_write::<T>().map_err(db_err)?;
    if cursor.seek_exact(key.to_vec()).map_err(db_err)?.is_some() {
        cursor.delete_current().map_err(db_err)?;
    }
    tx.commit().map_err(db_err)?;
    Ok(())
}

/// Iterate all key-value pairs in a table, collecting them.
pub fn db_iter_all<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(db: &DatabaseEnv) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
    let tx = db.tx().map_err(db_err)?;
    let mut cursor = tx.cursor_read::<T>().map_err(db_err)?;
    let mut results = Vec::new();
    let walker = cursor.walk(None).map_err(db_err)?;
    for entry in walker {
        let (k, v) = entry.map_err(db_err)?;
        results.push((k, v));
    }
    Ok(results)
}

/// Batch write multiple key-value pairs to a table in a single transaction.
pub fn db_batch_put<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(
    db: &DatabaseEnv,
    entries: Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<(), StorageError> {
    if entries.is_empty() {
        return Ok(());
    }
    let tx = db.tx_mut().map_err(db_err)?;
    let mut cursor = tx.cursor_write::<T>().map_err(db_err)?;
    for (key, value) in entries {
        cursor.upsert(key, &value).map_err(db_err)?;
    }
    tx.commit().map_err(db_err)?;
    Ok(())
}

/// Clear all entries in a table (used before full state reload).
pub fn db_clear<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(db: &DatabaseEnv) -> Result<(), StorageError> {
    let tx = db.tx_mut().map_err(db_err)?;
    let mut cursor = tx.cursor_write::<T>().map_err(db_err)?;
    while cursor.first().map_err(db_err)?.is_some() {
        cursor.delete_current().map_err(db_err)?;
    }
    tx.commit().map_err(db_err)?;
    Ok(())
}

// ── Convenience Methods for Each Table ────────────────────────────────

/// Save all protocol balances to the database (replaces entire table).
pub fn save_balances(
    db: &DatabaseEnv,
    balances: &std::collections::HashMap<(call_primitives::AssetId, call_primitives::Address), u128>,
    allowances: &std::collections::HashMap<(call_primitives::AssetId, call_primitives::Address, call_primitives::Address), u128>,
) -> Result<(), StorageError> {
    let balance_entries: Vec<(Vec<u8>, Vec<u8>)> = balances
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    let allowance_entries: Vec<(Vec<u8>, Vec<u8>)> = allowances
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();

    // Clear and repopulate in a single batch
    db_clear::<CallProtocolBalances>(db)?;
    db_batch_put::<CallProtocolBalances>(db, balance_entries)?;
    db_clear::<CallProtocolAllowances>(db)?;
    db_batch_put::<CallProtocolAllowances>(db, allowance_entries)?;
    Ok(())
}

/// Load all protocol balances from the database.
pub fn load_balances(
    db: &DatabaseEnv,
) -> Result<(
    std::collections::HashMap<(call_primitives::AssetId, call_primitives::Address), u128>,
    std::collections::HashMap<(call_primitives::AssetId, call_primitives::Address, call_primitives::Address), u128>,
), StorageError> {
    let balance_data = db_iter_all::<CallProtocolBalances>(db)?;
    let allowance_data = db_iter_all::<CallProtocolAllowances>(db)?;

    let balances = balance_data
        .into_iter()
        .map(|(k, v)| {
            let key: (call_primitives::AssetId, call_primitives::Address) = serde_json::from_slice(&k).unwrap();
            let value: u128 = serde_json::from_slice(&v).unwrap();
            (key, value)
        })
        .collect();
    let allowances = allowance_data
        .into_iter()
        .map(|(k, v)| {
            let key: (call_primitives::AssetId, call_primitives::Address, call_primitives::Address) =
                serde_json::from_slice(&k).unwrap();
            let value: u128 = serde_json::from_slice(&v).unwrap();
            (key, value)
        })
        .collect();
    Ok((balances, allowances))
}

/// Save prune state to the database.
pub fn save_prune_state(db: &DatabaseEnv, state: &crate::prune::PruneState) -> Result<(), StorageError> {
    let data = serde_json::to_vec(state).map_err(|e| StorageError::Serialization(e.to_string()))?;
    db_put::<CallPruneState>(db, vec![0], data)
}

/// Load prune state from the database.
pub fn load_prune_state(db: &DatabaseEnv) -> Result<crate::prune::PruneState, StorageError> {
    match db_get::<CallPruneState>(db, &[0])? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| StorageError::Serialization(e.to_string())),
        None => Ok(crate::prune::PruneState::new()),
    }
}
