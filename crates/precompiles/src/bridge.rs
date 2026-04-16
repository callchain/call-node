//! Bridge precompile at 0x103

use call_primitives::{Address, AssetId, Balance, Hash};
use alloy_primitives::address;

pub(crate) const BRIDGE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000103");

#[derive(Debug, Clone)]
pub struct BridgeOpStatus {
    pub op_id: u64,
    pub completed: bool,
    pub confirmations: u64,
    pub source_tx_hash: Option<Hash>,
}

#[derive(Debug, Default)]
pub struct BridgeState {
    pub total_deposits: Balance,
    pub total_withdrawals: Balance,
    pub pending_ops: u64,
}

pub fn bridge_get_status() -> BridgeOpStatus {
    BridgeOpStatus {
        op_id: 0,
        completed: false,
        confirmations: 0,
        source_tx_hash: None,
    }
}

pub fn bridge_deposit(
    state: &mut BridgeState,
    _asset_id: AssetId,
    _from: Address,
    _to: Address,
    amount: Balance,
) {
    state.total_deposits += amount;
    state.pending_ops += 1;
}

pub fn bridge_withdraw(
    state: &mut BridgeState,
    _asset_id: AssetId,
    _from: Address,
    _to: Address,
    amount: Balance,
) {
    state.total_withdrawals += amount;
    state.pending_ops += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_address() {
        assert_eq!(
            BRIDGE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000103")
        );
    }

    #[test]
    fn test_bridge_precompile_protocol_balance() {
        let status = bridge_get_status();
        assert!(!status.completed);
        assert_eq!(status.confirmations, 0);
    }

    #[test]
    fn test_bridge_deposit_and_withdraw() {
        let mut state = BridgeState::default();
        bridge_deposit(&mut state, 1, Address::ZERO, Address::ZERO, 1000);
        assert_eq!(state.total_deposits, 1000);
        assert_eq!(state.pending_ops, 1);

        bridge_withdraw(&mut state, 1, Address::ZERO, Address::ZERO, 500);
        assert_eq!(state.total_withdrawals, 500);
        assert_eq!(state.pending_ops, 2);
    }
}
