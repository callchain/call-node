//! Shared nested EVM call infrastructure for precompiles.
//!
//! Enables precompiles to execute mutating EVM calls against arbitrary
//! contracts using a temporary revm instance backed by the live journal.

use alloy_primitives::{keccak256, Address, Bytes, U256};
use revm::database_interface::Database;
use revm::primitives::{B256, TxKind};
use revm::{ExecuteEvm, MainBuilder, MainContext};
use revm_precompile::PrecompileError;

use crate::storage::StorageProvider;

/// Maximum gas allowed for a single nested EVM call.
const NESTED_CALL_GAS_LIMIT: u64 = 100_000;

// ── Database adapter ──────────────────────────────────────────────────

/// [`Database`] implementation backed by a [`StorageProvider`].
///
/// Uses `raw_*` methods so that gas is **not** double-counted: the nested EVM
/// tracks its own gas, and only the final `gas.spent()` is deducted from the
/// outer provider.
pub struct StorageProviderDb<'a> {
    pub provider: &'a mut dyn StorageProvider,
}

impl<'a> Database for StorageProviderDb<'a> {
    type Error = revm::database_interface::ErasedError;

    fn basic(
        &mut self,
        address: Address,
    ) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        let balance = self
            .provider
            .raw_balance_get(address)
            .map_err(revm::database_interface::ErasedError::new)?;
        let code = self
            .provider
            .raw_code_get(address)
            .map_err(revm::database_interface::ErasedError::new)?;
        let code_hash = if code.is_empty() {
            keccak256(&[])
        } else {
            keccak256(&code)
        };
        let code = if code.is_empty() {
            None
        } else {
            Some(revm::state::Bytecode::new_legacy(code))
        };
        Ok(Some(revm::state::AccountInfo {
            balance,
            nonce: 0,
            code_hash,
            code,
            account_id: None,
        }))
    }

    fn code_by_hash(
        &mut self,
        _hash: B256,
    ) -> Result<revm::state::Bytecode, Self::Error> {
        Ok(revm::state::Bytecode::default())
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        self.provider
            .raw_sload(address, index)
            .map_err(revm::database_interface::ErasedError::new)
    }

    fn block_hash(&mut self, _number: u64) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

// ── Nested EVM call helpers ───────────────────────────────────────────

/// Execute a mutating EVM call against `contract` with `data`.
///
/// The call is sent from `caller` with `gas_limit = 100_000`.
/// Actual gas spent is deducted from the outer `StorageProvider`.
pub fn execute_evm_call(
    db: &mut StorageProviderDb,
    caller: Address,
    contract: Address,
    data: Bytes,
) -> Result<(revm::context_interface::result::ExecutionResult, revm::state::EvmState), PrecompileError>
{
    let block_number = db.provider.block_number();
    let timestamp = db.provider.timestamp();

    let tx_env = revm::context::TxEnv::builder()
        .caller(caller)
        .gas_limit(NESTED_CALL_GAS_LIMIT)
        .gas_price(0)
        .kind(TxKind::Call(contract))
        .value(U256::ZERO)
        .data(data)
        .build()
        .map_err(|e| PrecompileError::Other(format!("tx build: {e:?}").into()))?;

    let result = {
        let ctx = revm::Context::mainnet()
            .with_db(&mut *db)
            .modify_cfg_chained(|cfg| {
                cfg.set_spec(revm::primitives::hardfork::SpecId::CANCUN)
            });
        let mut evm = ctx.build_mainnet();
        let mut block_env = revm::context::BlockEnv::default();
        block_env.number = U256::from(block_number);
        block_env.timestamp = timestamp;
        evm.set_block(block_env);
        evm.transact(tx_env)
            .map_err(|e| PrecompileError::Other(format!("evm call: {e:?}").into()))?
    };

    let gas_spent = match &result.result {
        revm::context_interface::result::ExecutionResult::Success { gas, .. } => gas.spent(),
        revm::context_interface::result::ExecutionResult::Revert { gas, .. } => gas.spent(),
        revm::context_interface::result::ExecutionResult::Halt { gas, .. } => gas.spent(),
    };
    db.provider.charge_gas(gas_spent)?;

    Ok((result.result, result.state))
}

/// Apply nested EVM state changes back to the outer `StorageProvider`.
pub fn apply_state_changes(
    storage: &mut dyn StorageProvider,
    state: revm::state::EvmState,
) -> Result<(), PrecompileError> {
    for (address, account) in state {
        if !account.status.is_touched() {
            continue;
        }

        // Apply storage changes
        for (slot, storage_slot) in account.storage {
            if storage_slot.present_value != storage_slot.original_value {
                storage.raw_sstore(address, slot, storage_slot.present_value)?;
            }
        }

        // Apply balance changes
        let old_balance = account.original_info.balance;
        let new_balance = account.info.balance;
        if old_balance != new_balance {
            if new_balance > old_balance {
                storage.balance_add(address, new_balance - old_balance)?;
            } else {
                storage.balance_sub(address, old_balance - new_balance)?;
            }
        }
    }
    Ok(())
}
