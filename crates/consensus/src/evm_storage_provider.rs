//! EVM state storage provider for protocol instruction execution.
//!
//! Bridges `EvmState` to the `StorageProvider` trait so that protocol
//! instructions can reuse precompile logic via `StorageCtx::enter`.

use alloy_primitives::{Address, LogData, U256};
use call_evm::EvmState;
use call_precompile::storage::StorageProvider;
use revm::context_interface::journaled_state::JournalCheckpoint;
use revm_precompile::PrecompileError;
use std::collections::HashMap;

/// Production [`StorageProvider`] backed by `EvmState`.
///
/// Used during protocol transaction execution to route all state changes
/// through the same storage layout as EVM precompiles.
pub struct EvmStateStorageProvider<'a> {
    evm_state: &'a mut EvmState,
    transient: HashMap<(Address, U256), U256>,
    gas_remaining: u64,
    gas_refunded: i64,
    gas_limit: u64,
    is_static: bool,
    chain_id: u64,
    timestamp: U256,
    block_number: u64,
    beneficiary: Address,
    checkpoints: Vec<EvmStateSnapshot>,
    events: Vec<(Address, LogData)>,
}

struct EvmStateSnapshot {
    accounts: std::collections::HashMap<Address, call_evm::EvmAccount>,
    transient: HashMap<(Address, U256), U256>,
    gas_remaining: u64,
    gas_refunded: i64,
    events_len: usize,
}

impl<'a> EvmStateStorageProvider<'a> {
    pub fn new(
        evm_state: &'a mut EvmState,
        gas_limit: u64,
        is_static: bool,
        chain_id: u64,
        timestamp: U256,
        block_number: u64,
        beneficiary: Address,
    ) -> Self {
        Self {
            evm_state,
            transient: HashMap::new(),
            gas_remaining: gas_limit,
            gas_refunded: 0,
            gas_limit,
            is_static,
            chain_id,
            timestamp,
            block_number,
            beneficiary,
            checkpoints: Vec::new(),
            events: Vec::new(),
        }
    }

    /// Drain and return emitted events.
    pub fn take_events(&mut self) -> Vec<(Address, LogData)> {
        std::mem::take(&mut self.events)
    }
}

impl<'a> StorageProvider for EvmStateStorageProvider<'a> {
    fn sload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        self.deduct_gas(100)?;
        Ok(self.evm_state.get_storage(&address, key))
    }

    fn sstore(
        &mut self,
        address: Address,
        key: U256,
        value: U256,
    ) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other(
                "static call cannot mutate state".into(),
            ));
        }
        self.deduct_gas(20000)?;
        self.evm_state.set_storage(address, key, value);
        Ok(())
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        self.deduct_gas(100)?;
        Ok(self.transient.get(&(address, key)).copied().unwrap_or(U256::ZERO))
    }

    fn tstore(
        &mut self,
        address: Address,
        key: U256,
        value: U256,
    ) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other(
                "static call cannot mutate state".into(),
            ));
        }
        self.deduct_gas(100)?;
        self.transient.insert((address, key), value);
        Ok(())
    }

    fn emit_event(
        &mut self,
        address: Address,
        event: LogData,
    ) -> Result<(), PrecompileError> {
        let topics_gas = 375u64 * event.topics().len() as u64;
        let data_gas = 8u64 * event.data.len() as u64;
        self.deduct_gas(375 + topics_gas + data_gas)?;
        self.events.push((address, event));
        Ok(())
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        let snapshot = EvmStateSnapshot {
            accounts: self.evm_state.clone().into_accounts(),
            transient: self.transient.clone(),
            gas_remaining: self.gas_remaining,
            gas_refunded: self.gas_refunded,
            events_len: self.events.len(),
        };
        self.checkpoints.push(snapshot);
        JournalCheckpoint::default()
    }

    fn checkpoint_commit(&mut self, _checkpoint: JournalCheckpoint) {
        self.checkpoints.pop();
    }

    fn checkpoint_revert(&mut self, _checkpoint: JournalCheckpoint) {
        if let Some(snapshot) = self.checkpoints.pop() {
            self.evm_state.accounts = snapshot.accounts;
            self.transient = snapshot.transient;
            self.gas_remaining = snapshot.gas_remaining;
            self.gas_refunded = snapshot.gas_refunded;
            self.events.truncate(snapshot.events_len);
        }
    }

    fn deduct_gas(&mut self, gas: u64) -> Result<(), PrecompileError> {
        self.gas_remaining = self
            .gas_remaining
            .checked_sub(gas)
            .ok_or(PrecompileError::OutOfGas)?;
        Ok(())
    }

    fn refund_gas(&mut self, gas: i64) {
        self.gas_refunded = self.gas_refunded.saturating_add(gas);
    }

    fn gas_used(&self) -> u64 {
        self.gas_limit - self.gas_remaining
    }

    fn gas_refunded(&self) -> i64 {
        self.gas_refunded
    }

    fn is_static(&self) -> bool {
        self.is_static
    }

    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn timestamp(&self) -> U256 {
        self.timestamp
    }

    fn block_number(&self) -> u64 {
        self.block_number
    }

    fn beneficiary(&self) -> Address {
        self.beneficiary
    }

    fn balance_add(&mut self, address: Address, amount: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other("static call cannot mutate balance".into()));
        }
        let current = self.evm_state.get_balance(&address);
        let new = current
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("balance overflow".into()))?;
        self.evm_state.set_balance(address, new);
        self.deduct_gas(100)
    }

    fn balance_sub(&mut self, address: Address, amount: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other("static call cannot mutate balance".into()));
        }
        let current = self.evm_state.get_balance(&address);
        let new = current
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("insufficient native balance".into()))?;
        self.evm_state.set_balance(address, new);
        self.deduct_gas(100)
    }

    fn balance_get(&mut self, address: Address) -> Result<U256, PrecompileError> {
        self.deduct_gas(100)?;
        Ok(self.evm_state.get_balance(&address))
    }
}
