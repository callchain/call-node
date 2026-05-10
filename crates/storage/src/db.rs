//! Database initialization with reth-db (MDBX) persistence.
//!
//! MDBX is the sole persistence backend. Initialization failure is fatal —
//! there is no JSON fallback. All state types are stored in MDBX tables.

use std::path::PathBuf;
use std::sync::Arc;

use reth_db::DatabaseEnv;

use crate::prune::PruneState;
use crate::reth_db::{
    db_get, db_put, init_call_db, load_prune_state as db_load_prune,
    save_prune_state as db_save_prune, CallSchemaVersion,
};
use crate::StorageError;

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

    /// Run pending database migrations.
    pub fn run_migrations(&self, runner: &MigrationRunner) -> Result<(), StorageError> {
        runner.run(&self.db)
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Migration Framework
// ═══════════════════════════════════════════════════════════════════════

/// A single database migration.
pub trait Migration: Send + Sync {
    /// Human-readable migration name (used for logging and idempotence).
    fn name(&self) -> &'static str;
    /// Target schema version after this migration runs.
    fn version(&self) -> u64;
    /// Apply the migration to the database.
    fn apply(&self, db: &DatabaseEnv) -> Result<(), StorageError>;
}

/// Reads the current schema version from the database.
/// Returns 0 if no version has been written (fresh database).
pub fn read_schema_version(db: &DatabaseEnv) -> Result<u64, StorageError> {
    match db_get::<CallSchemaVersion>(db, b"version")? {
        Some(bytes) if bytes.len() >= 8 => {
            let arr: [u8; 8] = bytes[..8]
                .try_into()
                .map_err(|_| StorageError::Decoding("invalid schema version bytes".into()))?;
            Ok(u64::from_be_bytes(arr))
        }
        _ => Ok(0),
    }
}

/// Writes the schema version to the database.
pub fn write_schema_version(db: &DatabaseEnv, version: u64) -> Result<(), StorageError> {
    db_put::<CallSchemaVersion>(db, b"version".to_vec(), version.to_be_bytes().to_vec())
}

/// Runner that applies pending migrations in version order.
#[derive(Default)]
pub struct MigrationRunner {
    migrations: Vec<Box<dyn Migration>>,
}

impl MigrationRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a migration. Migrations must be added in ascending version order.
    pub fn register(mut self, migration: Box<dyn Migration>) -> Self {
        self.migrations.push(migration);
        self
    }

    /// Read current schema version, then apply all migrations with a
    /// version strictly greater than the current one, bumping the stored
    /// version after each successful application.
    pub fn run(&self, db: &DatabaseEnv) -> Result<(), StorageError> {
        let current = read_schema_version(db)?;
        for migration in &self.migrations {
            if migration.version() <= current {
                continue;
            }
            migration.apply(db)?;
            write_schema_version(db, migration.version())?;
        }
        Ok(())
    }

    /// Total number of registered migrations.
    pub fn len(&self) -> usize {
        self.migrations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.migrations.is_empty()
    }
}

/// Open or create the Callchain database at the given path.
///
/// Initializes reth-db (MDBX). If initialization fails, returns an error
/// immediately — there is no fallback.
pub fn open_db(data_dir: PathBuf) -> Result<CallDb, StorageError> {
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| StorageError::IoError(std::io::Error::other(e.to_string())))?;

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
    use crate::prune::{BlockBody, ExecutionTrace};
    use call_primitives::Hash;

    #[test]
    fn test_db_open_create() {
        let tmp = std::env::temp_dir().join(format!("call-db-open-test-{}", std::process::id()));
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
        state.add_execution_trace(
            100,
            ExecutionTrace {
                tx_index: 0,
                gas_used: 50000,
                success: true,
            },
        );
        state.add_block_body(
            100,
            BlockBody {
                block_hash: Hash::ZERO,
                tx_count: 5,
                body_size: 1024,
            },
        );
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

    // ── Migration Framework Tests ───────────────────────────────────────

    #[test]
    fn test_migration_fresh_db_starts_at_version_zero() {
        let tmp =
            std::env::temp_dir().join(format!("call-db-migration-fresh-{}", std::process::id()));
        let db = open_db(tmp).expect("open test db");
        let version = read_schema_version(&db.db).expect("read version");
        assert_eq!(version, 0, "fresh db should have schema version 0");
        let _ = std::fs::remove_dir_all(&db.data_dir);
    }

    #[test]
    fn test_migration_applies_in_order() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let tmp =
            std::env::temp_dir().join(format!("call-db-migration-order-{}", std::process::id()));
        let db = open_db(tmp).expect("open test db");

        static ORDER: AtomicU64 = AtomicU64::new(0);
        static V1_RAN: AtomicU64 = AtomicU64::new(0);
        static V2_RAN: AtomicU64 = AtomicU64::new(0);

        ORDER.store(1, Ordering::SeqCst);
        V1_RAN.store(0, Ordering::SeqCst);
        V2_RAN.store(0, Ordering::SeqCst);

        struct MigrationV1;
        impl Migration for MigrationV1 {
            fn name(&self) -> &'static str {
                "add_default_asset"
            }
            fn version(&self) -> u64 {
                1
            }
            fn apply(&self, _db: &DatabaseEnv) -> Result<(), StorageError> {
                V1_RAN.store(ORDER.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
                Ok(())
            }
        }
        struct MigrationV2;
        impl Migration for MigrationV2 {
            fn name(&self) -> &'static str {
                "add_fee_currency_index"
            }
            fn version(&self) -> u64 {
                2
            }
            fn apply(&self, _db: &DatabaseEnv) -> Result<(), StorageError> {
                V2_RAN.store(ORDER.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
                Ok(())
            }
        }

        let runner = MigrationRunner::new()
            .register(Box::new(MigrationV1))
            .register(Box::new(MigrationV2));

        runner.run(&db.db).expect("run migrations");

        assert_eq!(V1_RAN.load(Ordering::SeqCst), 1, "v1 should run first");
        assert_eq!(V2_RAN.load(Ordering::SeqCst), 2, "v2 should run second");
        assert_eq!(read_schema_version(&db.db).unwrap(), 2);
        let _ = std::fs::remove_dir_all(&db.data_dir);
    }

    #[test]
    fn test_migration_idempotent() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let tmp = std::env::temp_dir().join(format!(
            "call-db-migration-idempotent-{}",
            std::process::id()
        ));
        let db = open_db(tmp).expect("open test db");

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.store(0, Ordering::SeqCst);

        struct CountingMigration;
        impl Migration for CountingMigration {
            fn name(&self) -> &'static str {
                "counting_migration"
            }
            fn version(&self) -> u64 {
                1
            }
            fn apply(&self, _db: &DatabaseEnv) -> Result<(), StorageError> {
                COUNTER.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let runner = MigrationRunner::new().register(Box::new(CountingMigration));

        runner.run(&db.db).expect("first run");
        assert_eq!(COUNTER.load(Ordering::SeqCst), 1);

        runner.run(&db.db).expect("second run");
        assert_eq!(
            COUNTER.load(Ordering::SeqCst),
            1,
            "migration should not run twice"
        );

        assert_eq!(read_schema_version(&db.db).unwrap(), 1);
        let _ = std::fs::remove_dir_all(&db.data_dir);
    }

    #[test]
    fn test_migration_failure_leaves_version_unchanged() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let tmp =
            std::env::temp_dir().join(format!("call-db-migration-fail-{}", std::process::id()));
        let db = open_db(tmp).expect("open test db");

        static V1_RAN: AtomicU64 = AtomicU64::new(0);
        V1_RAN.store(0, Ordering::SeqCst);

        struct GoodMigration;
        impl Migration for GoodMigration {
            fn name(&self) -> &'static str {
                "good_migration"
            }
            fn version(&self) -> u64 {
                1
            }
            fn apply(&self, _db: &DatabaseEnv) -> Result<(), StorageError> {
                V1_RAN.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        struct BadMigration;
        impl Migration for BadMigration {
            fn name(&self) -> &'static str {
                "bad_migration"
            }
            fn version(&self) -> u64 {
                2
            }
            fn apply(&self, _db: &DatabaseEnv) -> Result<(), StorageError> {
                Err(StorageError::Validation("intentional failure".into()))
            }
        }

        let runner = MigrationRunner::new()
            .register(Box::new(GoodMigration))
            .register(Box::new(BadMigration));

        let result = runner.run(&db.db);
        assert!(result.is_err(), "runner should fail on bad migration");

        // Version should remain at 1 (last successful migration)
        assert_eq!(read_schema_version(&db.db).unwrap(), 1);
        assert_eq!(
            V1_RAN.load(Ordering::SeqCst),
            1,
            "good migration should have run"
        );
        let _ = std::fs::remove_dir_all(&db.data_dir);
    }

    #[test]
    fn test_migration_can_write_data() {
        use crate::reth_db::{db_get, CallEvmAccounts};

        let tmp =
            std::env::temp_dir().join(format!("call-db-migration-write-{}", std::process::id()));
        let db = open_db(tmp).expect("open test db");

        struct SeedMigration;
        impl Migration for SeedMigration {
            fn name(&self) -> &'static str {
                "seed_test_data"
            }
            fn version(&self) -> u64 {
                1
            }
            fn apply(&self, db: &DatabaseEnv) -> Result<(), StorageError> {
                db_put::<CallEvmAccounts>(db, b"seed_key".to_vec(), b"seed_value".to_vec())
            }
        }

        let runner = MigrationRunner::new().register(Box::new(SeedMigration));

        runner.run(&db.db).expect("run migration");

        let value = db_get::<CallEvmAccounts>(&db.db, b"seed_key")
            .expect("read")
            .expect("exists");
        assert_eq!(value, b"seed_value");
        let _ = std::fs::remove_dir_all(&db.data_dir);
    }
}
