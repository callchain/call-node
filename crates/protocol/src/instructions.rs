//! T1.4 — Instruction Execution (per spec §3.5, §3.6)
//!
//! Instruction enum, execution flow, atomicity with rollback.

pub mod types;
pub mod agent;
pub mod exec;
#[cfg(test)]
pub mod tests;

pub use types::*;
pub use agent::*;
pub use exec::*;
