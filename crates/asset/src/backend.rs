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
