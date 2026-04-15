//! call-consensus — Simplex BFT consensus layer for Callchain.
//!
//! Implements block structures, proposer selection, validator staking,
//! and Simplex BFT consensus integration (per spec §2.3, §2.4, §2.5, §12.6).

pub mod block;
pub mod fork;
pub mod proposer;
pub mod simplex;
pub mod validator;

pub use block::*;
pub use fork::*;
pub use proposer::*;
pub use simplex::*;
pub use validator::*;
