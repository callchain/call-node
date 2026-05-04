//! EVM executor wrapping Revm (per spec §4)
//!
//! EVM transaction execution, ERC-20 deployment, gas tracking, validation.

use call_primitives::Address;
use call_precompile::{
    CallPrecompiles,
    AGENT_ADDRESS, BRIDGE_ADDRESS, COMPLIANCE_ADDRESS, GOVERNANCE_ADDRESS,
    ORACLE_ADDRESS, SHIELDED_ADDRESS, SWITCH_ADDRESS, VALIDATOR_ADDRESS,
    ASSET_ADDRESS,
};
use call_switch::precompile::SwitchPrecompile;
use call_bridge::precompile::BridgePrecompile;
use call_shielded::precompile::ShieldedPrecompile;
use call_asset::AssetPrecompile;
use call_validator::ValidatorPrecompile;
use call_compliance::CompliancePrecompile;
use call_agent::AgentPrecompile;
use call_oracle::precompile::OraclePrecompile;
use call_governance::precompile::GovernancePrecompile;
use alloy_primitives::{U256, Bytes, keccak256, FixedBytes};
use revm::{
    database::InMemoryDB,
    primitives::{hardfork::SpecId, TxKind, Log},
    Context, ExecuteEvm, MainBuilder, MainContext,
};
use crate::state::EvmState;

/// EVM execution error
#[derive(Debug)]
pub enum EvmError {
    InvalidTx(&'static str),
    ExecutionError(String),
}

impl core::fmt::Display for EvmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EvmError::InvalidTx(msg) => write!(f, "invalid tx: {msg}"),
            EvmError::ExecutionError(msg) => write!(f, "execution error: {msg}"),
        }
    }
}

impl std::error::Error for EvmError {}

/// EVM execution result
#[derive(Debug)]
pub struct EvmExecutionResult {
    pub success: bool,
    pub gas_used: u64,
    pub output: Bytes,
    pub logs: Vec<Log>,
}

/// EVM transaction input
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvmTransaction {
    pub caller: Address,
    pub nonce: u64,
    pub gas_limit: u64,
    pub gas_price: u128,
    pub to: Option<Address>,
    pub value: U256,
    pub data: Bytes,
    pub chain_id: u64,
}

/// EVM executor
#[derive(Debug)]
pub struct EvmExecutor {
    pub chain_id: u64,
    pub spec_id: SpecId,
}

impl EvmExecutor {
    pub fn new(chain_id: u64) -> Self {
        Self {
            chain_id,
            spec_id: SpecId::CANCUN,
        }
    }

    /// Execute an EVM transaction through revm with Callchain custom precompiles.
    ///
    /// Custom precompiles at 0x101 (Oracle), 0x102 (Balance), and 0x103 (Bridge)
    /// are executed via revm's normal call-frame mechanism with proper gas
    /// accounting, state isolation, and call depth tracking.
    pub fn execute_tx(
        &self,
        tx: EvmTransaction,
        state: &mut EvmState,
        block_number: u64,
        base_fee: u128,
    ) -> Result<EvmExecutionResult, EvmError> {
        // Build revm InMemoryDB and sync our state into it
        let mut db = InMemoryDB::default();
        state.sync_to_revm_db(&mut db);

        let tx_env = revm::context::TxEnv::builder()
            .caller(tx.caller)
            .gas_limit(tx.gas_limit)
            .gas_price(tx.gas_price)
            .kind(tx.to.map(TxKind::Call).unwrap_or(TxKind::Create))
            .value(tx.value)
            .data(tx.data.clone())
            .nonce(tx.nonce)
            .chain_id(Some(tx.chain_id))
            .build()
            .map_err(|_| EvmError::InvalidTx("tx build failed"))?;

        let ctx = Context::mainnet()
            .with_db(db)
            .modify_cfg_chained(|cfg| cfg.set_spec(self.spec_id));

        // Build EVM with Callchain custom precompiles
        let precompiles = CallPrecompiles::new(self.spec_id)
            .with_custom(ORACLE_ADDRESS,     Box::new(OraclePrecompile))
            .with_custom(BRIDGE_ADDRESS,     Box::new(BridgePrecompile))
            .with_custom(ASSET_ADDRESS,      Box::new(AssetPrecompile))
            .with_custom(SHIELDED_ADDRESS,   Box::new(ShieldedPrecompile))
            .with_custom(GOVERNANCE_ADDRESS, Box::new(GovernancePrecompile))
            .with_custom(VALIDATOR_ADDRESS,  Box::new(ValidatorPrecompile))
            .with_custom(COMPLIANCE_ADDRESS, Box::new(CompliancePrecompile))
            .with_custom(SWITCH_ADDRESS,     Box::new(SwitchPrecompile))
            .with_custom(AGENT_ADDRESS,      Box::new(AgentPrecompile));
        let mut evm = ctx.build_mainnet().with_precompiles(precompiles);

        let mut block_env = revm::context::BlockEnv::default();
        block_env.number = U256::from(block_number);
        block_env.basefee = base_fee as u64;
        evm.set_block(block_env);

        let result = evm
            .transact(tx_env)
            .map_err(|e| EvmError::ExecutionError(format!("{e:?}")))?;

        // Apply revm state changes back to our EvmState
        state.apply_from_revm_state(&result.state);

        let (success, output, gas_used, logs) = match result.result {
            revm::context_interface::result::ExecutionResult::Success {
                gas,
                output,
                logs,
                ..
            } => {
                let bytes = match output {
                    revm::context_interface::result::Output::Call(b) => b,
                    revm::context_interface::result::Output::Create(b, _) => b,
                };
                (true, bytes, gas.spent(), logs)
            }
            revm::context_interface::result::ExecutionResult::Revert {
                gas,
                output,
                ..
            } => (false, output, gas.spent(), vec![]),
            revm::context_interface::result::ExecutionResult::Halt { gas, .. } => {
                (false, Bytes::default(), gas.spent(), vec![])
            }
        };

        Ok(EvmExecutionResult {
            success,
            gas_used,
            output,
            logs,
        })
    }

    /// Deploy ERC-20 template contract for an asset.
    ///
    /// Uses pre-compiled `WrappedToken.sol` bytecode with ABI-encoded
    /// constructor arguments `(name, symbol, decimals, bridge, issuer, maxSupply, assetId)`.
    pub fn deploy_erc20_template(
        &self,
        deployer: Address,
        state: &mut EvmState,
        name: &str,
        symbol: &str,
        decimals: u8,
        bridge: Address,
        issuer: Address,
        max_supply: U256,
        asset_id: U256,
    ) -> Result<(Address, EvmExecutionResult), EvmError> {
        let init_code = crate::erc20_bytecode::build_erc20_init_code(
            name, symbol, decimals, bridge, issuer, max_supply, asset_id,
        );

        // Derive CREATE contract address from deployer + nonce
        let nonce = state.get_nonce(&deployer);
        let contract_addr = derive_create_address(deployer, nonce);

        let tx = EvmTransaction {
            caller: deployer,
            nonce,
            gas_limit: 10_000_000,
            gas_price: 10,
            to: None,
            value: U256::ZERO,
            data: init_code,
            chain_id: self.chain_id,
        };

        let result = self.execute_tx(tx, state, 0, 0)?;

        // Revm already sets the deployed code via apply_from_revm_state,
        // but we keep this as a safety net for CREATE output.
        if !result.output.is_empty() {
            state.set_code(contract_addr, result.output.clone());
        }

        Ok((contract_addr, result))
    }

    /// Helper: EVM call for bridge mint operations
    pub fn evm_call_bridge_mint(
        &self,
        caller: Address,
        contract: Address,
        state: &mut EvmState,
        to: Address,
        amount: U256,
    ) -> Result<EvmExecutionResult, EvmError> {
        // keccak256("bridgeMint(address,uint256)")[:4]
        let selector: FixedBytes<4> = FixedBytes::from_slice(
            &keccak256("bridgeMint(address,uint256)")[..4],
        );
        let mut data = Vec::new();
        data.extend_from_slice(&selector[..]);
        // ABI-encode address (32 bytes, left-padded)
        let mut addr_bytes = [0u8; 32];
        addr_bytes[12..].copy_from_slice(to.as_slice());
        data.extend_from_slice(&addr_bytes);
        // ABI-encode uint256
        data.extend_from_slice(&amount.to_be_bytes::<32>());

        let tx = EvmTransaction {
            caller,
            nonce: state.get_nonce(&caller),
            gas_limit: 500_000,
            gas_price: 10,
            to: Some(contract),
            value: U256::ZERO,
            data: Bytes::from(data),
            chain_id: self.chain_id,
        };

        self.execute_tx(tx, state, 0, 0)
    }

    /// Helper: EVM call for bridge burn operations
    pub fn evm_call_bridge_burn(
        &self,
        caller: Address,
        contract: Address,
        state: &mut EvmState,
        amount: U256,
    ) -> Result<EvmExecutionResult, EvmError> {
        // keccak256("bridgeBurn(uint256)")[:4]
        let selector: FixedBytes<4> = FixedBytes::from_slice(
            &keccak256("bridgeBurn(uint256)")[..4],
        );
        let mut data = Vec::new();
        data.extend_from_slice(&selector[..]);
        // ABI-encode uint256
        data.extend_from_slice(&amount.to_be_bytes::<32>());

        let tx = EvmTransaction {
            caller,
            nonce: state.get_nonce(&caller),
            gas_limit: 500_000,
            gas_price: 10,
            to: Some(contract),
            value: U256::ZERO,
            data: Bytes::from(data),
            chain_id: self.chain_id,
        };

        self.execute_tx(tx, state, 0, 0)
    }

    /// Helper: EVM call for issuer mint operations on WrappedToken
    pub fn evm_call_issuer_mint(
        &self,
        caller: Address,
        contract: Address,
        state: &mut EvmState,
        to: Address,
        amount: U256,
    ) -> Result<EvmExecutionResult, EvmError> {
        // keccak256("issuerMint(address,uint256)")[:4]
        let selector: FixedBytes<4> = FixedBytes::from_slice(
            &keccak256("issuerMint(address,uint256)")[..4],
        );
        let mut data = Vec::new();
        data.extend_from_slice(&selector[..]);
        // ABI-encode address (32 bytes, left-padded)
        let mut addr_bytes = [0u8; 32];
        addr_bytes[12..].copy_from_slice(to.as_slice());
        data.extend_from_slice(&addr_bytes);
        // ABI-encode uint256
        data.extend_from_slice(&amount.to_be_bytes::<32>());

        let tx = EvmTransaction {
            caller,
            nonce: state.get_nonce(&caller),
            gas_limit: 500_000,
            gas_price: 10,
            to: Some(contract),
            value: U256::ZERO,
            data: Bytes::from(data),
            chain_id: self.chain_id,
        };

        self.execute_tx(tx, state, 0, 0)
    }
}

// ── Transaction Validation ────────────────────────────────────────────

/// Validate an EVM transaction before execution
pub fn validate_evm_tx(
    tx: &EvmTransaction,
    state: &EvmState,
) -> Result<(), &'static str> {
    let expected_nonce = state.get_nonce(&tx.caller);
    if tx.nonce != expected_nonce {
        return Err("invalid nonce");
    }

    let max_gas_cost = U256::from(tx.gas_limit) * U256::from(tx.gas_price);
    let required = tx.value + max_gas_cost;
    // EvmState balance is U256, but we set it with small values in tests.
    // The validation uses the correct math.
    let balance = state.get_balance(&tx.caller);
    if balance < required {
        return Err("insufficient balance");
    }

    Ok(())
}

// ── ERC-20 Init Code ─────────────────────────────────────────────────

/// Derive CREATE opcode contract address from deployer + nonce
pub fn derive_create_address(deployer: Address, nonce: u64) -> Address {
    // CREATE address = keccak256(rlp(deployer, nonce))[12:]

    // RLP encode [deployer, nonce]
    let mut rlp_buf = Vec::new();

    // RLP encode the address (20 bytes, RLP prefix + data)
    rlp_buf.push(0x80 | 20); // RLP string of length 20
    rlp_buf.extend_from_slice(deployer.as_slice());

    // RLP encode the nonce
    let nonce_bytes = if nonce == 0 {
        vec![]
    } else {
        nonce.to_be_bytes().iter().skip_while(|&&b| b == 0).cloned().collect::<Vec<_>>()
    };
    if nonce_bytes.is_empty() {
        rlp_buf.push(0x80); // RLP empty string
    } else if nonce_bytes.len() == 1 {
        rlp_buf.push(0x80 | nonce_bytes[0]); // single byte, inline
    } else {
        rlp_buf.push(0x80 | nonce_bytes.len() as u8);
        rlp_buf.extend_from_slice(&nonce_bytes);
    }

    // RLP list prefix
    let list = {
        let mut out = Vec::new();
        if rlp_buf.len() < 56 {
            out.push(0xc0 | rlp_buf.len() as u8);
        } else {
            let len_bytes = rlp_buf.len().to_be_bytes();
            let first_nonzero = len_bytes.iter().position(|&b| b != 0).unwrap_or(len_bytes.len());
            let payload_len = len_bytes.len() - first_nonzero;
            out.push(0xf7 | payload_len as u8);
            out.extend_from_slice(&len_bytes[first_nonzero..]);
        }
        out.extend(rlp_buf);
        out
    };

    let hash = keccak256(&list);
    Address::from_slice(&hash[12..])
}

// ── Gas Tracking ──────────────────────────────────────────────────────

/// Track gas usage within a block
#[derive(Debug, Default)]
pub struct BlockGasTracker {
    pub gas_used: u64,
    pub gas_limit: u64,
}

impl BlockGasTracker {
    pub fn new(gas_limit: u64) -> Self {
        Self {
            gas_used: 0,
            gas_limit,
        }
    }

    pub fn add_gas(&mut self, used: u64) -> Result<(), &'static str> {
        if self.gas_used + used > self.gas_limit {
            return Err("block gas limit exceeded");
        }
        self.gas_used += used;
        Ok(())
    }

    pub fn remaining(&self) -> u64 {
        self.gas_limit - self.gas_used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_evm_nonce_validation() {
        let mut state = EvmState::new();
        let addr = test_addr(1);
        state.create_account(addr);
        state.set_balance(addr, U256::from(1_000_000_000i128));

        let tx = EvmTransaction {
            caller: addr,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            to: Some(test_addr(2)),
            value: U256::ZERO,
            data: Bytes::default(),
            chain_id: 1,
        };

        assert!(validate_evm_tx(&tx, &state).is_ok());
    }

    #[test]
    fn test_evm_insufficient_balance() {
        let mut state = EvmState::new();
        let addr = test_addr(1);
        state.create_account(addr);

        let tx = EvmTransaction {
            caller: addr,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            to: Some(test_addr(2)),
            value: U256::from(1000),
            data: Bytes::default(),
            chain_id: 1,
        };

        assert!(validate_evm_tx(&tx, &state).is_err());
    }

    #[test]
    fn test_evm_gas_tracking() {
        let mut tracker = BlockGasTracker::new(10_000_000);
        tracker.add_gas(3_000_000).unwrap();
        tracker.add_gas(5_000_000).unwrap();
        assert_eq!(tracker.gas_used, 8_000_000);
        assert_eq!(tracker.remaining(), 2_000_000);
        assert!(tracker.add_gas(3_000_000).is_err());
    }

    #[test]
    fn test_evm_execute_transfer() {
        let executor = EvmExecutor::new(1);
        let mut state = EvmState::new();

        let caller = test_addr(1);
        state.set_balance(caller, U256::from(1_000_000_000i128));
        state.create_account(caller);

        let tx = EvmTransaction {
            caller,
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 10,
            to: Some(test_addr(2)),
            value: U256::from(100),
            data: Bytes::default(),
            chain_id: 1,
        };

        let result = executor.execute_tx(tx, &mut state, 0, 0);
        assert!(result.is_ok());
    }

    #[test]
    fn test_evm_deploy_erc20_template() {
        let executor = EvmExecutor::new(1);
        let mut state = EvmState::new();
        let deployer = test_addr(1);
        state.set_balance(deployer, U256::from(100_000_000_000i128));
        state.create_account(deployer);

        let expected_addr = derive_create_address(deployer, 0);

        let bridge = test_addr(0xFF);
        let issuer = test_addr(1);
        let (addr, result) = executor
            .deploy_erc20_template(deployer, &mut state, "Test", "TST", 18, bridge, issuer, U256::from(0), U256::from(1))
            .unwrap();
        assert_eq!(addr, expected_addr);
        assert!(result.success, "ERC-20 deploy failed: gas_used={}, output={:?}", result.gas_used, result.output);
    }

    #[test]
    fn test_evm_call_bridge_mint() {
        let executor = EvmExecutor::new(1);
        let mut state = EvmState::new();
        let caller = test_addr(1);
        state.set_balance(caller, U256::from(10_000_000_000i128));
        state.create_account(caller);

        let result = executor
            .evm_call_bridge_mint(caller, test_addr(0xCC), &mut state, test_addr(2), U256::from(500))
            .unwrap();
        assert!(result.success);
    }
}
