use call_primitives::{Address, U256};

/// Pluggable EVM storage slot read/write backend.
///
/// `load` takes `&mut self` because journal-backed reads may warm slots
/// (mutating the journal). This eliminates the need for `mut_from_ref`
/// hacks that fabricate `&mut` from `&self`.
pub trait StorageBackend {
    fn load(&mut self, address: Address, slot: U256) -> U256;
    fn store(&mut self, address: Address, slot: U256, value: U256);
}

impl<B: StorageBackend + ?Sized> StorageBackend for &mut B {
    fn load(&mut self, address: Address, slot: U256) -> U256 {
        (**self).load(address, slot)
    }
    fn store(&mut self, address: Address, slot: U256, value: U256) {
        (**self).store(address, slot, value);
    }
}
