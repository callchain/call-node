//! T12.1 — Governance Module (per spec §13.3)
//!
//! Proposal lifecycle, dual-track voting, timelock, emergency pause.

pub mod config;
pub mod error;
pub mod precompile;
pub mod types;

#[cfg(test)]
mod tests;

pub use config::*;
pub use error::*;
pub use types::*;
