//! Protocol balance precompile at 0x102

use call_primitives::{Address, AssetId, Balance};
use alloy_primitives::address;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

#[allow(dead_code)]
pub(crate) const BALANCE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000102");

#[derive(Debug, Default)]
pub struct ProtocolBalanceState {
    balances: HashMap<(AssetId, Address), Balance>,
}

impl ProtocolBalanceState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_balance(&self, asset_id: AssetId, address: &Address) -> Balance {
        self.balances.get(&(asset_id, *address)).copied().unwrap_or(0)
    }

    pub fn set_balance(&mut self, asset_id: AssetId, address: Address, amount: Balance) {
        self.balances.insert((asset_id, address), amount);
    }
}

/// Live protocol balance shared across the system
static LIVE_BALANCE: OnceLock<Arc<RwLock<ProtocolBalanceState>>> = OnceLock::new();

/// Set the live balance state (called once during node boot).
/// Logs a warning if called more than once (the first caller wins).
pub fn set_live_balance(balance: Arc<RwLock<ProtocolBalanceState>>) {
    if LIVE_BALANCE.set(balance).is_err() {
        tracing::warn!("set_live_balance called after initialization — ignoring duplicate");
    }
}

/// Get the live balance state if initialized
pub fn get_live_balance() -> Option<Arc<RwLock<ProtocolBalanceState>>> {
    LIVE_BALANCE.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_balance_precompile_address() {
        assert_eq!(
            BALANCE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000102")
        );
    }

    #[test]
    fn test_protocol_balance_get() {
        let mut state = ProtocolBalanceState::new();
        let addr = Address::repeat_byte(0xAA);
        state.set_balance(1, addr, 5000);
        assert_eq!(state.get_balance(1, &addr), 5000);
        assert_eq!(state.get_balance(1, &Address::ZERO), 0);
    }
}
