//! Agent precompile at 0x209 — fallback stub
//!
//! The real implementation lives in `call-agent` at
//! `crates/agent/src/precompile.rs` and is registered at node startup via
//! `register_agent_precompile()`.  That registration replaces this stub in
//! the OnceLock, so the full `register` / `grant` / `revoke` logic is used
//! in production and tests that wire up the agent crate.
//!
//! This file only exists to provide a compile-time fallback (returning
//! "not yet implemented") when no external implementation is registered.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult};

#[allow(dead_code)]
pub(crate) const AGENT_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000209");

/// Fallback agent precompile entry point.
///
/// Never invoked at runtime because `call-agent` registers its own
/// implementation during node startup. Returns an error so callers get
/// immediate feedback if the registration was accidentally skipped.
pub fn agent_precompile_fn(_input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 10000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    Err(PrecompileError::Other(
        "agent precompile not registered — call register_agent_precompile()".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_address() {
        assert_eq!(
            AGENT_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000209")
        );
    }
}
