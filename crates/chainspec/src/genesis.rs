//! T10.1 — Genesis Initialization (per spec §16)
//!
//! Genesis struct with JSON parsing, initialization flow,
//! state root computation, and chain ID management.

use call_consensus::proposer::ConsensusParams;
use call_crypto::keccak256;
use call_evm::EvmExecutor;
use call_evm::provider::InMemoryStateProvider;
use call_primitives::{Address, AssetId, Balance, Ed25519PublicKey, Hash};
use call_protocol::gas::FeeParams;
use alloy_primitives::U256;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use call_consensus::exec::state_accessors;

// ── Genesis Types ─────────────────────────────────────────────────────

/// Genesis asset entry (per spec §16.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisAsset {
    /// Asset identifier
    pub asset_id: AssetId,
    /// Token symbol
    pub symbol: String,
    /// Token name
    pub name: String,
    /// Number of decimal places
    pub decimals: u8,
    /// Total initial supply
    pub initial_supply: Balance,
    /// Initial distribution: address -> amount
    pub distribution: HashMap<String, Balance>,
}

/// Genesis fee currency entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisFeeCurrency {
    /// Currency symbol (e.g., "CALL", "USDC")
    pub symbol: String,
    /// Asset ID for this currency
    pub asset_id: AssetId,
    /// Oracle address for price feeds (if stablecoin)
    pub oracle_address: Option<String>,
}

/// Genesis validator entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisValidator {
    /// Validator address (hex)
    pub address: String,
    /// Ed25519 public key (hex)
    pub ed25519_pubkey: String,
    /// Self-stake amount in CALL (18 decimals)
    pub self_stake: Balance,
}

/// Full Genesis configuration (per spec §16)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Genesis {
    /// Genesis format version
    pub version: u32,
    /// Chain name (e.g., "callchain-mainnet")
    pub chain_name: String,
    /// Unique chain identifier
    pub chain_id: u64,
    /// Genesis timestamp in milliseconds
    pub timestamp_millis: u64,
    /// Initial assets and their distribution
    pub initial_assets: Vec<GenesisAsset>,
    /// Supported fee currencies
    pub initial_fee_currencies: Vec<GenesisFeeCurrency>,
    /// Genesis validators
    pub validators: Vec<GenesisValidator>,
    /// Consensus parameters
    pub consensus_params: ConsensusParams,
    /// Fee parameters
    #[serde(skip, default = "FeeParams::default")]
    pub fee_params: FeeParams,
    /// Asset IDs to track for oracle price submissions
    #[serde(default)]
    pub oracle_assets: Option<Vec<AssetId>>,
}

impl Genesis {
    /// Create a new genesis configuration
    pub fn new(chain_name: &str, chain_id: u64, timestamp_millis: u64) -> Self {
        Self {
            version: 1,
            chain_name: chain_name.to_string(),
            chain_id,
            timestamp_millis,
            initial_assets: Vec::new(),
            initial_fee_currencies: Vec::new(),
            validators: Vec::new(),
            consensus_params: ConsensusParams::default(),
            fee_params: FeeParams::default(),
            oracle_assets: None,
        }
    }

    /// Add an initial asset
    pub fn with_asset(mut self, asset: GenesisAsset) -> Self {
        self.initial_assets.push(asset);
        self
    }

    /// Add a fee currency
    pub fn with_fee_currency(mut self, currency: GenesisFeeCurrency) -> Self {
        self.initial_fee_currencies.push(currency);
        self
    }

    /// Add a genesis validator
    pub fn with_validator(mut self, validator: GenesisValidator) -> Self {
        self.validators.push(validator);
        self
    }

    /// Set consensus parameters
    pub fn with_consensus_params(mut self, params: ConsensusParams) -> Self {
        self.consensus_params = params;
        self
    }

    /// Set fee parameters
    pub fn with_fee_params(mut self, params: FeeParams) -> Self {
        self.fee_params = params;
        self
    }

    /// Parse genesis from JSON string
    pub fn from_json(json: &str) -> Result<Self, GenesisError> {
        serde_json::from_str(json).map_err(|e| GenesisError::InvalidJson(e.to_string()))
    }

    /// Serialize genesis to JSON string
    pub fn to_json(&self) -> Result<String, GenesisError> {
        serde_json::to_string_pretty(self).map_err(|e| GenesisError::InvalidJson(e.to_string()))
    }

    /// Load genesis from a JSON file path.
    pub fn load_from_file(path: impl AsRef<std::path::Path>) -> Result<Self, GenesisError> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| GenesisError::InvalidJson(format!("read file: {e}")))?;
        Self::from_json(&json)
    }
}

// ── Genesis Error ─────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    #[error("invalid genesis JSON: {0}")]
    InvalidJson(String),
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("invalid public key: {0}")]
    InvalidPublicKey(String),
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
    #[error("chain ID mismatch: expected {expected}, got {actual}")]
    ChainIdMismatch { expected: u64, actual: u64 },
}

// ── Genesis State ─────────────────────────────────────────────────────

/// Initialized genesis state with all computed roots
pub struct GenesisState {
    /// Unified state root
    pub state_root: Hash,
    /// EVM state
    pub evm_state: InMemoryStateProvider,
    /// Registered fee currency asset IDs
    pub fee_currencies: Vec<AssetId>,
}

// ── Genesis Executor ──────────────────────────────────────────────────

/// Executes genesis initialization flow (per spec §16.3)
pub struct GenesisExecutor {
    genesis: Genesis,
}

impl GenesisExecutor {
    /// Create a new genesis executor
    pub fn new(genesis: Genesis) -> Self {
        Self { genesis }
    }

    /// Execute the genesis initialization flow.
    ///
    /// Per spec §16.3:
    /// 1. Parse genesis and validate schema
    /// 2. Initialize state tables
    /// 3. Register assets
    /// 4. Register validators
    /// 5. Register fee currencies
    /// 6. Deploy EVM ERC-20 templates
    /// 7. Compute initial state roots
    pub fn execute(&self) -> Result<GenesisState, GenesisError> {
        // Step 1: Validate genesis schema
        self.validate()?;

        // Step 2: Initialize state tables
        let mut evm_state = InMemoryStateProvider::new();

        // Step 3: Register assets and distribute initial balances (EVM only)
        self.register_assets(&mut evm_state)?;

        // Step 4: Register validators (EVM only)
        self.register_validators(&mut evm_state)?;

        // Step 5: Register fee currencies
        let fee_currencies = self.register_fee_currencies(&evm_state)?;

        // Step 6: Deploy EVM ERC-20 templates for non-CALL genesis assets
        self.deploy_evm_templates(&mut evm_state)?;

        // Step 7: Compute initial state root (EVM-only)
        let state_root = compute_evm_state_root(&evm_state);

        Ok(GenesisState {
            state_root,
            evm_state,
            fee_currencies,
        })
    }

    /// Validate genesis schema
    fn validate(&self) -> Result<(), GenesisError> {
        if self.genesis.chain_name.is_empty() {
            return Err(GenesisError::InvalidJson("chain_name is empty".into()));
        }
        if self.genesis.chain_id == 0 {
            return Err(GenesisError::InvalidJson("chain_id must be non-zero".into()));
        }
        if self.genesis.timestamp_millis == 0 {
            return Err(GenesisError::InvalidJson("timestamp_millis must be non-zero".into()));
        }
        if self.genesis.validators.is_empty() {
            return Err(GenesisError::InvalidJson("no genesis validators".into()));
        }
        if self.genesis.initial_assets.is_empty() {
            return Err(GenesisError::InvalidJson("no initial assets".into()));
        }
        if self.genesis.initial_fee_currencies.is_empty() {
            return Err(GenesisError::InvalidJson("no fee currencies".into()));
        }
        // Validate asset IDs are unique
        let mut seen = std::collections::HashSet::new();
        for asset in &self.genesis.initial_assets {
            if !seen.insert(asset.asset_id) {
                return Err(GenesisError::InvalidJson(format!(
                    "duplicate asset_id: {}",
                    asset.asset_id
                )));
            }
        }
        Ok(())
    }

    /// Register assets and distribute initial balances (EVM only)
    fn register_assets(
        &self,
        evm_state: &mut InMemoryStateProvider,
    ) -> Result<(), GenesisError> {
        for asset in &self.genesis.initial_assets {
            // Distribute initial balances (EVM only)
            let mut total_allocated: Balance = 0;
            for (address_str, amount) in &asset.distribution {
                let addr = parse_address(address_str)?;
                total_allocated = total_allocated
                    .checked_add(*amount)
                    .ok_or_else(|| GenesisError::ExecutionFailed("supply overflow".into()))?;

                // Seed EVM asset storage so get_balance reads from EVM state root
                state_accessors::seed_balance(evm_state, asset.asset_id, addr, *amount);

                // Seed native EVM balance for gas payment
                evm_state.set_balance(addr, U256::from(*amount));
                evm_state.create_account(addr);
            }

            // Seed EVM asset metadata
            state_accessors::seed_asset(
                evm_state,
                asset.asset_id,
                &asset.symbol,
                &asset.name,
                asset.decimals,
                Address::ZERO,
                0,
                total_allocated,
                0, // active
            );
        }
        Ok(())
    }

    /// Register genesis validators (EVM + legacy)
    fn register_validators(
        &self,
        evm_state: &mut InMemoryStateProvider,
    ) -> Result<(), GenesisError> {
        use call_precompile::{address_to_u256, u128_to_u256, u64_to_u256, VALIDATOR_ADDRESS};
        use call_precompile::storage::storage_slot;

        for (i, gv) in self.genesis.validators.iter().enumerate() {
            let addr = parse_address(&gv.address)?;
            let pubkey = parse_pubkey(&gv.ed25519_pubkey)?;

            // EVM validator slots
            let validator_id = (i + 1) as u64;
            evm_state.set_storage(VALIDATOR_ADDRESS, U256::ZERO, u64_to_u256(validator_id));
            evm_state.set_storage(
                VALIDATOR_ADDRESS,
                storage_slot(&[addr.as_slice(), b"validator_id"]),
                u64_to_u256(validator_id),
            );
            evm_state.set_storage(
                VALIDATOR_ADDRESS,
                storage_slot(&[b"validators"]) + U256::from(validator_id),
                address_to_u256(addr),
            );
            evm_state.set_storage(
                VALIDATOR_ADDRESS,
                storage_slot(&[addr.as_slice(), b"stake"]),
                u128_to_u256(gv.self_stake),
            );
            evm_state.set_storage(
                VALIDATOR_ADDRESS,
                storage_slot(&[addr.as_slice(), b"pubkey"]),
                U256::from_be_slice(&pubkey),
            );
            evm_state.set_storage(
                VALIDATOR_ADDRESS,
                storage_slot(&[addr.as_slice(), b"status"]),
                U256::from(1u8), // active
            );

            // Seed staking escrow balance
            state_accessors::seed_balance(
                evm_state,
                call_protocol::CALL_ASSET_ID,
                call_consensus::STAKING_ESCROW,
                gv.self_stake,
            );
        }
        Ok(())
    }

    /// Register fee currencies
    fn register_fee_currencies(
        &self,
        _evm_state: &InMemoryStateProvider,
    ) -> Result<Vec<AssetId>, GenesisError> {
        let mut ids = Vec::new();
        for currency in &self.genesis.initial_fee_currencies {
            ids.push(currency.asset_id);
        }
        Ok(ids)
    }

    /// Deploy EVM ERC-20 templates for non-CALL genesis assets.
    /// CALL (asset_id == 1) bridges as native EVM balance and does not
    /// require a WrappedToken contract.
    fn deploy_evm_templates(
        &self,
        evm_state: &mut InMemoryStateProvider,
    ) -> Result<(), GenesisError> {
        let executor = EvmExecutor::new(self.genesis.chain_id);

        for asset in &self.genesis.initial_assets {
            if asset.asset_id == call_protocol::CALL_ASSET_ID {
                // CALL bridges as native EVM balance — no WrappedToken needed
                continue;
            }

            let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
            evm_state.set_balance(deployer, U256::from(100_000_000_000u128));
            evm_state.create_account(deployer);

            let (contract_addr, deploy_result) = executor
                .deploy_erc20_template(
                    deployer,
                    evm_state,
                    &asset.name,
                    &asset.symbol,
                    asset.decimals,
                    call_protocol::BRIDGE_EVM_ADDRESS,
                    Address::ZERO,
                    alloy_primitives::U256::ZERO,
                    alloy_primitives::U256::from(asset.asset_id),
                )
                .map_err(|e| GenesisError::ExecutionFailed(e.to_string()))?;

            if !deploy_result.success {
                return Err(GenesisError::ExecutionFailed(
                    "ERC-20 deployment reverted".into(),
                ));
            }

            state_accessors::seed_asset_contract_address(evm_state, asset.asset_id, contract_addr);
        }

        Ok(())
    }
}

// ── State Root Computation ────────────────────────────────────────────

/// Compute EVM state root
pub fn compute_evm_state_root(evm_state: &InMemoryStateProvider) -> Hash {
    let mut data = Vec::new();
    let mut entries: Vec<_> = evm_state.get_all_accounts().iter().collect();
    entries.sort_by_key(|(addr, _)| **addr);

    for (addr, account) in entries {
        data.extend_from_slice(addr.as_slice());
        data.extend_from_slice(&account.nonce.to_le_bytes());
        data.extend_from_slice(&account.balance.to_be_bytes::<32>());
        data.extend_from_slice(&keccak256(&account.code).0);
    }

    if data.is_empty() {
        return Hash::ZERO;
    }

    keccak256(&data)
}

// ── Helpers ───────────────────────────────────────────────────────────

fn parse_address(s: &str) -> Result<Address, GenesisError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).map_err(|e| GenesisError::InvalidAddress(e.to_string()))?;
    if bytes.len() != 20 {
        return Err(GenesisError::InvalidAddress(format!(
            "expected 20 bytes, got {}",
            bytes.len()
        )));
    }
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&bytes);
    Ok(Address::from_slice(&addr))
}

fn parse_pubkey(s: &str) -> Result<Ed25519PublicKey, GenesisError> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).map_err(|e| GenesisError::InvalidPublicKey(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(GenesisError::InvalidPublicKey(format!(
            "expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&bytes);
    Ok(pubkey)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn addr_hex(addr: &Address) -> String {
        format!("0x{}", hex::encode(addr.as_slice()))
    }

    fn pubkey_hex() -> String {
        format!("0x{}", hex::encode([1u8; 32]))
    }

    fn make_test_genesis() -> Genesis {
        Genesis::new("callchain-test", 1, 1_700_000_000_000)
            .with_asset(GenesisAsset {
                asset_id: 1,
                symbol: "CALL".to_string(),
                name: "Call Token".to_string(),
                decimals: 18,
                initial_supply: 1_000_000_000 * 10u128.pow(18),
                distribution: {
                    let mut map = HashMap::new();
                    map.insert(addr_hex(&test_addr(1)), 500_000_000 * 10u128.pow(18));
                    map.insert(addr_hex(&test_addr(2)), 500_000_000 * 10u128.pow(18));
                    map
                },
            })
            .with_fee_currency(GenesisFeeCurrency {
                symbol: "CALL".to_string(),
                asset_id: 1,
                oracle_address: None,
            })
            .with_validator(GenesisValidator {
                address: addr_hex(&test_addr(10)),
                ed25519_pubkey: pubkey_hex(),
                self_stake: call_consensus::proposer::ConsensusParams::default().min_self_stake,
            })
    }

    #[test]
    fn test_genesis_json_parse() {
        let genesis = make_test_genesis();
        let json = genesis.to_json().unwrap();
        let parsed = Genesis::from_json(&json).unwrap();
        assert_eq!(parsed.chain_id, 1);
        assert_eq!(parsed.chain_name, "callchain-test");
        assert_eq!(parsed.initial_assets.len(), 1);
        assert_eq!(parsed.validators.len(), 1);
    }

    #[test]
    fn test_genesis_initialization() {
        let genesis = make_test_genesis();
        let executor = GenesisExecutor::new(genesis);
        let state = executor.execute().unwrap();

        assert_ne!(state.state_root, Hash::ZERO);
        assert_eq!(state.fee_currencies.len(), 1);
        assert_eq!(state.fee_currencies[0], 1);
    }

    #[test]
    fn test_genesis_evm_balances() {
        let genesis = make_test_genesis();
        let executor = GenesisExecutor::new(genesis);
        let state = executor.execute().unwrap();

        let bal1 = call_consensus::exec::state_accessors::read_balance(
            &state.evm_state, 1, test_addr(1));
        assert_eq!(bal1, 500_000_000 * 10u128.pow(18));

        let bal2 = call_consensus::exec::state_accessors::read_balance(
            &state.evm_state, 1, test_addr(2));
        assert_eq!(bal2, 500_000_000 * 10u128.pow(18));
    }

    #[test]
    fn test_genesis_evm_erc20_deploy() {
        let genesis = make_test_genesis();
        let executor = GenesisExecutor::new(genesis);
        let state = executor.execute().unwrap();

        assert!(!state.evm_state.get_all_accounts().is_empty());
    }

    #[test]
    fn test_genesis_validator_registration() {
        let genesis = make_test_genesis();
        let executor = GenesisExecutor::new(genesis);
        let state = executor.execute().unwrap();

        let count = call_consensus::exec::state_accessors::read_validator_count(&state.evm_state);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_genesis_fee_currency_registration() {
        let genesis = make_test_genesis();
        let executor = GenesisExecutor::new(genesis);
        let state = executor.execute().unwrap();

        assert_eq!(state.fee_currencies.len(), 1);
        assert_eq!(state.fee_currencies[0], 1);
    }

    #[test]
    fn test_genesis_state_root_computation() {
        let genesis = make_test_genesis();
        let executor = GenesisExecutor::new(genesis);
        let state = executor.execute().unwrap();

        let state2 = executor.execute().unwrap();
        assert_eq!(state.state_root, state2.state_root);
    }

    #[test]
    fn test_genesis_validation_empty_name() {
        let mut g = make_test_genesis();
        g.chain_name = String::new();
        let executor = GenesisExecutor::new(g);
        assert!(executor.execute().is_err());
    }

    #[test]
    fn test_genesis_validation_zero_chain_id() {
        let mut g = make_test_genesis();
        g.chain_id = 0;
        let executor = GenesisExecutor::new(g);
        assert!(executor.execute().is_err());
    }

    #[test]
    fn test_genesis_validation_no_validators() {
        let mut g = make_test_genesis();
        g.validators = vec![];
        let executor = GenesisExecutor::new(g);
        assert!(executor.execute().is_err());
    }

    #[test]
    fn test_genesis_validation_no_assets() {
        let mut g = make_test_genesis();
        g.initial_assets = vec![];
        let executor = GenesisExecutor::new(g);
        assert!(executor.execute().is_err());
    }

    #[test]
    fn test_genesis_validation_duplicate_asset() {
        let mut g = make_test_genesis();
        g.initial_assets.push(GenesisAsset {
            asset_id: 1, // duplicate
            symbol: "DUP".to_string(),
            name: "Duplicate".to_string(),
            decimals: 18,
            initial_supply: 1000,
            distribution: HashMap::new(),
        });
        let executor = GenesisExecutor::new(g);
        assert!(executor.execute().is_err());
    }
}
