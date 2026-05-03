use call_primitives::{Address, U256};

/// Pluggable EVM storage slot read/write backend.
///
/// Two implementations:
/// - `EvmStateBackend` — in domain crates (call-asset, etc.), for consensus/RPC/tests
/// - `JournalBackend` — in call-precompiles, for precompile execution
pub trait StorageBackend {
    fn load(&self, address: Address, slot: U256) -> U256;
    fn store(&mut self, address: Address, slot: U256, value: U256);
}

impl<B: StorageBackend> StorageBackend for &mut B {
    fn load(&self, address: Address, slot: U256) -> U256 {
        (**self).load(address, slot)
    }
    fn store(&mut self, address: Address, slot: U256, value: U256) {
        (**self).store(address, slot, value);
    }
}

impl<B: StorageBackend> StorageBackend for &B {
    fn load(&self, address: Address, slot: U256) -> U256 {
        (**self).load(address, slot)
    }
    fn store(&mut self, _address: Address, _slot: U256, _value: U256) {
        panic!("cannot store through an immutable reference");
    }
}
