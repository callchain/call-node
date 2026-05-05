use call_protocol::storage_backend::StorageBackend;
use call_primitives::{Address, U256};

// ── ProtocolStorage trait ─────────────────────────────────────────────

/// Abstracts the storage operations needed by the protocol layer.
///
/// Implemented by [`InMemoryStateProvider`] so that protocol accessors
/// can work with an MDBX-backed provider without knowing the details.
///
/// This is the migration bridge for P0-3: once all accessors are generic
/// over `ProtocolStorage`, the underlying implementation can be swapped
/// (e.g. to direct MDBX cursor access) without touching call-sites.
pub trait ProtocolStorage {
    /// Read a storage slot. Returns `U256::ZERO` if the slot is absent.
    fn get_storage(&self, address: &Address, key: U256) -> U256;

    /// Write a storage slot.
    fn set_storage(&mut self, address: Address, key: U256, value: U256);

    /// Read an account's EVM balance. Returns `U256::ZERO` if absent.
    fn get_balance(&self, address: &Address) -> U256;

    /// Write an account's EVM balance.
    fn set_balance(&mut self, address: Address, balance: U256);
}

impl ProtocolStorage for crate::provider::InMemoryStateProvider {
    fn get_storage(&self, address: &Address, key: U256) -> U256 {
        self.get_storage(address, key)
    }
    fn set_storage(&mut self, address: Address, key: U256, value: U256) {
        self.set_storage(address, key, value);
    }
    fn get_balance(&self, address: &Address) -> U256 {
        self.get_balance(address)
    }
    fn set_balance(&mut self, address: Address, balance: U256) {
        self.set_balance(address, balance);
    }
}

// Blanket impls so that `&S` and `&mut S` also implement ProtocolStorage,
// allowing callers to pass `&RwLockReadGuard<S>` or `&MutexGuard<S>`
// without manual dereferencing.

impl<S: ProtocolStorage + ?Sized> ProtocolStorage for &S {
    fn get_storage(&self, address: &Address, key: U256) -> U256 {
        (**self).get_storage(address, key)
    }
    fn set_storage(&mut self, _address: Address, _key: U256, _value: U256) {
        panic!("&S ProtocolStorage is read-only");
    }
    fn get_balance(&self, address: &Address) -> U256 {
        (**self).get_balance(address)
    }
    fn set_balance(&mut self, _address: Address, _balance: U256) {
        panic!("&S ProtocolStorage is read-only");
    }
}

impl<S: ProtocolStorage + ?Sized> ProtocolStorage for &mut S {
    fn get_storage(&self, address: &Address, key: U256) -> U256 {
        (**self).get_storage(address, key)
    }
    fn set_storage(&mut self, address: Address, key: U256, value: U256) {
        (**self).set_storage(address, key, value);
    }
    fn get_balance(&self, address: &Address) -> U256 {
        (**self).get_balance(address)
    }
    fn set_balance(&mut self, address: Address, balance: U256) {
        (**self).set_balance(address, balance);
    }
}

// ── Generic StorageBackend implementations ────────────────────────────

/// StorageBackend backed by any mutable [`ProtocolStorage`] reference.
pub struct ProtocolStateBackend<'a, S: ProtocolStorage + ?Sized>(pub &'a mut S);

impl<'a, S: ProtocolStorage + ?Sized> StorageBackend for ProtocolStateBackend<'a, S> {
    fn load(&self, address: Address, slot: U256) -> U256 {
        self.0.get_storage(&address, slot)
    }
    fn store(&mut self, address: Address, slot: U256, value: U256) {
        self.0.set_storage(address, slot, value);
    }
}

/// StorageBackend backed by any immutable [`ProtocolStorage`] reference.
///
/// Panics on store — use only for read-only operations.
pub struct ProtocolStateRefBackend<'a, S: ProtocolStorage + ?Sized>(pub &'a S);

impl<'a, S: ProtocolStorage + ?Sized> StorageBackend for ProtocolStateRefBackend<'a, S> {
    fn load(&self, address: Address, slot: U256) -> U256 {
        self.0.get_storage(&address, slot)
    }
    fn store(&mut self, _address: Address, _slot: U256, _value: U256) {
        panic!("ProtocolStateRefBackend is read-only");
    }
}

