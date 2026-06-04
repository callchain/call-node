//! Callchain EVM Layer — Revm-based EVM execution (per spec §4)
//!
//! EVM executor, state management, ERC-20 deployment, gas tracking.

pub mod backend;
pub mod block_executor;
pub mod codec;
pub mod db;
pub mod erc20_bytecode;
mod executor;
pub mod provider;
pub mod state;
pub mod trie;

pub use alloy_primitives::{Bytes, U256};
pub use backend::*;
pub use executor::*;
pub use reth_db::DatabaseEnv as EvmDatabaseEnv;
