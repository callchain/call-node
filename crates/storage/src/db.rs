//! Database initialization stub.
//!
//! reth-db (MDBX) integration is deferred. The open_db function signature
//! is defined for future use when the reth dependency is compatible.

use std::path::PathBuf;

/// Database handle placeholder.
pub struct CallDb {
    pub data_dir: PathBuf,
}

/// Open or create the Callchain database at the given path.
///
/// Currently returns a stub. When reth-db is integrated, this will initialize
/// the MDBX environment with all tables registered.
pub fn open_db(data_dir: PathBuf) -> Result<CallDb, StorageError> {
    std::fs::create_dir_all(&data_dir).map_err(|e| {
        StorageError::Database(format!("failed to create data directory: {e}"))
    })?;

    // TODO: replace with reth-db initialization:
    // let db = init_db(&data_dir, Default::default())?;
    // register_tables(&db)?;

    Ok(CallDb { data_dir })
}

/// Create a temporary database for testing.
pub fn open_test_db() -> Result<CallDb, StorageError> {
    let tmp = std::env::temp_dir().join(format!("call-db-test-{}", std::process::id()));
    open_db(tmp)
}

use crate::StorageError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_open_create() {
        let tmp = std::env::temp_dir().join(format!(
            "call-db-open-test-{}",
            std::process::id()
        ));
        let db = open_db(tmp.clone()).expect("open db");
        assert_eq!(db.data_dir, tmp);
        // Clean up
        let _ = std::fs::remove_dir_all(&db.data_dir);
    }

    #[test]
    fn test_db_open_invalid_path() {
        // /root/db should be inaccessible
        let result = open_db(PathBuf::from("/root/call-db-invalid"));
        assert!(result.is_err());
    }
}
