//! Callchain storage layer — table definitions, prune config, state snapshots.
//!
//! reth-db (MDBX) integration is deferred until the reth dependency
//! supports our Rust toolchain. All types, tables, and prune logic are defined.

mod db;
mod prune;
mod tables;

pub use db::*;
pub use prune::*;
pub use tables::*;

use thiserror::Error;

/// Error type for storage operations
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Database(String),
    #[error("encoding error: {0}")]
    Encoding(String),
    #[error("decoding error: {0}")]
    Decoding(String),
    #[error("not found: {0}")]
    NotFound(String),
}
