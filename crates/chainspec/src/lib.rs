//! call-chainspec — Genesis and chain configuration (per spec §16)
//!
//! Genesis JSON parsing, initialization flow, state root computation,
//! and chain ID management.

pub mod genesis;

pub use genesis::*;
