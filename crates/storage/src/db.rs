//! Database initialization with reth-db (MDBX) persistence.
//!
//! MDBX is the sole persistence backend. Initialization failure is fatal —
//! there is no JSON fallback. All state types are stored in MDBX tables.

use std::path::PathBuf;
use std::sync::Arc;

use reth_db::DatabaseEnv;

use crate::StorageError;
use crate::prune::PruneState;
use crate::reth_db::{init_call_db, load_prune_state as db_load_prune, save_prune_state as db_save_prune};

/// Database handle with mandatory reth-db (MDBX) persistence.
#[derive(Clone)]
pub struct CallDb {
    pub data_dir: PathBuf,
    /// reth-db environment (MDBX) — always present after successful open
    pub db: Arc<DatabaseEnv>,
}

impl CallDb {
    /// Persist the current prune state to MDBX.
    pub fn save_prune_state(&self, state: &PruneState) -> Result<(), StorageError> {
        db_save_prune(&self.db, state)
    }

    /// Load prune state from MDBX, or return a fresh instance.
    pub fn load_prune_state(&self) -> Result<PruneState, StorageError> {
        db_load_prune(&self.db)
    }
}

/// Open or create the Callchain database at the given path.
///
/// Initializes reth-db (MDBX). If initialization fails, returns an error
/// immediately — there is no fallback.
pub fn open_db(data_dir: PathBuf) -> Result<CallDb, StorageError> {
    std::fs::create_dir_all(&data_dir).map_err(|e| {
        StorageError::IoError(std::io::Error::other(e.to_string()))
    })?;

    let db = init_call_db(&data_dir)?;

    Ok(CallDb { data_dir, db })
}

/// Create a temporary database for testing.
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
