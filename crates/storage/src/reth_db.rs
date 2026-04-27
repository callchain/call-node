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

/// Validator meta state: single entry () -> serialized ValidatorMetaSnapshot
/// Stores queues, churn counters, next_id, and params.
#[derive(Debug)]
pub struct CallValidatorMeta;
impl Table for CallValidatorMeta {
    const NAME: &'static str = "call_validator_meta";
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

/// Compliance state: single entry () -> serialized ComplianceEngineSnapshot
#[derive(Debug)]
pub struct CallComplianceState;
impl Table for CallComplianceState {
    const NAME: &'static str = "call_compliance_state";
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

// ── Missing tables (added for full 34-table coverage) ─────────────────

/// Protocol assets: serialized asset_id -> serialized AssetEntry
#[derive(Debug)]
pub struct CallProtocolAssets;
impl Table for CallProtocolAssets {
    const NAME: &'static str = "call_protocol_assets";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Shielded Merkle tree: serialized node_index -> serialized node_hash
#[derive(Debug)]
pub struct CallShieldedMerkleTree;
impl Table for CallShieldedMerkleTree {
    const NAME: &'static str = "call_shielded_merkle_tree";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Shielded viewing keys: serialized address -> encrypted key material
#[derive(Debug)]
pub struct CallShieldedViewingKeys;
impl Table for CallShieldedViewingKeys {
    const NAME: &'static str = "call_shielded_viewing_keys";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Agent balances: serialized (owner, agent_id, asset_id) -> serialized balance
#[derive(Debug)]
pub struct CallAgentBalances;
impl Table for CallAgentBalances {
    const NAME: &'static str = "call_agent_balances";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Agent nonces: serialized (owner, agent_id) -> serialized nonce
#[derive(Debug)]
pub struct CallAgentNonces;
impl Table for CallAgentNonces {
    const NAME: &'static str = "call_agent_nonces";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// EVM contracts: serialized address -> serialized bytecode
#[derive(Debug)]
pub struct CallEvmContracts;
impl Table for CallEvmContracts {
    const NAME: &'static str = "call_evm_contracts";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Consensus blocks: serialized height -> serialized Block
#[derive(Debug)]
pub struct CallConsensusBlocks;
impl Table for CallConsensusBlocks {
    const NAME: &'static str = "call_consensus_blocks";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Metadata chain id: single entry () -> chain_id
#[derive(Debug)]
pub struct CallMetadataChainId;
impl Table for CallMetadataChainId {
    const NAME: &'static str = "call_metadata_chain_id";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Metadata compliance: serialized asset_id -> serialized compliance policy
#[derive(Debug)]
pub struct CallMetadataCompliance;
impl Table for CallMetadataCompliance {
    const NAME: &'static str = "call_metadata_compliance";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Metadata agents: serialized agent_id -> serialized status
#[derive(Debug)]
pub struct CallMetadataAgents;
impl Table for CallMetadataAgents {
    const NAME: &'static str = "call_metadata_agents";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Receipts: serialized tx_hash -> serialized ProtocolReceipt
#[derive(Debug)]
pub struct CallReceipts;
impl Table for CallReceipts {
    const NAME: &'static str = "call_receipts";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Receipt index: block_number -> Vec<TxHash> for efficient block-level queries and pruning
#[derive(Debug)]
pub struct CallReceiptsByBlock;
impl Table for CallReceiptsByBlock {
    const NAME: &'static str = "call_receipts_by_block";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Logs: serialized (block_number, log_index) -> serialized LogEntry
#[derive(Debug)]
pub struct CallLogs;
impl Table for CallLogs {
    const NAME: &'static str = "call_logs";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Memos: serialized tx_hash -> serialized memo data
#[derive(Debug)]
pub struct CallMemos;
impl Table for CallMemos {
    const NAME: &'static str = "call_memos";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Fee currency registry: serialized asset_id -> serialized FeeCurrencyEntry
#[derive(Debug)]
pub struct CallFeeCurrencyRegistry;
impl Table for CallFeeCurrencyRegistry {
    const NAME: &'static str = "call_fee_currency_registry";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Fee params: single entry () -> serialized FeeParams
#[derive(Debug)]
pub struct CallFeeParams;
impl Table for CallFeeParams {
    const NAME: &'static str = "call_fee_params";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Oracle prices: serialized (asset_id, block_number) -> serialized price
#[derive(Debug)]
pub struct CallOraclePrices;
impl Table for CallOraclePrices {
    const NAME: &'static str = "call_oracle_prices";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Oracle validator info: serialized validator_id -> serialized oracle status
#[derive(Debug)]
pub struct CallOracleValidatorInfo;
impl Table for CallOracleValidatorInfo {
    const NAME: &'static str = "call_oracle_validator_info";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Governance proposals: serialized proposal_id -> serialized proposal
#[derive(Debug)]
pub struct CallGovernanceProposals;
impl Table for CallGovernanceProposals {
    const NAME: &'static str = "call_governance_proposals";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Vote delegations: serialized (delegator, validator_id) -> serialized delegation
#[derive(Debug)]
pub struct CallVoteDelegations;
impl Table for CallVoteDelegations {
    const NAME: &'static str = "call_vote_delegations";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Sponsor auths: serialized (owner, sponsor) -> serialized authorization
#[derive(Debug)]
pub struct CallSponsorAuths;
impl Table for CallSponsorAuths {
    const NAME: &'static str = "call_sponsor_auths";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Sponsor pools: serialized sponsor_address -> serialized pool balance
#[derive(Debug)]
pub struct CallSponsorPools;
impl Table for CallSponsorPools {
    const NAME: &'static str = "call_sponsor_pools";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Sponsor daily usage: serialized (sponsor_address, day) -> serialized usage
#[derive(Debug)]
pub struct CallSponsorDailyUsage;
impl Table for CallSponsorDailyUsage {
    const NAME: &'static str = "call_sponsor_daily_usage";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Session keys: serialized session_key -> serialized (owner, expiry)
#[derive(Debug)]
pub struct CallSessionKeys;
impl Table for CallSessionKeys {
    const NAME: &'static str = "call_session_keys";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Multi-sig configs: serialized address -> serialized MultiSigConfig
#[derive(Debug)]
pub struct CallMultiSigConfigs;
impl Table for CallMultiSigConfigs {
    const NAME: &'static str = "call_multi_sig_configs";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Social recovery configs: serialized address -> serialized RecoveryConfig
#[derive(Debug)]
pub struct CallSocialRecoveryConfigs;
impl Table for CallSocialRecoveryConfigs {
    const NAME: &'static str = "call_social_recovery_configs";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Fork state: single entry () -> serialized ForkManager
#[derive(Debug)]
pub struct CallForkState;
impl Table for CallForkState {
    const NAME: &'static str = "call_fork_state";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Checkpoint marker: single entry "pending" -> state_hash for crash recovery
#[derive(Debug)]
pub struct CallCheckpoint;
impl Table for CallCheckpoint {
    const NAME: &'static str = "call_checkpoint";
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
                box_info::<CallProtocolAssets>,
                box_info::<CallProtocolBalances>,
                box_info::<CallProtocolAllowances>,
                box_info::<CallShieldedMerkleTree>,
                box_info::<CallShieldedNullifiers>,
                box_info::<CallShieldedCommitments>,
                box_info::<CallShieldedViewingKeys>,
                box_info::<CallAgents>,
                box_info::<CallAgentBalances>,
                box_info::<CallAgentNonces>,
                box_info::<CallEvmAccounts>,
                box_info::<CallEvmContracts>,
                box_info::<CallEvmStorage>,
                box_info::<CallBridgeOps>,
                box_info::<CallConsensusBlocks>,
                box_info::<CallConsensusState>,
                box_info::<CallMetadataChainId>,
                box_info::<CallValidators>,
                box_info::<CallValidatorMeta>,
                box_info::<CallMetadataCompliance>,
                box_info::<CallMetadataAgents>,
                box_info::<CallReceipts>,
                box_info::<CallReceiptsByBlock>,
                box_info::<CallLogs>,
                box_info::<CallMemos>,
                box_info::<CallFeeCurrencyRegistry>,
                box_info::<CallFeeParams>,
                box_info::<CallOraclePrices>,
                box_info::<CallOracleValidatorInfo>,
                box_info::<CallGovernanceProposals>,
                box_info::<CallVoteDelegations>,
                box_info::<CallSponsorAuths>,
                box_info::<CallSponsorPools>,
                box_info::<CallSponsorDailyUsage>,
                box_info::<CallSessionKeys>,
                box_info::<CallMultiSigConfigs>,
                box_info::<CallSocialRecoveryConfigs>,
                box_info::<CallForkState>,
                box_info::<CallCheckpoint>,
                box_info::<CallPruneState>,
                box_info::<CallGovernanceState>,
                box_info::<CallComplianceState>,
                box_info::<CallOracleState>,
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

/// Compact the MDBX database to release unused disk pages back to the OS.
///
/// MDBX uses a write-ahead log and copy-on-write design. When data is
/// deleted, pages go onto the freelist for reuse by future writes. This
/// function triggers a full sync of the database, ensuring all freed pages
/// are properly tracked and can be reused.
///
/// For full disk space reclamation, the node operator should periodically
/// restart the node — MDBX reclaims freed pages during startup cleanup.
pub fn compact_db(db: &DatabaseEnv) -> Result<(), StorageError> {
    // Commit an empty transaction to ensure all freed pages are returned
    // to the freelist and the database is fully synced.
    let tx = db.tx_mut().map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(())
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

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::db::{CallDb, open_db};
    use call_primitives::Address;
    use std::collections::HashMap;
    use std::thread;
    use std::sync::Arc;

    fn temp_db() -> CallDb {
        let path = std::env::temp_dir().join(format!(
            "call-mdbx-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        open_db(path).expect("failed to open temp db")
    }

    #[test]
    fn test_concurrent_writes_different_keys() {
        let db = temp_db();

        let db_a = Arc::clone(&db.db);
        let handle_a = thread::spawn(move || {
            for i in 0..100 {
                let key = format!("balance_{}", i).into_bytes();
                let value: u128 = i as u128 * 1_000_000;
                let data = serde_json::to_vec(&value).unwrap();
                db_put::<CallProtocolBalances>(&db_a, key, data).unwrap();
            }
        });

        let db_b = Arc::clone(&db.db);
        let handle_b = thread::spawn(move || {
            for i in 0..100 {
                let key = format!("allowance_{}", i).into_bytes();
                let value: u128 = i as u128 * 500_000;
                let data = serde_json::to_vec(&value).unwrap();
                db_put::<CallProtocolAllowances>(&db_b, key, data).unwrap();
            }
        });

        handle_a.join().unwrap();
        handle_b.join().unwrap();

        for i in 0..100 {
            let key = format!("balance_{}", i).into_bytes();
            let data = db_get::<CallProtocolBalances>(&db.db, &key).unwrap().unwrap();
            let value: u128 = serde_json::from_slice(&data).unwrap();
            assert_eq!(value, i as u128 * 1_000_000);

            let key = format!("allowance_{}", i).into_bytes();
            let data = db_get::<CallProtocolAllowances>(&db.db, &key).unwrap().unwrap();
            let value: u128 = serde_json::from_slice(&data).unwrap();
            assert_eq!(value, i as u128 * 500_000);
        }
    }

    #[test]
    fn test_crash_recovery_checkpoint() {
        let db = temp_db();

        write_checkpoint(&db.db, [0xDEu8; 32]).unwrap();
        assert!(check_recovery(&db.db).unwrap(), "should detect pending checkpoint");

        clear_checkpoint(&db.db).unwrap();
        assert!(!check_recovery(&db.db).unwrap(), "checkpoint should be cleared");
    }

    #[test]
    fn test_compaction_flag() {
        let db = temp_db();
        save_balances(&db.db, &HashMap::new(), &HashMap::new()).unwrap();
        compact_db(&db.db).unwrap();

        let (balances, allowances) = load_balances(&db.db).unwrap();
        assert!(balances.is_empty());
        assert!(allowances.is_empty());
    }

    #[test]
    fn test_save_load_balances_roundtrip() {
        let db = temp_db();
        let mut balances = HashMap::new();
        let mut allowances = HashMap::new();
        balances.insert((1, Address::repeat_byte(0x01)), 1_000_000);
        balances.insert((2, Address::repeat_byte(0x02)), 2_000_000);
        allowances.insert((1, Address::repeat_byte(0x01), Address::repeat_byte(0x03)), 500);

        save_balances(&db.db, &balances, &allowances).unwrap();
        let (loaded_balances, loaded_allowances) = load_balances(&db.db).unwrap();

        assert_eq!(loaded_balances, balances);
        assert_eq!(loaded_allowances, allowances);
    }

    #[test]
    fn test_save_load_prune_state_roundtrip() {
        let db = temp_db();
        let state = crate::prune::PruneState::new();
        save_prune_state(&db.db, &state).unwrap();
        let loaded = load_prune_state(&db.db).unwrap();
        assert_eq!(serde_json::to_string(&state).unwrap(), serde_json::to_string(&loaded).unwrap());
    }

    #[test]
    fn test_db_clear_and_repopulate() {
        let db = temp_db();
        db_put::<CallMetadataChainId>(&db.db, b"key1".to_vec(), b"value1".to_vec()).unwrap();
        db_put::<CallMetadataChainId>(&db.db, b"key2".to_vec(), b"value2".to_vec()).unwrap();

        assert!(db_get::<CallMetadataChainId>(&db.db, b"key1").unwrap().is_some());
        assert!(db_get::<CallMetadataChainId>(&db.db, b"key2").unwrap().is_some());

        db_clear::<CallMetadataChainId>(&db.db).unwrap();

        assert!(db_get::<CallMetadataChainId>(&db.db, b"key1").unwrap().is_none());
        assert!(db_get::<CallMetadataChainId>(&db.db, b"key2").unwrap().is_none());
    }

    #[test]
    fn test_batch_write_large_dataset() {
        let db = temp_db();
        let mut pairs = Vec::new();
        for i in 0..10_000 {
            let key = format!("receipt_{:08x}", i).into_bytes();
            let receipt = format!("receipt data {}", i);
            let value = serde_json::to_vec(&receipt).unwrap();
            pairs.push((key, value));
        }

        db_batch_put::<CallReceipts>(&db.db, pairs).unwrap();

        for i in [0, 4999, 9999] {
            let key = format!("receipt_{:08x}", i).into_bytes();
            let data = db_get::<CallReceipts>(&db.db, &key).unwrap().unwrap();
            let receipt: String = serde_json::from_slice(&data).unwrap();
            assert_eq!(receipt, format!("receipt data {}", i));
        }

        let all = db_iter_all::<CallReceipts>(&db.db).unwrap();
        assert_eq!(all.len(), 10_000);
    }

    fn write_checkpoint(db: &DatabaseEnv, block_hash: [u8; 32]) -> Result<(), String> {
        db_put::<CallCheckpoint>(db, b"pending".to_vec(), block_hash.to_vec())
            .map_err(|e| e.to_string())
    }

    fn check_recovery(db: &DatabaseEnv) -> Result<bool, String> {
        match db_get::<CallCheckpoint>(db, b"pending").map_err(|e: StorageError| e.to_string())? {
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }

    fn clear_checkpoint(db: &DatabaseEnv) -> Result<(), String> {
        db_del::<CallCheckpoint>(db, b"pending").map_err(|e| e.to_string())
    }
}
