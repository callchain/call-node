//! Callchain EVM Layer — Revm-based EVM execution (per spec §4)
//!
//! EVM executor, state management, ERC-20 deployment, gas tracking.

mod executor;
mod state;

pub use executor::*;
pub use state::*;
pub use alloy_primitives::{U256, Bytes};
