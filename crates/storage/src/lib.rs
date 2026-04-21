//! Callchain storage layer — table definitions, prune config, state snapshots.
//!
//! Uses reth-db (MDBX) as the sole persistence backend — no JSON fallback.

mod db;
mod expiration;
mod prune;
pub mod reth_db;
mod tables;

pub use db::*;
pub use expiration::*;
pub use prune::*;
pub use reth_db::*;
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
    #[error("io error: {0}")]
    IoError(std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("validation error: {0}")]
    Validation(String),
}
