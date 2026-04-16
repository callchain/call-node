//! Database initialization with reth-db (MDBX) persistence.
//!
//! Uses reth-db as the primary persistence backend for all state types.
//! JSON file fallback is retained for testing scenarios without MDBX.

use std::path::PathBuf;
use std::sync::Arc;

use reth_db::DatabaseEnv;

use crate::StorageError;
use crate::prune::PruneState;
use crate::reth_db::{init_call_db, load_prune_state as db_load_prune, save_prune_state as db_save_prune};

/// Database handle with reth-db persistence.
#[derive(Clone)]
pub struct CallDb {
    pub data_dir: PathBuf,
    /// Directory for state snapshots and persistence files (JSON fallback)
    pub state_dir: PathBuf,
    /// Directory for prune state persistence (JSON fallback)
    pub prune_dir: PathBuf,
    /// reth-db environment (MDBX) — Some when initialized successfully
    pub db: Option<Arc<DatabaseEnv>>,
}

impl CallDb {
    /// Persist the current prune state to disk.
    /// Uses reth-db if available, falls back to JSON.
    pub fn save_prune_state(&self, state: &PruneState) -> Result<(), StorageError> {
        if let Some(ref db) = self.db {
            return db_save_prune(db, state);
        }
        // JSON fallback
        let data = serde_json::to_vec_pretty(state)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let path = self.prune_dir.join("prune_state.json");
        std::fs::write(&path, data)
            .map_err(|e| StorageError::IoError(std::io::Error::other(e.to_string())))?;
        Ok(())
    }

    /// Load prune state from disk, or return a fresh instance.
    /// Uses reth-db if available, falls back to JSON.
    pub fn load_prune_state(&self) -> Result<PruneState, StorageError> {
        if let Some(ref db) = self.db {
            return db_load_prune(db);
        }
        // JSON fallback
        let path = self.prune_dir.join("prune_state.json");
        if !path.exists() {
            return Ok(PruneState::new());
        }
        let data = std::fs::read(&path)
            .map_err(|e| StorageError::IoError(std::io::Error::other(e.to_string())))?;
        serde_json::from_slice(&data)
            .map_err(|e| StorageError::Serialization(e.to_string()))
    }
}

/// Open or create the Callchain database at the given path.
///
/// Initializes reth-db (MDBX) as the primary persistence backend.
/// If reth-db initialization fails, falls back to JSON file persistence.
pub fn open_db(data_dir: PathBuf) -> Result<CallDb, StorageError> {
    std::fs::create_dir_all(&data_dir).map_err(|e| {
        StorageError::IoError(std::io::Error::other(e.to_string()))
    })?;

    let state_dir = data_dir.join("state");
    let prune_dir = data_dir.join("prune");
    std::fs::create_dir_all(&state_dir).map_err(|e| {
        StorageError::IoError(std::io::Error::other(e.to_string()))
    })?;
    std::fs::create_dir_all(&prune_dir).map_err(|e| {
        StorageError::IoError(std::io::Error::other(e.to_string()))
    })?;

    // Try to initialize reth-db
    let db = match init_call_db(&data_dir) {
        Ok(db_env) => Some(db_env),
        Err(e) => {
            tracing::warn!(error = %e, "failed to initialize reth-db, falling back to JSON persistence");
            None
        }
    };

    Ok(CallDb { data_dir, state_dir, prune_dir, db })
}

/// Create a temporary database for testing (JSON fallback mode).
pub fn open_test_db() -> Result<CallDb, StorageError> {
    let tmp = std::env::temp_dir().join(format!("call-db-test-{}", std::process::id()));
    open_db(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prune::{ExecutionTrace, BlockBody};
    use call_primitives::Hash;

    #[test]
    fn test_db_open_create() {
        let tmp = std::env::temp_dir().join(format!(
            "call-db-open-test-{}",
            std::process::id()
        ));
        let db = open_db(tmp.clone()).expect("open db");
        assert!(db.data_dir.exists());
        assert!(db.state_dir.exists());
        assert!(db.prune_dir.exists());
        let _ = std::fs::remove_dir_all(&db.data_dir);
    }

    #[test]
    fn test_db_open_invalid_path() {
        let result = open_db(PathBuf::from("/root/call-db-invalid"));
        assert!(result.is_err());
    }

    #[test]
    fn test_prune_state_persistence() {
        let tmp = std::env::temp_dir().join(format!("call-db-persist-test-{}", std::process::id()));
        let db = open_db(tmp).expect("open test db");

        // Save fresh prune state
        let mut state = PruneState::new();
        state.add_execution_trace(100, ExecutionTrace { tx_index: 0, gas_used: 50000, success: true });
        state.add_block_body(100, BlockBody { block_hash: Hash::ZERO, tx_count: 5, body_size: 1024 });
        db.save_prune_state(&state).expect("save");

        // Load it back
        let loaded = db.load_prune_state().expect("load");
        assert_eq!(loaded.trace_count(), 1);
        assert_eq!(loaded.body_count(), 1);

        let _ = std::fs::remove_dir_all(&db.data_dir);
    }

    #[test]
    fn test_prune_state_missing_file() {
        let tmp = std::env::temp_dir().join(format!("call-db-missing-test-{}", std::process::id()));
        let db = open_db(tmp).expect("open test db");
        let loaded = db.load_prune_state().expect("load defaults");
        assert_eq!(loaded.trace_count(), 0);
        let _ = std::fs::remove_dir_all(&db.data_dir);
    }
}
