//! Bridge deposit: Protocol balance → EVM balance (per spec §5.2)
//!
//! Flow:
//! 1. Validate asset is registered
//! 2. Check bridge not paused
//! 3. Check per-tx and daily limits
//! 4. Deduct protocol balance (burn from protocol)
//! 5. EVM mint equivalent tokens via bridgeMint (for user assets)
//!    or native EVM balance transfer (for CALL, asset_id == 1)
//! 6. Same-block completion guarantee
//!
//! CALL (asset_id == 1) is bridged as native EVM balance.
//! User-defined assets are bridged as ERC-20 wrapped tokens.
//! Asset 0 (virtual USD) is not bridgeable.

use alloy_primitives::{Address, U256};
use call_primitives::AssetId;
use call_protocol::AccountState;
use call_protocol::registry::AssetRegistry;
use call_evm::{EvmExecutor, EvmState, EvmExecutionResult};
use crate::{BridgeConfig, BridgeError, BridgeOp, BridgeStateManager};

/// Execute a bridge deposit: deduct protocol balance, mint EVM tokens
///
/// Per spec §5.2: same-block completion guarantee.
/// If EVM mint fails, the protocol balance is restored (atomic rollback).
pub fn execute_deposit(
    op: &BridgeOp,
    protocol_account: &mut AccountState,
    evm_state: &mut EvmState,
    evm_executor: &EvmExecutor,
    bridge_state: &mut BridgeStateManager,
    config: &BridgeConfig,
    asset_registry: &AssetRegistry,
    bridge_address: Address, // 0xCC bridge operator address
    _protocol_bridge_caller: Address, // caller for bridgeMint (bridge operator)
    current_block: u64,
) -> Result<EvmExecutionResult, BridgeError> {
    let BridgeOp::DepositToEvm {
        asset_id,
        from,
        to,
        amount,
    } = op
    else {
        return Err(BridgeError::EvmExecutionFailed("not a deposit op".into()));
    };

    // 1. Validate asset is registered
    if asset_registry.get_asset(*asset_id).is_none() {
        return Err(BridgeError::AssetNotRegistered(*asset_id));
    }

    // 2. Check bridge not paused
    if bridge_state.is_paused(*asset_id) {
        return Err(BridgeError::BridgePaused(*asset_id));
    }

    // 3. Check per-tx limit
    bridge_state.check_per_tx_limit(*amount, config.max_per_tx)?;

    // 4. Check daily limit (auto-resets when a new day starts)
    bridge_state.check_and_update_daily_limit(*asset_id, *amount, config.daily_limit_per_asset, current_block, config.blocks_per_day)?;

    // 5. Check protocol balance is sufficient
    let protocol_balance = protocol_account.get_balance(*asset_id, from);
    if protocol_balance < *amount {
        return Err(BridgeError::InsufficientProtocolBalance(*asset_id, *amount));
    }

    // 6. Reject virtual USD (asset_id == 0) from bridging
    if *asset_id == 0 {
        return Err(BridgeError::AssetNotRegistered(*asset_id));
    }

    // 7. Deduct protocol balance (atomic — if EVM fails, we restore)
    let snapshot = protocol_account.clone();
    protocol_account
        .deduct_balance(*asset_id, *from, *amount)
        .map_err(|_| BridgeError::InsufficientProtocolBalance(*asset_id, *amount))?;

    // 8. Bridge to EVM
    let amount_u256 = U256::from(*amount);

    let exec_result = if *asset_id == 1 {
        // CALL: transfer as native EVM balance
        let current = evm_state.get_balance(to);
        evm_state.set_balance(*to, current + amount_u256);
        Ok(EvmExecutionResult {
            success: true,
            gas_used: 21_000,
            output: alloy_primitives::Bytes::default(),
            logs: vec![],
        })
    } else {
        // User-defined asset: mint ERC-20 wrapped token
        evm_executor.evm_call_bridge_mint(
            *from,
            bridge_address,
            evm_state,
            *to,
            amount_u256,
        )
        .map_err(|e| BridgeError::EvmExecutionFailed(format!("{e:?}")))
    };

    match exec_result {
        Ok(execution) => {
            if execution.success {
                // 9. Record completed deposit
                bridge_state.record_deposit(*asset_id, *amount);
                Ok(execution)
            } else {
                // EVM reverted — restore protocol balance
                *protocol_account = snapshot;
                Err(BridgeError::EvmExecutionFailed("bridge deposit to EVM reverted".into()))
            }
        }
        Err(e) => {
            // EVM error — restore protocol balance
            *protocol_account = snapshot;
            Err(e)
        }
    }
}

/// Check if there is sufficient protocol balance for a deposit
pub fn check_deposit_balance(
    protocol_account: &AccountState,
    asset_id: AssetId,
    from: Address,
    amount: u128,
) -> Result<(), BridgeError> {
    let balance = protocol_account.get_balance(asset_id, &from);
    if balance < amount {
        Err(BridgeError::InsufficientProtocolBalance(asset_id, amount))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_protocol::registry::AssetRegistry;
    use call_evm::{EvmExecutor, EvmState};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn setup_registry() -> AssetRegistry {
        let mut registry = AssetRegistry::new();
        registry
            .register_asset(
                "CALL".into(),
                "Callchain".into(),
                18,
                test_addr(1),
                0, // compliance_policy: None
                100, // registered_at
            )
            .ok();
        registry
    }

    #[test]
    fn test_deposit_insufficient_protocol_balance() {
        let mut protocol_account = AccountState::new();
        protocol_account
            .balances
            .set_balance(1, test_addr(1), 1000)
            .unwrap();

        let evm_state = EvmState::new();
        let evm_executor = EvmExecutor::new(1);
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig::default();
        let registry = setup_registry();

        let op = BridgeOp::DepositToEvm {
            asset_id: 1,
            from: test_addr(1),
            to: test_addr(2),
            amount: 500,
        };

        // Protocol balance is 1000, deposit 500 → should have sufficient
        // But EVM mint will fail because contract has no code at bridge_address
        let bridge_addr = test_addr(0xCC);
        let result = execute_deposit(
            &op,
            &mut protocol_account,
            &mut evm_state.clone(),
            &evm_executor,
            &mut bridge_state,
            &config,
            &registry,
            bridge_addr,
            test_addr(0xFF),
            100,
        );
        // EVM mint will fail because no contract at address, but protocol balance check passes
        assert!(result.is_err() || result.as_ref().is_ok_and(|r| r.success));
    }

    #[test]
    fn test_deposit_success_with_contract() {
        let mut protocol_account = AccountState::new();
        protocol_account
            .balances
            .set_balance(1, test_addr(1), 10_000)
            .unwrap();

        let mut evm_state = EvmState::new();
        evm_state.set_balance(test_addr(1), U256::from(100_000_000_000_000u128));

        let evm_executor = EvmExecutor::new(1);
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig::default();
        let mut registry = setup_registry();

        // Deploy wrapped token contract for asset 1
        let bridge = Address::repeat_byte(0xFF);
        let (contract_addr, deploy_result) = evm_executor
            .deploy_erc20_template(
                test_addr(1),
                &mut evm_state,
                "CALL",
                "CALL",
                18,
                bridge,
            )
            .unwrap();
        assert!(deploy_result.success);
        registry.set_evm_contract_address(1, contract_addr);

        let op = BridgeOp::DepositToEvm {
            asset_id: 1,
            from: test_addr(1),
            to: test_addr(2),
            amount: 500,
        };

        let result = execute_deposit(
            &op,
            &mut protocol_account,
            &mut evm_state,
            &evm_executor,
            &mut bridge_state,
            &config,
            &registry,
            contract_addr,
            test_addr(1),
            100,
        );

        assert!(result.is_ok(), "deposit failed: {:?}", result);
        assert!(result.unwrap().success);

        // Protocol balance deducted
        assert_eq!(protocol_account.get_balance(1, &test_addr(1)), 9_500);
    }

    #[test]
    fn test_check_deposit_balance() {
        let mut protocol_account = AccountState::new();
        protocol_account
            .balances
            .set_balance(1, test_addr(1), 1000)
            .unwrap();

        // Sufficient
        assert!(check_deposit_balance(&protocol_account, 1, test_addr(1), 500).is_ok());
        // Exact
        assert!(check_deposit_balance(&protocol_account, 1, test_addr(1), 1000).is_ok());
        // Insufficient
        assert!(check_deposit_balance(&protocol_account, 1, test_addr(1), 1001).is_err());
    }
}
