//! Asset precompile entry point.
//!
//! TODO (Phase 1 wave 2): Replace with Unified Dispatch framework.

use call_precompiles::StatefulPrecompile;
use call_primitives::Address;
use revm_precompile::PrecompileResult;

/// Thin wrapper that routes EVM calls to AssetStorage via JournalBackend.
pub struct AssetPrecompile;

impl StatefulPrecompile for AssetPrecompile {
    fn call(&mut self, _calldata: &[u8], _msg_sender: Address) -> PrecompileResult {
        // TODO: implement once JournalBackend is available
        // For now, the canonical AssetPrecompile still lives in call-precompiles/src/asset.rs
        Err(revm_precompile::PrecompileError::Other(
            "AssetPrecompile not yet migrated".into(),
        ))
    }
}
