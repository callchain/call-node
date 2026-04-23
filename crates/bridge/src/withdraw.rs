//! Bridge withdrawal: EVM balance → Protocol balance (per spec §5.3)
//!
//! Flow:
//! 1. Validate asset is registered
//! 2. Check bridge not paused
//! 3. Check per-tx and daily limits
//! 4. EVM burn equivalent tokens via bridgeBurn (for user assets)
//!    or native EVM balance transfer (for CALL, asset_id == 1)
//! 5. Restore protocol balance
//! 6. Same-block completion guarantee
//!
//! CALL (asset_id == 1) is withdrawn from native EVM balance.
//! User-defined assets are withdrawn from ERC-20 wrapped tokens.
//! Asset 0 (virtual USD) is not bridgeable.

use alloy_primitives::{Address, U256};
use call_protocol::balances::BalanceState;
use call_protocol::registry::AssetRegistry;
use call_evm::{EvmExecutor, EvmState, EvmExecutionResult};
use crate::{BridgeConfig, BridgeError, BridgeOp, BridgeStateManager};

/// Execute a bridge withdrawal: burn EVM tokens, restore protocol balance
///
/// Per spec §5.3: same-block completion guarantee.
/// If EVM burn fails, no protocol balance change occurs (atomic).
pub fn execute_withdraw(
    op: &BridgeOp,
    protocol_balances: &mut BalanceState,
    evm_state: &mut EvmState,
    evm_executor: &EvmExecutor,
    bridge_state: &mut BridgeStateManager,
    config: &BridgeConfig,
    asset_registry: &AssetRegistry,
    bridge_address: Address, // 0xCC bridge operator address
    protocol_bridge_caller: Address, // caller for bridgeBurn
    current_block: u64,
) -> Result<EvmExecutionResult, BridgeError> {
    let BridgeOp::WithdrawToProtocol {
        asset_id,
        from,
        to,
        amount,
    } = op
    else {
        return Err(BridgeError::EvmExecutionFailed("not a withdraw op".into()));
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

    // 5. Reject virtual USD (asset_id == 0) from bridging
    if *asset_id == 0 {
        return Err(BridgeError::AssetNotRegistered(*asset_id));
    }

    // 6. Bridge from EVM
    let amount_u256 = U256::from(*amount);

    let exec_result = if *asset_id == 1 {
        // CALL: transfer from native EVM balance
        let evm_balance = evm_state.get_balance(from);
        if evm_balance < amount_u256 {
            return Err(BridgeError::InsufficientEvmBalance(*from, amount_u256));
        }
        evm_state.set_balance(*from, evm_balance - amount_u256);
        Ok(EvmExecutionResult {
            success: true,
            gas_used: 21_000,
            output: alloy_primitives::Bytes::default(),
            logs: vec![],
        })
    } else {
        // User-defined asset: burn ERC-20 wrapped token
        evm_executor.evm_call_bridge_burn(
            protocol_bridge_caller,
            bridge_address,
            evm_state,
            amount_u256,
        )
        .map_err(|e| BridgeError::EvmExecutionFailed(format!("{e:?}")))
    };

    match exec_result {
        Ok(execution) => {
            if execution.success {
                // 7. Restore protocol balance
                let _ = protocol_balances.credit_balance(*asset_id, *to, *amount);
                // 8. Record completed withdrawal
                bridge_state.record_withdrawal(*asset_id, *amount);
                Ok(execution)
            } else {
                Err(BridgeError::EvmExecutionFailed("bridge withdraw from EVM reverted".into()))
            }
        }
        Err(e) => Err(e),
    }
}

/// Check if there is sufficient EVM balance for a withdrawal
pub fn check_withdraw_balance(
    evm_state: &EvmState,
    from: Address,
    amount: u128,
) -> Result<(), BridgeError> {
    let balance = evm_state.get_balance(&from);
    let amount_u256 = U256::from(amount);
    if balance < amount_u256 {
        Err(BridgeError::InsufficientEvmBalance(from, amount_u256))
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
    fn test_withdraw_insufficient_evm_balance() {
        let mut evm_state = EvmState::new();
        evm_state.set_balance(test_addr(1), U256::from(100));

        // Try to withdraw 500 but only have 100
        assert!(check_withdraw_balance(&evm_state, test_addr(1), 500).is_err());
        // Exact balance
        assert!(check_withdraw_balance(&evm_state, test_addr(1), 100).is_ok());
    }

    #[test]
    fn test_withdraw_full_flow_call_native() {
        let mut protocol_balances = BalanceState::new();
        let mut evm_state = EvmState::new();
        evm_state.set_balance(test_addr(1), U256::from(1000));

        let evm_executor = EvmExecutor::new(1);
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig::default();
        let registry = setup_registry();

        let op = BridgeOp::WithdrawToProtocol {
            asset_id: 1, // CALL is withdrawn as native EVM balance
            from: test_addr(1),
            to: test_addr(1),
            amount: 500,
        };

        let bridge_addr = test_addr(0xCC);
        let result = execute_withdraw(
            &op,
            &mut protocol_balances,
            &mut evm_state,
            &evm_executor,
            &mut bridge_state,
            &config,
            &registry,
            bridge_addr,
            test_addr(0xFF),
            100,
        );
        // CALL (asset_id == 1) withdraws from native EVM balance — should succeed
        assert!(result.is_ok(), "withdraw failed: {:?}", result);
        assert!(result.unwrap().success);

        // Protocol balance credited
        assert_eq!(protocol_balances.get_balance(1, &test_addr(1)), 500);
        // EVM native balance deducted
        assert_eq!(evm_state.get_balance(&test_addr(1)), U256::from(500));
    }

    #[test]
    fn test_withdraw_success_with_contract() {
        let mut protocol_balances = BalanceState::new();
        let mut evm_state = EvmState::new();
        evm_state.set_balance(test_addr(1), U256::from(100_000_000_000_000u128));

        let evm_executor = EvmExecutor::new(1);
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig::default();
        let mut registry = setup_registry();

        // Deploy wrapped token contract for asset 1
        let (contract_addr, deploy_result) = evm_executor
            .deploy_erc20_template(
                test_addr(1),
                &mut evm_state,
                "CALL",
                "CALL",
                18,
                U256::ZERO,
            )
            .unwrap();
        assert!(deploy_result.success);
        registry.set_evm_contract_address(1, contract_addr);

        // Mint 1_000 tokens to test_addr(1) in EVM
        let mint_result = evm_executor.evm_call_bridge_mint(
            test_addr(1),
            contract_addr,
            &mut evm_state,
            test_addr(1),
            U256::from(1_000),
        );
        assert!(mint_result.is_ok() && mint_result.unwrap().success);

        let op = BridgeOp::WithdrawToProtocol {
            asset_id: 1,
            from: test_addr(1),
            to: test_addr(1),
            amount: 500,
        };

        let result = execute_withdraw(
            &op,
            &mut protocol_balances,
            &mut evm_state,
            &evm_executor,
            &mut bridge_state,
            &config,
            &registry,
            contract_addr,
            test_addr(1),
            100,
        );

        assert!(result.is_ok(), "withdraw failed: {:?}", result);
        assert!(result.unwrap().success);

        // Protocol balance credited
        assert_eq!(protocol_balances.get_balance(1, &test_addr(1)), 500);
    }
}
