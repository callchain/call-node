//! [`StorageBackend`] implementation backed by a [`StorageProvider`].
//!
//! Used by domain precompiles (e.g. `call-asset::AssetStorage`) to delegate
//! business-logic storage operations to the live EVM state.

#![allow(unsafe_code)]

use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;

use crate::storage::StorageProvider;

/// Opaque wrapper around a raw fat pointer to `dyn StorageProvider`.
///
/// We need `JournalBackend` to be `Copy` so multiple domain storage structs
/// (e.g. `GovernanceStorage` + `AssetStorage`) can be created from the same
/// `&mut dyn StorageProvider` inside a single precompile call.  A normal
/// `&mut` reference is not `Copy`, so we erase the reference into a raw
/// pointer.  Because `dyn StorageProvider` is a fat pointer we cannot store
/// it in a normal `*mut` field and keep `#[derive(Clone, Copy)]`; instead
/// we store the two pointer words inline.
#[derive(Clone, Copy)]
pub struct JournalBackend {
    ptr: *mut (),
    vtable: *mut (),
}

impl JournalBackend {
    pub fn new(storage: &mut dyn StorageProvider) -> Self {
        let fat: *mut dyn StorageProvider = storage;
        // SAFETY: `*mut dyn Trait` is a fat pointer (two usize words).
        // We decompose it into the data pointer and vtable pointer so the
        // struct remains `Copy`.  This is sound as long as the original
        // `&mut dyn StorageProvider` remains valid for the duration of the
        // precompile call, which it does by construction.
        let (ptr, vtable): (*mut (), *mut ()) = unsafe { std::mem::transmute(fat) };
        Self { ptr, vtable }
    }

    #[allow(clippy::mut_from_ref)]
    fn as_mut(&self) -> &mut dyn StorageProvider {
        let fat: *mut dyn StorageProvider = unsafe { std::mem::transmute((self.ptr, self.vtable)) };
        unsafe { &mut *fat }
    }
}

impl StorageBackend for JournalBackend {
    fn load(&self, address: Address, slot: U256) -> U256 {
        self.as_mut().sload(address, slot).unwrap_or(U256::ZERO)
    }

    fn store(&mut self, address: Address, slot: U256, value: U256) {
        let _ = self.as_mut().sstore(address, slot, value);
    }
}
