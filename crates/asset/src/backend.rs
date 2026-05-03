use call_evm::EvmState;
use call_protocol::storage_backend::StorageBackend;
use call_primitives::{Address, U256};

/// StorageBackend implementation backed by a mutable EvmState reference.
///
/// Used by consensus, RPC, tests, and genesis injection.
pub struct EvmStateBackend<'a>(pub &'a mut EvmState);

impl<'a> StorageBackend for EvmStateBackend<'a> {
    fn load(&self, address: Address, slot: U256) -> U256 {
        self.0.get_storage(&address, slot)
    }
    fn store(&mut self, address: Address, slot: U256, value: U256) {
        self.0.set_storage(address, slot, value);
    }
}

/// StorageBackend implementation backed by an immutable EvmState reference.
///
/// Panics on store — use only for read-only operations.
pub struct EvmStateRefBackend<'a>(pub &'a EvmState);

impl<'a> StorageBackend for EvmStateRefBackend<'a> {
    fn load(&self, address: Address, slot: U256) -> U256 {
        self.0.get_storage(&address, slot)
    }
    fn store(&mut self, _address: Address, _slot: U256, _value: U256) {
        panic!("EvmStateRefBackend is read-only");
    }
}
