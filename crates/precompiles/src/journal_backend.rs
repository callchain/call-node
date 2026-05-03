//! [`StorageBackend`] implementation backed by [`StorageCtx`] (TLS journal).
//!
//! Used by domain precompiles (e.g. `call-asset::AssetPrecompile`) to delegate
//! business-logic storage operations to the live revm journal.
//!
//! # Automatic gas metering
//!
//! Gas for SLOAD / SSTORE is **not** tracked inside `JournalBackend`. Instead,
//! it is automatically deducted by the underlying [`StorageProvider`] (e.g.
//! [`EvmStorageProvider`](crate::storage::EvmStorageProvider)) via revm's
//! native warm/cold access flags and Cancun SSTORE refund rules.
//!
//! `JournalBackend::load()` calls `StorageCtx::sload()`, which goes through the
//! provider and triggers automatic gas deduction. The same applies to
//! `store()` → `StorageCtx::sstore()`. Precompile dispatch functions then
//! propagate the accumulated `gas_used` / `gas_refunded` from `StorageCtx`
//! back into the [`PrecompileOutput`] via [`fill_precompile_output`](crate::storage::fill_precompile_output).

use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;

use crate::storage::StorageCtx;

/// Zero-sized type that implements [`StorageBackend`] via the thread-local
/// [`StorageCtx`] singleton.
///
/// Must only be used inside a [`StorageCtx::enter`] closure.
#[derive(Debug, Default, Clone, Copy)]
pub struct JournalBackend;

impl StorageBackend for JournalBackend {
    fn load(&self, address: Address, slot: U256) -> U256 {
        StorageCtx::sload(address, slot).unwrap_or(U256::ZERO)
    }

    fn store(&mut self, address: Address, slot: U256, value: U256) {
        let _ = StorageCtx::sstore(address, slot, value);
    }
}
