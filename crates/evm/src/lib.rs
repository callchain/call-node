//! Callchain EVM Layer — Revm-based EVM execution (per spec §4)
//!
//! EVM executor, state management, ERC-20 deployment, gas tracking.

mod executor;
mod state;
pub mod backend;
pub mod db;
pub mod erc20_bytecode;
pub mod trie;
pub mod provider;
pub mod block_executor;

pub use executor::*;
pub use reth_db::DatabaseEnv as EvmDatabaseEnv;
pub use state::*;
pub use backend::*;
pub use alloy_primitives::{U256, Bytes};
