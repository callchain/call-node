//! call-consensus — Simplex BFT consensus layer for Callchain.
//!
//! Implements block structures, proposer selection, validator staking,
//! and Simplex BFT consensus integration (per spec §2.3, §2.4, §2.5, §12.6).

pub mod bft;
pub mod block;
pub mod block_cache;
pub mod digest;
pub mod evm_storage_provider;
pub mod exec;
pub mod fork;
pub mod proposer;
pub mod simplex;
pub mod validator;


#[cfg(test)]
mod tests;

pub use bft::*;
pub use block::*;
pub use block_cache::*;
pub use digest::*;
pub use exec::block_executor::EvmBlockExecutor;
pub use fork::*;
pub use proposer::*;
pub use simplex::*;
pub use validator::*;
