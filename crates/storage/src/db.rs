//! Database initialization with file-based JSON persistence.
//!
//! Since reth-db is deferred, this provides a file-based backend that
//! serializes state to JSON files on disk and reloads on startup.

use std::path::PathBuf;

use crate::StorageError;
use crate::prune::PruneState;

/// Database handle with file-based persistence.
pub struct CallDb {
    pub data_dir: PathBuf,
    /// Directory for state snapshots and persistence files
    pub state_dir: PathBuf,
    /// Directory for prune state persistence
    pub prune_dir: PathBuf,
}

impl CallDb {
    /// Persist the current prune state to disk.
    pub fn save_prune_state(&self, state: &PruneState) -> Result<(), StorageError> {
        let data = serde_json::to_vec_pretty(state)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let path = self.prune_dir.join("prune_state.json");
        std::fs::write(&path, data)
            .map_err(|e| StorageError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        Ok(())
    }

    /// Load prune state from disk, or return a fresh instance.
    pub fn load_prune_state(&self) -> Result<PruneState, StorageError> {
        let path = self.prune_dir.join("prune_state.json");
        if !path.exists() {
            return Ok(PruneState::new());
        }
        let data = std::fs::read(&path)
            .map_err(|e| StorageError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        serde_json::from_slice(&data)
            .map_err(|e| StorageError::Serialization(e.to_string()))
    }
}

/// Open or create the Callchain database at the given path.
///
/// Creates the necessary directory structure and returns a `CallDb` handle
/// for file-based state persistence.
pub fn open_db(data_dir: PathBuf) -> Result<CallDb, StorageError> {
    std::fs::create_dir_all(&data_dir).map_err(|e| {
        StorageError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    })?;

    let state_dir = data_dir.join("state");
    let prune_dir = data_dir.join("prune");
    std::fs::create_dir_all(&state_dir).map_err(|e| {
        StorageError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    })?;
    std::fs::create_dir_all(&prune_dir).map_err(|e| {
        StorageError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    })?;

    Ok(CallDb { data_dir, state_dir, prune_dir })
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
