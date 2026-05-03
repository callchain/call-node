//! T12.1 — Governance Module (per spec §13.3)
//!
//! Proposal lifecycle, dual-track voting, timelock, emergency pause.

pub mod types;
pub mod config;
pub mod error;
pub mod manager;
pub mod precompile;

#[cfg(test)]
mod tests;

pub use types::*;
pub use config::*;
pub use error::*;
pub use manager::*;
