//! Ethereum light client implementation.
//!
//! Verifies Ethereum block headers against a trusted anchor and
//! transaction inclusion via Merkle-Patricia Trie proofs.

pub(crate) mod client;
pub(crate) mod proof;

pub use client::*;
