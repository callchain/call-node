# Callchain Implementation Task List

Generated from `spec.md` (4974 lines, 26 sections). Each task specifies crate, files, types/functions to implement, and required unit tests. Tasks ordered by dependency (P0 → P6). E2E tests at the end cover cross-component integration.

---

## P0: Foundation (No dependencies, must be done first)

### [x] T0.1 — Primitives Crate (`crates/primitives`)

**Files**: `crates/primitives/src/lib.rs`, `crates/primitives/Cargo.toml`

**Implement** (per spec §2.4, §3.1, §9.5, §12):
- Type aliases: `TxHash = B256`, `BlockHash = B256`, `AssetId = u64`, `Balance = u128`, `ValidatorId = u32`, `Nonce = u64`
- `Address` wrapper (if not using `alloy_primitives::Address` directly) — 20 bytes, hex encoding, EIP-55 checksum
- `Hash` type alias (H256)
- `Signature` type (secp256k1)
- `PublicKey` type
- `Ed25519PublicKey` type
- `FeeCurrency` enum: `Call`, `Stablecoin(AssetId)`
- `ExecutionStatus` enum: `Success`, `Reverted { reason: String }`
- `InstructionType` enum (discriminant for each Instruction variant)
- `ProtocolVersion`: `major/minor/patch`

**Tests**:
- `test_address_zero()` — zero address construction
- `test_address_checksum()` — EIP-55 checksum encoding/decoding
- `test_address_serialization_rlp()` — RLP round-trip
- `test_address_serialization_json()` — JSON hex round-trip
- `test_fee_currency_equality()`
- `test_type_alias_sizes()` — verify expected sizes

---

### [x] T0.2 — Crypto Crate (`crates/crypto`)

**Files**: `crates/crypto/src/lib.rs`, `crates/crypto/src/secp256k1.rs`, `crates/crypto/src/ed25519.rs`, `crates/crypto/src/hash.rs`, `crates/crypto/Cargo.toml`

**Implement** (per spec §13.1):
- `secp256k1.rs`: key generation, signing, verification, address recovery (`recover_secp256k1_signer`)
- `ed25519.rs`: key generation, signing, verification (for consensus)
- `hash.rs`: `keccak256`, `sha256`, Merkle tree construction (`build_merkle_root`)
- `hash_fn(tx) -> Hash` for all transaction types

**Tests**:
- `test_secp256k1_sign_verify()`
- `test_secp256k1_recover_signer()`
- `test_ed25519_sign_verify()`
- `test_keccak256_known_vectors()` — against known test vectors
- `test_sha256_known_vectors()`
- `test_merkle_root_single_leaf()`
- `test_merkle_root_multiple_leaves()`
- `test_merkle_proof_verification()`

---

### [x] T0.3 — Serialization Crate (`crates/serialization`)

**Files**: `crates/serialization/src/lib.rs`, `crates/serialization/src/rlp.rs`, `crates/serialization/src/json.rs`, `crates/serialization/Cargo.toml`

**Implement** (per spec §9):
- RLP derive macros / trait implementations for all protocol types
- `StorageCodec` trait: `encode_to_buf`, `decode_from_buf`
- Serde JSON serializers for RPC responses
- Address: RLP as 20 raw bytes, JSON as hex string
- `ProtocolTransaction` RLP encode/decode
- `Block` RLP encode/decode
- `ProtocolReceipt` RLP encode/decode

**Tests**:
- `test_address_rlp_roundtrip()`
- `test_address_json_hex_roundtrip()`
- `test_transaction_rlp_roundtrip()`
- `test_block_rlp_roundtrip()`
- `test_receipt_rlp_roundtrip()`
- `test_storage_codec_roundtrip()`

---

### [x] T0.4 — Storage Crate (`crates/storage`)

**Files**: `crates/storage/src/lib.rs`, `crates/storage/src/tables.rs`, `crates/storage/src/db.rs`, `crates/storage/src/prune.rs`, `crates/storage/Cargo.toml`

**Implement** (per spec §10):
- `db.rs`: reth-db (MDBX) initialization, open/create, table definitions
- `tables.rs`: define all tables per §10.2 directory structure:
  - `ProtocolAssets`, `ProtocolBalances`, `ProtocolAllowances`
  - `ShieldedMerkleTree`, `ShieldedNullifiers`, `ShieldedCommitments`, `ShieldedViewingKeys`
  - `AgentRegistrations`, `AgentBalances`, `AgentNonces`
  - `EvmAccounts`, `EvmContracts`, `EvmStorage`
  - `BridgePendingOps`
  - `ConsensusBlocks`, `ConsensusState`
  - `MetadataChainId`, `MetadataValidators`, `MetadataCompliance`, `MetadataAgents`
  - `Receipts`, `Logs`, `Memos`
  - `FeeCurrencyRegistry`, `OraclePrices`, `OracleValidatorInfo`
  - `GovernanceProposals`, `VoteDelegations`
  - `SponsorAuths`, `SponsorPools`, `SponsorDailyUsage`
  - `SessionKeys`, `MultiSigConfigs`, `SocialRecoveryConfigs`
- `prune.rs`: `PruneConfig` with defaults per §10.3.2: `snapshot_interval` (100K blocks), `snapshot_keep` (3), `prune_interval` (10K), `keep_recent` (50K), `keep_block_body` (100K), `keep_receipt` (1M blocks)
- `NodeMode` enum: `Validator`, `Full`, `Light`, `Archive`
- State snapshot: `StateSnapshot` struct with roots + signatures
- Fast sync flow per §10.3.5: (1) download recent snapshot from network, (2) verify 2/3 validator signatures + Merkle root consistency, (3) restore snapshot state to local DB, (4) incremental sync from snapshot height, (5) participate in consensus. Target: <5 minutes total sync time

**Tests**:
- `test_db_open_create()`
- `test_balance_read_write()`
- `test_nullifier_insert_check()`
- `test_prune_config_defaults()`
- `test_snapshot_generation()`
- `test_snapshot_verification()`

---

## P1: Core Protocol Layer

### [x] T1.1 — Protocol Crate: Asset Registry (`crates/protocol/src/registry.rs`)

**Implement** (per spec §3.1, §3.2):
- `Asset` struct (all fields per spec)
- `AssetStatus` enum: `Active`, `Frozen`, `Delisted`
- `register_asset()` function with fee deduction, AssetId allocation, protocol balance creation
- Asset lookup: `get_asset(id)`, `get_asset_by_symbol(symbol)`

**Tests**:
- `test_register_asset_success()`
- `test_register_asset_duplicate_symbol()`
- `test_register_asset_insufficient_fee()`
- `test_asset_freeze_and_delist()`
- `test_get_asset_not_found()`

---

### [x] T1.2 — Protocol Crate: Balance Management (`crates/protocol/src/balances.rs`)

**Implement** (per spec §3.3):
- `ProtocolBalances` type: `HashMap<AssetId, HashMap<Address, u128>>`
- `Allowances` type: `HashMap<(AssetId, Address, Address), u128>`
- Functions: `transfer_balance()`, `mint_balance()`, `burn_balance()`, `deduct_balance()`, `credit_balance()`
- Allowance functions: `set_allowance()`, `spend_allowance()`, `get_allowance()`
- Balance queries: `get_balance(asset_id, address)`

**Tests**:
- `test_transfer_sufficient_balance()`
- `test_transfer_insufficient_balance()`
- `test_mint_by_non_issuer()`
- `test_burn_by_non_issuer()`
- `test_allowance_set_and_spend()`
- `test_allowance_insufficient()`
- `test_balance_overflow_protection()`

---

### [x] T1.3 — Protocol Crate: Compliance Engine (`crates/protocol/src/compliance.rs`)

**Implement** (per spec §3.4):
- `CompliancePolicy` enum: `None`, `OfacBlacklist`, `KycRequired`, `Whitelist`, `Custom`
- `check_compliance()` function matching spec exactly
- Blacklist management: `is_sanctioned()`, `add_to_blacklist()`
- KYC registry: `is_kyc_verified()`, `set_kyc_status()`
- Whitelist registry: `is_whitelisted()`, `add_to_whitelist()`
- Custom handler via precompile callback

**Tests**:
- `test_compliance_none()`
- `test_compliance_ofac_blacklist_blocked()`
- `test_compliance_ofac_blacklist_allowed()`
- `test_compliance_kyc_required()`
- `test_compliance_whitelist_blocked()`
- `test_compliance_custom_handler()`

---

### [x] T1.4 — Protocol Crate: Instruction Execution (`crates/protocol/src/instructions.rs`)

**Implement** (per spec §3.5, §3.6):
- `Instruction` enum with all variants per spec:
  - `Transfer`, `BatchTransfer`, `Approve`, `TransferFrom`, `Mint`, `Burn`
  - `AgentPay`, `AgentBatchPay`, `AgentCall`, `AgentBridgeDeposit`
  - `BridgeDeposit`, `UpdateCompliance`
  - `ShieldedTransfer`, `ShieldedWithdraw`, `ShieldedDeposit`
- `PaymentEntry`, `AgentPayment`, `PaymentMemo` structs — `PaymentMemo { message: String, reference: Option<String>, metadata: Option<Vec<u8>> }` with size limits (256/128/1024 bytes)
- `ComplianceStatus` enum for `UpdateCompliance` instruction, `update_compliance_status()` function
- `execute_protocol_tx()` — full execution flow per spec §3.6:
  1. `verify_auth()` — validate signature/authorization
  2. nonce check — reject if stale or duplicate
  3. `estimate_gas()` — sum gas units for all instructions
  4. `compute_actual_fee()` — base_fee + priority_fee
  5. `take_state_snapshot()` — snapshot current state for rollback
  6. `deduct_gas()` — deduct fee from sender/sponsor
  7. loop through instructions calling `execute_instruction()`
  8. `commit_state_changes()` on success or `restore_state_snapshot()` on failure
  9. `increment_nonce()` — update sender nonce

**Tests**:
- `test_execute_transfer()`
- `test_execute_batch_transfer()`
- `test_execute_approve_and_transfer_from()`
- `test_execute_mint_issuer_only()`
- `test_execute_burn_issuer_only()`
- `test_memo_size_limits()`
- `test_instruction_rollback_on_failure()`
- `test_atomic_multi_instruction()`

---

### [x] T1.5 — Protocol Crate: Transaction Model & Gas (`crates/protocol/src/transaction.rs`)

**Implement** (per spec §3.5, §12.2):
- `ProtocolTransaction` struct: `sender: Address`, `nonce: u64`, `instructions: Vec<Instruction>`, `gas_config: GasConfig`, `fee_currency: FeeCurrency`, `gas_limit: u64`, `max_fee: u128`, `auth: AuthScheme`
- `GasConfig` enum: `SelfPay`, `AuthorizedSponsor`, `PoolSponsor`, `PerTxSponsor`
- `GasConfig::hash()` methods for each variant
- `execute_protocol_tx()` — main entry point per spec §3.6
- `deduct_gas(sender, config, fee_currency, fee, tx_hash)` — 5 params, dispatch by `fee_currency` to CALL or stablecoin deduction per spec §3.6 signature
- `deduct_call_from_payer()` — handles all 4 GasConfig modes
- `deduct_stablecoin_from_payer()` — handles all 4 GasConfig modes with registry check + oracle conversion, plus helper functions `deduct_asset_balance()` and `deduct_asset_from_sponsor_pool()` for stablecoin deduction per §3.6
- Gas calculation: `calculate_gas_units()` with base gas unit table per §12.2.1:
  - Transfer=10,000, Approve/Mint/Burn=5,000, BatchTransfer per payment=1,000
  - BridgeDeposit=10,000, ShieldedDeposit/Withdraw=20,000, ShieldedTransfer=50,000
  - ExternalBridgeDeposit=30,000, Agent instructions=0.5x base
  - Memo: +1 gas per byte
- Marginal discount tiers: 1st instruction 1.0x, 2nd-10th 0.5x, 11th+ 0.25x
- `FeeParams` struct with all 7 fields and default genesis values:
  - `base_fee`, `target_gas_per_block` (10M), `max_gas_per_block` (20M), `adjustment_coefficient` (1/8), `min_base_fee` (1 wei), `max_base_fee` (1B wei), `initial_base_fee` (10 wei)
- Gas unit table with memo variants: `Transfer (with Memo) = 10,000 + memo_bytes * 1`, `BatchTransfer Memo = memo_bytes * 1 gas`
- `update_base_fee()` per spec §12.2.3 with min/max clamping
- Fee allocation per §12.2.5: CALL payment (50% base_fee burn + 50% validator + 100% priority_fee to proposer), stablecoin payment (50% base_fee to treasury + 50% validator + 100% priority_fee to proposer)
- `convert_fee_to_stablecoin()` per spec §12.3.0 with ceil rounding
- MemPool acceptance: `accept_to_mempool()` per spec §12.2.7

**Tests**:
- `test_gas_calculation_single_transfer()`
- `test_gas_calculation_multi_instruction_discount()`
- `test_gas_calculation_batch_100_payments()`
- `test_base_fee_increase_on_congestion()`
- `test_base_fee_decrease_on_idle()`
- `test_base_fee_capping_at_max()`
- `test_deduct_gas_self_pay()`
- `test_deduct_gas_authorized_sponsor()`
- `test_deduct_gas_pool_sponsor()`
- `test_deduct_gas_per_tx_sponsor()`
- `test_stablecoin_fee_conversion()`
- `test_stablecoin_not_in_registry_rejected()`
- `test_mempool_accept_low_fee_rejected()`
- `test_mempool_accept_sufficient_balance()`

---

### [x] T1.6 — Protocol Crate: Smart Accounts (`crates/protocol/src/smart_accounts.rs`)

**Implement** (per spec §3.9):
- `AuthScheme` enum with variant fields: `SingleSig { signature: Signature }`, `MultiSig { signatures: Vec<Signature> }`, `SessionKey { key: Address, signature: Signature }`
- `verify_auth()` — main dispatcher
- `verify_single_sig()` with social recovery check: check pending recovery, if threshold met and delay elapsed, reject old key
- `MultiSigConfig { version: u64, signers: Vec<Address>, threshold: u32 }`, constraints: 2-10 signers, threshold 1..=n
- `register_multi_sig()`, `update_multi_sig()`, `verify_multisig()`
- `SocialRecoveryConfig { recovery_delay_secs: u64, pending_recovery: Option<RecoveryRequest> }` — delay range 24-72 hours
- `RecoveryRequest { new_key: PublicKey, approved_by: Vec<Address>, initiated_at: u64, initiator_signature: Signature }`
- `initiate_recovery()`, `guardian_approve()`, `finalize_recovery()`, `cancel_recovery()`
- `SessionPermissions { allowed_instructions: Vec<InstructionType>, max_per_tx: u128, max_daily: u128, allowed_targets: Vec<Address>, allowed_assets: Vec<AssetId> }`
- `SessionKeyConfig`, `SessionKeyDailyUsage` tracking
- `create_session_key()`, `revoke_session_key()`, `verify_session_key()`

**Tests**:
- `test_single_sig_valid()`
- `test_single_sig_invalid()`
- `test_multisig_2_of_3_valid()`
- `test_multisig_threshold_not_met()`
- `test_multisig_duplicate_signer()`
- `test_multisig_update_requires_old_threshold()`
- `test_recovery_initiation()`
- `test_recovery_guardian_approval()`
- `test_recovery_finalization_after_delay()`
- `test_recovery_cancellation_by_owner()`
- `test_session_key_create_and_revoke()`
- `test_session_key_expired()`
- `test_session_key_daily_limit_exceeded()`
- `test_session_key_instruction_not_allowed()`

---

### [x] T1.7 — Protocol Crate: Fee Currency Registry (`crates/protocol/src/fee_currency.rs`)

**Implement** (per spec §12.3.0):
- `FeeCurrencyEntry` struct: `asset_id`, `name`, `decimals`, `oracle_price_key`, `added_at_block`, `added_by_proposal`
- `FeeCurrencyRegistry` struct: `allowed_currencies: Vec<FeeCurrencyEntry>`, `stablecoin_cap_bps` (default 5000 = 50%)
- `FeeCurrencyRegistry::is_allowed()`, `get_call_price()`
- `add_fee_currency()` — governance-only via `ProposalType::FeeCurrencyAdd { asset_id, name, oracle_price_key }`
- `remove_fee_currency()` — governance-only via `ProposalType::FeeCurrencyRemove { asset_id, grace_period_blocks }`
- `FeeCurrencyCap` proposal type for changing `stablecoin_cap_bps`
- `stablecoin_cap_bps` enforcement: per-block stablecoin gas payment cap
- `convert_fee_to_stablecoin()` with oracle price lookup and ceil rounding
- `priority_score()` for multi-currency mempool sorting (CALL direct, stablecoin converted via oracle price)
- Governance parameters: `min_market_cap_usd` (100M), `oracle_strikes_before_disable` (10), `grace_period_blocks` (86,400 / ~1 day default on `FeeCurrencyRemove`)

**Tests**:
- `test_fee_currency_registry_is_allowed()`
- `test_fee_currency_add_remove()`
- `test_priority_score_call()`
- `test_priority_score_stablecoin_conversion()`

---

### [x] T1.8 — Protocol Crate: Gas Sponsor System (`crates/protocol/src/sponsor.rs`)

**Implement** (per spec §12.3.1-12.3.4):
- `GasSponsorAuth` struct: `sponsor`, `allowed_senders`, `max_daily`, `expires_at`, `sponsor_signature`
- `register_sponsor_auth()`, `revoke_sponsor_auth()`
- `verify_and_deduct_authorized_sponsor()` — expiry + whitelist + daily limit check
- `GasSponsorPool` struct: `sponsor`, `balance`, `delegated_to: Vec<Address>`, `per_tx_limit: u128`
- `deposit_to_pool()`, `withdraw_from_pool()`
- `verify_and_deduct_pool_sponsor()` — check delegated_to whitelist + per_tx_limit before deducting from pool balance
- `verify_and_deduct_per_tx_sponsor()` — signature verification + deduction

**Tests**:
- `test_sponsor_auth_register_and_revoke()`
- `test_sponsor_auth_expired_expiry()`
- `test_sponsor_auth_sender_not_whitelisted()`
- `test_sponsor_auth_daily_limit_exceeded()`
- `test_sponsor_pool_deposit_and_withdraw()`
- `test_sponsor_pool_insufficient_balance()`
- `test_per_tx_sponsor_signature_verification()`

---

### [x] T1.9 — Protocol Crate: Receipts (`crates/protocol/src/receipts.rs`)

**Implement** (per spec §18.4):
- `ProtocolReceipt` struct: `tx_hash: TxHash`, `status: ExecutionStatus`, `gas_used: u64`, `gas_payer: Address`, `fee_currency: FeeCurrency`, `fee_amount: u128`, `instruction_results: Vec<InstructionResult>`, `logs: Vec<LogEntry>`, `memos: Vec<MemoEntry>`, `state_changes: Vec<StateChange>`
- `LogEntry` (receipt log): `address: Address`, `topics: Vec<Hash>`, `data: Vec<u8>`
- `ChangeType` enum: `Balance`, `Allowance`, `Nonce`
- `EvmReceipt` struct: `logs_bloom: Bloom`, `contract_address: Option<Address>`, plus standard receipt fields
- `EvmLogEntry` struct
- `ShieldedReceipt` with `nullifiers`, `commitments`, `encrypted_event`
- `ExternalBridgeReceipt` with `source_tx_hash`, `source_block`, `confirmations`
- `compute_receipt_root()` — keccak256(rlp_encode(each receipt)) → Merkle root
- Receipt query functions
- Receipt prune strategy per §18.4.7: keep_receipt (default 1M blocks for validator/full), receipt_root always in block header

**Tests**:
- `test_receipt_creation_success()`
- `test_receipt_creation_reverted()`
- `test_receipt_root_computation()`
- `test_shielded_receipt_hides_amounts()`
- `test_external_bridge_receipt_source_tx()`

---

### [x] T1.10 — Protocol Crate: Issuer Management (`crates/protocol/src/issuer.rs`)

**Implement** (per spec §7):
- `IssuerAction` enum: `Mint { to, amount }`, `Burn { from, amount }`, `FreezeAddress { target }`, `UnfreezeAddress { target }`, `UpdatePolicy { new_policy }`, `TransferOwnership { new_issuer }`
- `freeze_address(asset_id, target)` — issuer can freeze specific address, blocking all transfers
- `unfreeze_address(asset_id, target)` — unfreeze previously frozen address
- `transfer_ownership(asset_id, new_issuer)` — transfer issuer permissions
- Issuer permission checks: only `asset.issuer` can call mint/burn/freeze/unfreeze/update-policy/transfer-ownership
- Issuer limitations (§7.2): cannot modify other issuers' assets, cannot bypass compliance, cannot change fee model, cannot modify bridge rules, cannot directly modify user balances (only via mint/burn)

**Tests**:
- `test_issuer_freeze_address()`
- `test_issuer_unfreeze_address()`
- `test_frozen_address_cannot_transfer()`
- `test_issuer_transfer_ownership()`
- `test_non_issuer_cannot_freeze()`
- `test_issuer_cannot_modify_other_assets()`
- `test_issuer_cannot_bypass_compliance()`

---

### [x] T1.11 — Protocol Crate: Economics Module (`crates/protocol/src/economics.rs`)

**Implement** (per spec §12.1, §12.4, §12.5):
- Total supply: 1B CALL fixed, no inflation
- `minimum_unit: u128 = 10^-18 CALL` (1 wei)
- Distribution tracking:
  - Validator rewards: 350M CALL, linear release over 8 years
  - Ecosystem fund: 100M CALL, multi-sig managed
  - Community airdrop: 50M CALL, 30% at mainnet + remaining 24mo linear
  - Historical allocation: 500M CALL (pre-genesis, already distributed)
- Fee distribution logic:
  - CALL payment: `base_fee × 50% → burn`, `base_fee × 50% → validator reward (by stake proportion)`, `priority_fee 100% → block proposer`
  - Stablecoin payment: `base_fee × 50% → treasury reserve (not burned)`, `base_fee × 50% → validator reward`, `priority_fee 100% → block proposer`
- Multi-currency fee aggregation per block: track CALL, USDC, USDT separately, distribute each by validator stake weight
- Linear release schedule calculation: blocks elapsed → released amount
- Airdrop release: 30% instant at TGE, remaining 70% over 24 months linear

**Tests**:
- `test_total_supply_fixed()`
- `test_fee_distribution_call_burn()`
- `test_fee_distribution_stablecoin_treasury()`
- `test_validator_reward_linear_release()`
- `test_validator_reward_multi_currency_by_stake()`
- `test_airdrop_release_mainnet_30pct()`
- `test_airdrop_release_linear_24mo()`
- `test_no_inflation()`

---

## P2: EVM Layer

### [x] T2.1 — EVM Crate (`crates/evm`)

**Files**: `crates/evm/src/lib.rs`, `crates/evm/src/executor.rs`, `crates/evm/src/state.rs`, `crates/evm/Cargo.toml`

**Implement** (per spec §4):
- EVM executor wrapping Revm
- EVM state management (accounts, contracts, storage)
- ERC-20 template contract deployment on asset registration
- `evm_call()` helper for bridge operations
- Gas tracking and limit enforcement
- EVM transaction validation (nonce, balance, gas_limit)

**Tests**:
- `test_evm_execute_transfer()`
- `test_evm_deploy_erc20_template()`
- `test_evm_call_bridge_mint()`
- `test_evm_gas_tracking()`
- `test_evm_nonce_validation()`
- `test_evm_insufficient_balance()`

---

### [x] T2.2 — ERC-20 Template Contract (`crates/evm/src/contracts/erc20_template.sol`)

**Implement** (per spec §4.2):
- Solidity `AssetToken` contract with:
  - Standard ERC-20 interface
  - `bridgeMint()`, `bridgeBurn()` with `onlyBridge` modifier
  - Independent EVM layer balances

**Tests**:
- `test_erc20_transfer()`
- `test_erc20_approve_transfer_from()`
- `test_bridge_mint_only_bridge()`
- `test_bridge_burn_only_bridge()`

---

### [x] T2.3 — Precompiles (`crates/precompiles`)

**Files**: `crates/precompiles/src/lib.rs`, `crates/precompiles/src/oracle.rs`, `crates/precompiles/src/bridge.rs`, `crates/precompiles/Cargo.toml`

**Implement** (per spec §25.4):
- Oracle precompile at `0x101`: `getPrice()`, `getTWAP()`, `isStale()`, `getOracleStatus()`
- Bridge precompile for protocol balance queries from EVM
- Protocol balance precompile

**Tests**:
- `test_oracle_precompile_get_price()`
- `test_oracle_precompile_get_twapped()`
- `test_oracle_precompile_is_stale()`
- `test_bridge_precompile_protocol_balance()`

---

## P3: Bridge Layer

### [x] T3.1 — Internal Bridge (`crates/bridge`)

**Files**: `crates/bridge/src/lib.rs`, `crates/bridge/src/deposit.rs`, `crates/bridge/src/withdraw.rs`, `crates/bridge/Cargo.toml`

**Implement** (per spec §5.1-5.5):
- `BridgeOp` enum with fields: `DepositToEvm { asset_id: AssetId, from: Address, to: Address, amount: u128 }`, `WithdrawToProtocol { asset_id: AssetId, from: Address, to: Address, amount: u128 }`
- `execute_deposit()` — deduct protocol balance, EVM mint
- `execute_withdraw()` — EVM burn, restore protocol balance
- Pending bridge queue management
- Same-block bridge completion guarantee

**Tests**:
- `test_deposit_insufficient_protocol_balance()`
- `test_check_deposit_balance()`
- `test_withdraw_insufficient_evm_balance()`
- `test_withdraw_full_flow()`
- `test_bridge_state_pending_ops()` — pending queue management
- `test_bridge_state_deposit_tracking()`
- `test_bridge_daily_limit()`
- `test_bridge_pause_unpause()`

---

### [x] T3.2 — External Bridge (`crates/bridge/src/external.rs`)

**Implement** (per spec §5.6):
- `ExternalChain` enum: `EthereumMainnet`, `Arbitrum`
- `ExternalBridgeOp` enum with full fields:
  - `Deposit { source_chain, source_tx_hash, source_block_number, sender, recipient, asset_id, amount, signatures }`
  - `Withdraw { target_chain, target_address, asset_id, sender, amount }`
- `verify_bridge_signatures()` — 14+ unique validator secp256k1 signatures
- `sign_bridge_event()` — validator signing over bridge event hash
- `BridgeConfig { max_per_tx, daily_limit_per_asset, eth_min_confirmations (12), bridge_fee, allowed_assets, signature_timeout_secs (300), min_validator_signatures (14) }`
- `process_external_deposit()` — sig verify → asset check → limits → replay protection → credit
- `process_external_withdraw()` — asset check → limits → deduct protocol balance

**Tests**:
- `test_external_chain_ids()`
- `test_verify_insufficient_signatures()` — 5 sigs, need 14
- `test_verify_duplicate_validator()` — real sigs, duplicate validator rejected
- `test_verify_valid_signatures_14()` — 14 unique valid signatures accepted
- `test_verify_signer_not_in_validator_set()` — rogue signer rejected
- `test_signature_complete()`
- `test_external_deposit_asset_not_allowed()`
- `test_external_withdraw_insufficient_balance()`

---

## P4: Agent Payments

### [x] T4.1 — Agent Payments Foundation/Module Setup

**Files**: `crates/agent/src/lib.rs`, `crates/agent/Cargo.toml`

**Implement** (per spec §6):
- Crate structure with all sub-modules: registry, permissions, balances, executor
- Core types: `AgentRegistration`, `DomainProof`, `AgentPermissions`, `AgentFeeConfig`, `FeePayer`, `AgentFundingAction`, `AgentBalances`, `AgentNonces`, `SignedAgentTx`

**Tests**: 5 lib tests passing

---

### [x] T4.2 — Agent Registry with Domain Verification

**Files**: `crates/agent/src/registry.rs`

**Implement** (per spec §6.1-6.2):
- `AgentRegistration` struct: `agent_id`, `owner`, `agent_public_key`, `name`, `url`, `metadata_hash`, `domain_proof`, `registered_at`
- `DomainProof` enum: `DnsTxt { domain, txt_value }`, `HttpFile { url, expected_content }`
- Agent registration with domain verification

**Tests**: 8 registry tests passing

---

### [x] T4.3 — Agent Permission System

**Files**: `crates/agent/src/permissions.rs`

**Implement** (per spec §6.3-6.4):
- `AgentPermissions` struct: `allowed_assets`, `daily_limit`, `per_tx_limit`, `allowed_counterparties`, `allowed_protocols`, `expires_at`
- `AgentFeeConfig` struct: `fee_payer`, `owner_max_daily_fee`, `owner_max_total_fee`, `require_owner_signature_above`
- `FeePayer` enum: `SelfPay`, `OwnerPays`, `ThirdParty { payer }`
- Permission enforcement and validation

**Tests**: 11 permissions tests passing

---

### [x] T4.4 — Agent Balance Management

**Files**: `crates/agent/src/balances.rs`

**Implement** (per spec §6.5):
- `AgentBalances`: `HashMap<(Address, u64, AssetId), u128>`
- `AgentNonces`: `HashMap<(Address, u64), u64>`
- Fund grant, top-up, revoke (immediate, no Agent consent needed), update config

**Tests**: 7 balances tests passing

---

### [x] T4.5 — Agent Transaction Execution

**Files**: `crates/agent/src/executor.rs`

**Implement** (per spec §6.6-6.8):
- `SignedAgentTx` struct: `protocol_tx: ProtocolTransaction`, `owner_signature: Option<Signature>`
- `verify_agent_tx()` — full 5-step validation per spec §6.6
- `execute_agent_tx()` — multi-instruction execution with gas discount (0.5x per §6.8)
- Helper functions: `execute_agent_pay()`, `execute_agent_batch_pay()`, `execute_agent_call()`, `execute_agent_bridge_deposit()`

**Tests**: 9 executor tests passing

---

**P4 Test Summary**: 40/40 tests passing
- balances: 7 tests
- registry: 8 tests
- permissions: 11 tests
- executor: 9 tests
- lib: 5 tests

---

## P5: Shielded Pool

### [x] T5.1 — Shielded Crate (`crates/shielded`)

**Files**: `crates/shielded/src/lib.rs`, `crates/shielded/src/merkle.rs`, `crates/shielded/src/notes.rs`, `crates/shielded/src/nullifiers.rs`, `crates/shielded/src/circuit.rs`, `crates/shielded/src/prover.rs`, `crates/shielded/src/compliance.rs`, `crates/shielded/Cargo.toml`

**Implement** (per spec §3.8):
- `Note`: `value`, `asset_id`, `rcm`, `recipient_view_key`
- `NoteCommitment(pub Hash)`, `Nullifier(pub Hash)`, `ViewingKey`: `incoming_view_key`, `full_view_key`
- `ZkProof`: `proof_data: Vec<u8>`, `public_inputs` (nullifiers + commitments)
- `ShieldedState`: merkle_tree, nullifier_set, note_registry
- Incremental Merkle Tree (depth 32, ~4.2B leaves), O(log n) update
- Nullifier management: insert, check spent, BitSet compression (~1 bit/nullifier, 100M tx ~12.5MB)
- Note encryption and storage
- `ShieldedComplianceMode` enum: `Unrestricted`, `KycRequired`, `IssuerAuditable`, `WhitelistedOnly`
- ZK circuit definition per §3.8.3:
  - Public inputs: `nullifiers[]`, `commitments[]`, `asset_id`
  - Private inputs: `notes[]`, `new_notes[]`, `spending_key`, `merkle_path[]`
  - 5 constraints: (1) nullifier correctly derived, (2) note in Merkle Tree (path valid), (3) sender owns spending rights, (4) sum(new_notes.value) <= sum(notes.value), (5) values in valid range (no overflow/underflow)
- Groth16 parameters (~200B proof, ~3ms verify); Halo2 migration path
- `verify_zk_proof()`, `verify_shielded_balance()`
- Per-block shielded transaction limit (50) enforced in payload builder (§13.5.1); excess shielded txs queued to next block
- Viewing key management for compliance audit: generate → decrypt → verify

**Tests**:
- `test_merkle_tree_insert()`
- `test_merkle_tree_depth_limit()`
- `test_nullifier_insert_and_check()`
- `test_nullifier_double_spend_detection()`
- `test_nullifier_bitset_compression()`
- `test_note_encryption_decryption()`
- `test_viewing_key_generation()`
- `test_viewing_key_balance_disclosure()`
- `test_shielded_compliance_kyc_required()`
- `test_shielded_compliance_issuer_auditable()`
- `test_zk_proof_verification_mock()`
- `test_per_block_shielded_limit()`

**Tests (53 total)**:
- merkle: 7 tests (insert, depth limit, proof verification for 1/2/3/16 leaves, deterministic, empty)
- notes: 6 tests (creation, commitment, nullifier, encryption/decryption, wrong key)
- nullifiers: 5 tests (insert/check, double-spend, bitset compression, no false negatives, clear)
- circuit: 7 tests (nullifier, merkle path, spending rights, value conservation, range, asset mismatch, all constraints)
- prover: 8 tests (mock generate/verify, Groth16 generate/verify/key-sizes, constraint violation, bad proof rejection)
- compliance: 10 tests (unrestricted, KYC required/empty, issuer auditable/invalid-issuer/zero-key, whitelist/empty, audit record, equality)
- lib: 10 tests (viewing key gen/balance, zk proof mock/empty/dup-nullifiers, shielded balance, transfer conservation, state process, per-block limit, note commitment/nullifier)

---

## P6: Consensus Integration

### [x] T6.1 — Consensus Crate (`crates/consensus`)

**Files**: `crates/consensus/src/lib.rs`, `crates/consensus/src/simplex.rs`, `crates/consensus/src/proposer.rs`, `crates/consensus/src/validator.rs`, `crates/consensus/Cargo.toml`

**Implement** (per spec §2.1-2.5):
- Simplex BFT integration with commonware-consensus
- `Block` struct: header, protocol_txs, evm_txs, system_txs, bridge_operations
- `BlockHeader` struct with all root fields
- `ConsensusParams` struct: `max_validators: u32` (216), `subset_size: u32` (21), `block_time_millis: u64` (250), `slashing_window: u64`
- Validator count range: 100-216, with dynamic minimum stake mechanism to keep count stable
- Block execution order: EVM → Protocol → Bridge → System
- Proposer selection and subset rotation (21 per round from 216)
- Validator management: `ValidatorStake`, staking, slashing
- `update_base_fee()` called at end of each block
- System transaction execution: validator reward distribution, fee settlement
- Double-slash detection, offline penalty

**Tests**:
- `test_block_structure_serialization()`
- `test_block_execution_order()`
- `test_proposer_subset_rotation()`
- `test_validator_stake_and_reward()`
- `test_double_sign_slash()`
- `test_offline_penalty()`
- `test_base_fee_update_after_block()`
- `test_system_tx_reward_distribution()`

---

### [x] T6.2 — Validator Staking (`crates/consensus/src/staking.rs`)

**Implement** (per spec §12.6):
- `ValidatorStake` struct: `validator_id: u32`, `staked_call: u128`, `self_stake: u128`, `delegated_call: u128`, `rewards: u128`, `slash_history: Vec<SlashEvent>`
- `SlashEvent` struct: `reason: String`, `amount_slashed: u128`, `block: u64`
- Minimum self-stake: 1,000,000 CALL
- Unbonding period: 7 days
- Slash logic: double-sign (full self-stake), offline (proportional)
- Reward distribution: 50% fee split by stake proportion

**Tests**:
- `test_validator_stake_minimum()`
- `test_validator_unbonding_period()`
- `test_slash_double_sign_full_loss()`
- `test_slash_offline_proportional()`
- `test_reward_distribution_by_stake()`

---

## P7: Network Layer

### [x] T7.1 — Network Crate (`crates/network`)

**Files**: `crates/network/src/lib.rs`, `crates/network/src/p2p.rs`, `crates/network/src/gossip.rs`, `crates/network/src/limits.rs`, `crates/network/Cargo.toml`

**Implement** (per spec §8, §13.5.4):
- commonware-p2p integration
- Gossipsub for transaction propagation (ProtocolTransaction high priority, EvmTx standard)
- Request-response pattern for state sync
- `NetworkLimits` with default values per §13.5.4: `max_peers` (50), `max_messages_per_second` (100), `max_message_size` (10 MB), `known_txs_cache_size` (1,000,000), `ban_duration_seconds` (3,600)
- Transaction deduplication via LRU cache
- Rate limiting and peer management

**Tests**:
- `test_transaction_propagation_priority()`
- `test_known_txs_dedup()`
- `test_rate_limit_enforcement()`
- `test_max_message_size_rejection()`
- `test_peer_connection_limit()`

---

## P8: Transaction Pool (Mempool)

### [x] T8.1 — Transaction Pool Crate (`crates/transaction-pool`)

**Files**: `crates/transaction-pool/src/lib.rs`, `crates/transaction-pool/src/pool.rs`, `crates/transaction-pool/src/priority.rs`, `crates/transaction-pool/Cargo.toml`

**Implement** (per spec §17):
- `Mempool` struct with protocol_pool, agent_pool, evm_pool, pending_bridges, known_txs
- `known_txs: LruCache<TxHash, ()>` deduplication cache
- Priority ordering per §17.2: Protocol > Agent > EVM > Bridge
- `priority_score()` multi-currency calculation
- Capacity limits: 50K protocol, 100K EVM, 256 per address
- Eviction strategy: low fee → stale nonce → tail eviction
- 72-block lifetime for pending transactions
- Anti-spam: minimum fee, dedup, signature validation

**Tests**:
- `test_mempool_insert_protocol_tx()`
- `test_mempool_priority_ordering()`
- `test_mempool_multi_currency_priority()`
- `test_mempool_capacity_limit()`
- `test_mempool_per_address_limit()`
- `test_mempool_eviction_low_fee()`
- `test_mempool_eviction_stale_nonce()`
- `test_mempool_duplicate_tx_rejected()`
- `test_mempool_lifetime_expiry()`

---

## P9: Payload Builder

### [x] T9.1 — Payload Builder (`crates/payload-builder`)

**Files**: `crates/payload-builder/src/lib.rs`, `crates/payload-builder/src/builder.rs`, `crates/payload-builder/Cargo.toml`

**Implement**:
- Block assembly from mempool: select txs by priority
- Enforce block limits (§13.5.1): max_block_size, max_transactions, max_shielded_per_block, max_instructions_per_tx, max_tx_size, max_batch_payments, max_evm_gas_per_block
- Execute txs in spec order: EVM → Protocol → Bridge → System
- Compute state roots after execution
- Reject block if roots don't match

**Tests**:
- `test_payload_builds_block_from_mempool()`
- `test_payload_enforces_block_limits()`
- `test_payload_shielded_per_block_limit()`
- `test_payload_execution_order()`
- `test_payload_state_root_mismatch_rejects()`

---

### [x] T9.2 — Payload Types (`crates/payload-types`)

**Files**: `crates/payload-types/src/lib.rs`, `crates/payload-types/Cargo.toml`

**Implement**:
- `BlockLimits` struct with default values per §13.5.1:
  - `max_block_size` (5 MB), `max_transactions` (10,000), `max_shielded_per_block` (50),
  - `max_instructions_per_tx` (1,000), `max_tx_size` (256 KB), `max_batch_payments` (5,000),
  - `max_evm_gas_per_block` (30,000,000)
- `FeeParams` struct
- `PruneConfig` struct (re-export from storage)
- Payload attributes for consensus integration

**Tests**:
- `test_block_limits_defaults()` — verify all default values match §13.5.1 spec
- `test_fee_params_defaults()` — verify default FeeParams values
- `test_prune_config_reexport()` — verify re-export resolves correctly
- `test_payload_attributes_serialization()` — round-trip for consensus integration

---

## P10: Chain Spec

### [x] T10.1 — Chain Spec / Genesis (12 tests passing)

---

## P11: RPC Layer

### [x] T11.1 — RPC Layer (16 tests passing)

All RPC endpoints implemented:
- **Standard Ethereum**: eth_getBalance, eth_call, eth_sendRawTransaction, eth_getTransactionReceipt, eth_blockNumber, eth_getLogs, eth_getProof
- **Callchain extensions**: call_assetInfo, call_protocolBalance, call_sendPayment, call_registerAsset, call_compliancePolicy, call_totalBalance
- **Agent RPC**: call_agentRegister, call_agentInfo, call_agentBalance, call_agentHistory, call_agentGrant, call_agentRevoke
- **Shielded RPC**: call_shieldedDepositProve, call_shieldedTransferProve, call_shieldedBalance, call_shieldedTreeState
- **Receipt queries**: call_getTransactionReceipt, call_getBlockReceipts, call_getLogs, call_getTxByReference
- **WebSocket subscriptions**: 8 subscription endpoints (payment block, payment received, bridge, asset, agent, shielded)

---

## P12: Governance

### [x] T12.1 — Governance Module (16 tests passing)

All governance features implemented:
- **Proposal types**: ParameterChange, ProtocolUpgrade, TreasurySpend, ValidatorSlash, ComplianceUpdate, EmergencyPause, FeeCurrencyAdd/Remove/Cap
- **Dual-track voting**: validators (1=1), CALL holders (balance-weighted), joint issuer+validator for compliance
- **Proposal lifecycle**: submit (10K CALL deposit) → review → vote → timelock → execute → expire
- **Vote delegation**: delegate/undelegate with expiry
- **Emergency pause**: 2/3 validator signatures, immediate effect
- **Quorum calculation**: per-type (2/3 validators, 20% supply, simple majority)

---

## P14: Node Application & CLI

### [x] T14.1 — Node Crate (`crates/node`) — 6 tests passing

**Files**: `crates/node/src/main.rs`, `crates/node/src/cli.rs`, `crates/node/src/config.rs`, `crates/node/src/boot.rs`, `crates/node/Cargo.toml`

**Implement** (per spec §21):
- CLI argument parsing (clap): genesis, validator mode, keys, peers, P2P, RPC, storage, metrics, logging
- CLI defaults: `--p2p-listen` (0.0.0.0:51235), `--rpc-http-addr` (127.0.0.1:8545), `--rpc-ws-addr` (127.0.0.1:8546), `--data-dir` (~/.callchain), `--db-cache-size` (1024 MB), `--metrics-addr` (0.0.0.0:9090), `--log-level` (info), `--log-format` (text)
- TOML config file parsing
- Boot sequence per §21.3: parse config → init logging → open DB → load genesis → init P2P → connect seeds → init consensus → start RPC → start metrics → sync/participate
- DB recovery path: if DB has existing data, resume from last saved state
- Validator mode vs full node mode
- Key management: validator key, consensus key

**Tests**:
- `test_cli_parse_all_args()`
- `test_toml_config_parse()`
- `test_cli_overrides_toml()`
- `test_boot_sequence_fresh_db()`
- `test_boot_sequence_existing_db_recovery()`
- `test_validator_mode_requires_keys()`

---

## P15: Telemetry & Monitoring

### [x] T15.1 — Telemetry Module (`crates/node/src/telemetry.rs`) — 12 tests passing

**Implement** (per spec §20):
- Prometheus metrics: consensus, mempool, bridge, P2P, performance, system
- `/metrics` HTTP endpoint on `:9090` with Prometheus content-type header
- OpenTelemetry tracing integration with `tracing-opentelemetry` layer
- Alert rules: consensus stall, validator offline, mempool overflow, bridge delay, memory, disk
- Span recording helpers: `record_block_span()`, `record_tx_span()`, `record_p2p_span()`
- `/health` endpoint for liveness checks

**Tests**:
- `test_prometheus_metrics_endpoint()`
- `test_consensus_metrics_recorded()`
- `test_mempool_size_metric()`
- `test_alert_consensus_stall()`
- `test_alert_mempool_overflow()`
- `test_telemetry_registry_default()`
- `test_telemetry_uptime_increases()`
- `test_register_custom_metric()`
- `test_metrics_http_server()` — HTTP /metrics returns Prometheus format
- `test_health_endpoint()` — /health returns 200 OK
- `test_metrics_content_type()` — correct Prometheus content-type header
- `test_opentelemetry_span_recording()`

---

## P16: Light Client

### [x] T16.1 — Light Client (`crates/node/src/light_client.rs`) — 14 tests passing

**Implement** (per spec §23):
- `LightClient` struct: `trusted_validators`, `latest_block_header`, `chain_id`, `checkpoint`, `verified_headers`
- `verify_header()` — parent hash link, 2/3+ validator signatures, state root consistency
- `verify_proof()` — generic Merkle proof verification
- `verify_shielded_tx_full()` — full ZK proof verification (~3ms Groth16) + nullifier proof + commitment proof
- `verify_shielded_tx_light()` — Merkle inclusion only, trust validators verified ZK
- `query_shielded_balance()` — via viewing key, decrypt notes with Merkle proofs
- Light client RPC methods per §23.2: `light_verifyBlockHeader`, `call_getBalanceProof`, `eth_getProof`, `call_getTransactionProof`, `call_getBridgeProof`, `call_getShieldedStateProof`, `call_getShieldedBalanceProof`, `call_getShieldedTxProof`
- Proof types as distinct structs:
  - `BalanceProof { merkle_proof: MerkleProof, balance: u128 }`
  - `TransactionProof { tx_hash: TxHash, merkle_proof: MerkleProof, block: u64 }`
  - `BridgeProof { bridge_op_hash: Hash, signatures: Vec<Signature>, status: String }`
  - `ShieldedStateProof { root: Hash, commitment_count: u64, nullifier_count: u64 }`
  - `EncryptedBalanceProof { encrypted_notes: Vec<EncryptedNote>, merkle_proofs: Vec<MerkleProof> }`
- Sync strategy per §23.4: initial sync (from checkpoint), incremental sync (block-by-block header), state sync (on-demand Merkle proofs)
- Resource targets: <10MB storage, ~1KB/block bandwidth, ~50ms compute per block

**Tests**:
- `test_light_client_verify_header()`
- `test_light_client_verify_header_invalid_sig()`
- `test_light_client_merkle_proof()`
- `test_light_client_shielded_light_verification()`
- `test_light_client_shielded_balance_query()`
- `test_light_client_sync_incremental()`
- `test_light_client_checkpoint()`
- `test_light_client_storage_estimate()`
- `test_light_client_parent_hash_mismatch()`
- `test_light_parent_hash_mismatch()`
- `test_merkle_proof_verify()`
- `test_balance_proof_structure()`
- `test_light_client_default()`
- `test_block_signatures_valid_count()`

---

## P17: Logging & Auditing

### [x] T17.1 — Logging Module (`crates/node/src/logging.rs`) — 13 tests passing

**Implement** (per spec §24):
- `LogEntry` struct (logging, distinct from receipt LogEntry): `timestamp` (ISO 8601), `level` (trace/debug/info/warn/error), `target` (module path), `message`, `fields: HashMap<String, Value>`
- Log configuration per §24.4 table: log level options (trace/debug/info/warn/error), format (text/json), output (stdout/file/both), rotation by size (100MB) or daily, retention (30 days), audit log always enabled
- `AuditEntry` struct: `block_height`, `tx_index`, `tx_type`, `action`, `agent_id`, `fee_payer`, `before_state`, `after_state`, `tx_hash`, `shielded_details` — append-only
- `ShieldedAuditInfo` struct: `nullifiers`, `commitments`, `encrypted_amounts: Vec<EncryptedValue>`, `compliance_mode` — encrypted, only viewing key holders can decrypt
- Audit log: independent `audit_log` table, Merkle-ized with root optionally written to block header
- Compliance report API: `call_exportComplianceReport` — export CSV for asset/address range
- Log rotation (100MB or daily), retention (30 days), audit log always enabled
- Configuration per §24.4 table

**Tests**:
- `test_structured_log_json_format()`
- `test_structured_log_text_format()`
- `test_audit_log_append_only()`
- `test_audit_log_merkle_root()`
- `test_compliance_report_export()`
- `test_log_rotation()`
- `test_audit_log_file_roundtrip()`
- `test_should_rotate_by_size()`
- `test_log_entry_new()`
- `test_format_timestamp()`
- `test_encrypted_value_serde()`
- `test_audit_log_merkle_proof_verification()`
- `test_compliance_report_csv()`

---

## P18: Oracle System

### [x] T18.1 — Oracle Module (`crates/protocol/src/oracle.rs`) — 8 tests passing

**Implemented** (per spec §25):
- `OracleSubmission`, `AggregatedPrice`, `OracleValidatorInfo`, `HistoricalPrice`, `OracleConfig` structs
- `submit_oracle_price()` — Ed25519 signature verification, period check (`ORACLE_UPDATE_INTERVAL` = 1000 blocks), dedup, quorum trigger (14 of 21)
- `aggregate_and_publish_price()` — sort + median, outlier marking (>5% deviation), TWAP history append
- Outlier management: 10 cumulative strikes → `is_active = false`
- Full oracle configuration per §25.5: update_interval=1000, quorum=14, outlier_threshold=5%, outlier_tolerance=10, twap_window=24h, staleness=900s, min_data_sources=2
- `OracleManager` with validator registration, TWAP calculation, staleness checks
- `simple_submit_price()` for legacy precompile compatibility
- Integrated with `FeeCurrencyRegistry.get_call_price()` and `priority_score()`
- `OracleState` in precompiles wraps `OracleManager`

**Tests**:
- `test_oracle_submission_valid()`
- `test_oracle_submission_wrong_period()`
- `test_oracle_submission_duplicate()`
- `test_oracle_aggregation_median()`
- `test_oracle_outlier_detection()`
- `test_oracle_outlier_disabled_after_10()`
- `test_oracle_price_staleness()`
- `test_oracle_twap_calculation()`

---

## P19: Fork & Upgrade

### [x] T19.1 — Fork Management (`crates/consensus/src/fork.rs`) — 4 tests passing

**Implemented** (per spec §19):
- `UpgradeEntry`, `EmergencyRollback`, `ForkManager`, `EmergencyRollbackResult` structs
- Height-activated upgrades via `schedule_upgrade()` and `check_upgrades_at_height()`
- Governance proposal trigger with timelock: `schedule_governance_upgrade()` enforces minimum `timelock_blocks` (default 1000, min 100)
- Version check on block processing: `validate_block_version()` rejects mismatched versions
- Emergency rollback via 2/3 validator signatures: `submit_rollback_signature()` with Ed25519 verification, dedup, quorum check (`ceil(2n/3)`)
- `ProtocolVersion` already defined in `call_primitives`, reused here
- `rollback_quorum()` computes proper ceil(2n/3) quorum

**Tests**:
- `test_height_activated_upgrade()`
- `test_version_check_mismatch_rejects()`
- `test_governance_trigger_upgrade()`
- `test_emergency_rollback()`

---

## P20: State Expiration

### [ ] T20.1 — State Expiration Module (`crates/storage/src/expiration.rs`)

**Implement** (per spec §22):
- Protocol layer: no state expiration — protocol balances, bridge state, agent registrations never expire
- EVM layer: EIP-161 empty account cleanup — accounts with nonce=0, balance=0, code_hash=empty auto-cleared after transaction
- EVM storage slot zero-value optimization — zero slots not written to disk
- State expiration vs prune relationship (§22.4):
  - State expiration decides "what data no longer has meaning" (protocol semantics)
  - Prune decides "what data no longer needs to be stored locally" (storage implementation)
  - Current balances never expire but historical intermediate states can be pruned
  - Nullifiers never expire (anti-double-spend required)
  - Agent registrations never expire (unless Revoke)
- Per-data-type expiration table implementation (§22.4):
  - Protocol balances: never expire, historical versions pruned
  - Shielded nullifiers: never expire
  - Agent registrations: never expire (unless Revoke)
  - EVM empty accounts: EIP-161 auto-clear
  - Historical transaction traces: pruned after keep_recent
  - Block bodies: pruned after keep_block_body

**Tests**:
- `test_protocol_balance_never_expires()`
- `test_evm_empty_account_eip161_cleanup()`
- `test_nullifier_never_expires()`
- `test_agent_registration_never_expires()`
- `test_expiration_vs_prune_separation()`
- `test_state_prune_does_not_affect_current_balance()`

---

## P21: Security & Attack Prevention

### [ ] T21.1 — Security Module (`crates/protocol/src/security.rs`)

**Implement** (per spec §13.5):
- `BlockLimits` struct enforcement
- Mempool attack prevention: tx flooding, address saturation, large tx, multi-instruction bloat, batch transfer inflation, signature forgery, replay, shielded proof bloat, agent abuse
- Shielded Pool defense: ZK proof DoS, nullifier set inflation, Merkle depth attack, per-block limit
- P2P defense: Sybil, message flood, large message, eclipse, route hijacking
- Consensus defense: 51%, double-sign, offline, long-range, nothing-at-stake
- MEV protection per §13.2:
  - PBS (Proposer-Builder Separation) — separate block building from block proposing
  - Commit-reveal for mempool transactions — encrypt tx in mempool, reveal at execution time

**Tests**:
- `test_block_limits_max_tx_size()`
- `test_block_limits_max_instructions()`
- `test_mempool_tx_flood_protection()`
- `test_mempool_address_saturation_protection()`
- `test_mempool_replay_protection()`
- `test_shielded_per_block_limit()`
- `test_shielded_nullifier_double_spend()`
- `test_p2p_rate_limiting()`
- `test_consensus_double_sign_slash()`

---

## P22: Integration Tests

### [ ] T22.1 — Integration Test Suite (`tests/integration/`)

**Files**: `tests/integration/mod.rs`, `tests/integration/test_payment_flow.rs`, `tests/integration/test_bridge_flow.rs`, `tests/integration/test_agent_flow.rs`, `tests/integration/test_shielded_flow.rs`, `tests/integration/test_governance_flow.rs`

**Implement** cross-crate integration tests:

**Shared infrastructure**:
- `SharedTxCorpus` (shared with T23.1 E2E tests) — reusable transaction builders and test vectors, avoiding duplicate tx construction logic between integration and E2E layers

**`test_payment_flow.rs`**:
- Full protocol payment: register asset → transfer → check balance → generate receipt
- Multi-instruction atomic transfer: Transfer + Approve + BridgeDeposit
- Batch transfer 100 recipients with PaymentMemo
- Stablecoin gas payment: submit tx with FeeCurrency::Stablecoin → oracle conversion → fee deduction
- GasConfig scenarios: SelfPay, AuthorizedSponsor, PoolSponsor, PerTxSponsor
- AuthScheme scenarios: SingleSig, MultiSig 2-of-3, SessionKey with limits

**`test_bridge_flow.rs`**:
- Internal bridge: Protocol deposit → EVM mint → EVM balance check
- Internal bridge: EVM withdraw → Protocol restore → Protocol balance check
- Same-block bridge completion
- External bridge deposit: simulate ETH bridge contract → validator signatures → Callchain mint
- External bridge withdraw: Callchain burn → validator signatures → ETH bridge release

**`test_agent_flow.rs`**:
- Agent registration → owner grant → agent pays (OwnerPays mode)
- Agent multi-instruction: AgentBridgeDeposit + AgentCall
- Agent permission enforcement: disallowed asset rejected
- Agent revoke: funds return to owner
- Agent daily limit enforcement

**`test_shielded_flow.rs`**:
- Shielded deposit: transparent → shielded → balance check
- Shielded transfer: shielded → shielded → nullifier check → Merkle update
- Shielded withdraw: shielded → transparent → balance check
- Viewing key balance disclosure
- Shielded compliance: KYC required mode blocks unverified receiver

**`test_governance_flow.rs`**:
- Parameter change proposal: submit → vote → pass → timelock → execute
- Treasury spend: CALL holder voting → quorum → execute
- Emergency pause: 2/3 validator signatures → immediate pause
- Vote delegation: delegate → vote → undelegate

---

## P23: End-to-End (E2E) Tests

### [ ] T23.1 — E2E Test Suite (`tests/e2e/`)

**Files**: `tests/e2e/mod.rs`, `tests/e2e/harness.rs`, `tests/e2e/test_full_node_lifecycle.rs`, `tests/e2e/test_consensus_block_production.rs`, `tests/e2e/test_multi_node_network.rs`, `tests/e2e/test_malicious_proposer.rs`, `tests/e2e/test_fork_upgrade.rs`, `tests/e2e/test_evm_compatibility.rs`, `tests/e2e/test_stress.rs`

**Test harness (`harness.rs`)** — inspired by Tempo's `tempo_e2e` deterministic runtime approach:
- `TestNode` struct — encapsulates consensus + execution layer start/stop, with custom genesis injection
- `DeterministicRuntime` — runs consensus engine in deterministic simulation (seed-based task scheduling) while maintaining tokio async environment; same seed = same execution order for reproducible test failures
- `NetworkSimulator` — inject network partition, latency, message drop between test nodes
- `SharedTxCorpus` — reusable transaction corpus shared with T22.1 integration tests (avoid duplicate tx construction logic between integration and E2E layers)
- All E2E tests: 5-minute timeout per test (prevent CI hangs from sync/stall issues)

**`test_full_node_lifecycle.rs`**:
- Start fresh node → load genesis → sync to latest → process transactions → query via RPC → shut down → restart → verify state recovery
- Light client sync: download snapshot → verify → incremental sync

**`test_consensus_block_production.rs`**:
- Single validator produces blocks continuously for 1000 blocks
- Verify block structure: headers, roots, execution order
- Verify base fee dynamics over 100 blocks
- Verify fee distribution: burn + validator rewards
- Verify nonce progression
- Verify receipt root matches individual receipts

**`test_multi_node_network.rs`**:
- 3 validator nodes + 2 full nodes
- P2P gossip: tx submitted to node A appears on node B, C
- Consensus: validators agree on blocks, finality reached
- Network partition: split network → no blocks produced → restore partition → blocks resume
- Validator subset rotation verified across rounds

**`test_malicious_proposer.rs`** (new — inspired by Tempo milestone #3):
- Double-sign: validator signs two blocks at same height → verify full self-stake slash
- Offline: validator goes silent for 100 blocks → verify proportional slash
- Invalid tx: proposer includes malformed transaction → block rejected
- Invalid root: proposer submits block with mismatched state root → block rejected
- Recovery: slashed validator replaced by next in line → chain continues normally

**`test_fork_upgrade.rs`**:
- Deploy chain with version 1.0.0
- Submit governance proposal for version 1.1.0 at height X
- Reach height X → new rules activate
- Nodes running old version reject new blocks
- All nodes upgrade → chain continues

**`test_evm_compatibility.rs`**:
- Deploy ERC-20 token via Foundry (tempo-foundry style fork if needed)
- Execute token transfer via eth_sendRawTransaction
- Query balance via eth_getBalance
- Verify logs via eth_getLogs
- Deploy DEX contract → execute swap → verify balances
- Protocol asset bridge to EVM → use in DeFi

**`test_stress.rs`**:
- Submit 10,000 transactions/sec for 60 seconds
- Verify mempool handles load without crash
- Verify block production maintains <250ms block time
- Verify no transaction double-spend
- Verify final state matches expected balances

---

## Task Dependency Graph

```
P0 (Foundation)
  T0.1 primitives ─┐
  T0.2 crypto ─────┼──→ P1 (Protocol Layer)
  T0.3 serialization┤     T1.1 registry
  T0.4 storage ────┘     T1.2 balances
                         T1.3 compliance
                         T1.4 instructions ← T1.1, T1.2, T1.3
                         T1.5 transaction & gas ← T1.4, T0.4
                         T1.6 smart accounts ← T1.5
                         T1.7 fee currency ← T1.5
                         T1.8 sponsor system ← T1.5, T1.7
                         T1.9 receipts ← T1.5

P1 + P0 ──────────→ P2 (EVM Layer)
                         T2.1 executor
                         T2.2 ERC-20 template
                         T2.3 precompiles ← T2.1

P1 + P2 ──────────→ P3 (Bridge Layer)
                         T3.1 internal bridge
                         T3.2 external bridge

P1 + P3 ──────────→ P4 (Agent Payments)
                         T4.1 agent crate

P1 ───────────────→ P5 (Shielded Pool)
                         T5.1 shielded crate

P0 + P1 + P2 + P3 ─→ P6 (Consensus)
                         T6.1 consensus integration
                         T6.2 validator staking

P0 ───────────────→ P7 (Network)
                         T7.1 network crate

P1 + P7 ──────────→ P8 (Mempool)
                         T8.1 transaction pool

P1 + P6 + P8 ─────→ P9 (Payload Builder)
                         T9.1 payload builder
                         T9.2 payload types

P0 ───────────────→ P10 (Chain Spec)
                         T10.1 genesis

P1 + P2 + P3 + P4 + P5 → P11 (RPC)
                         T11.1 RPC crate

P1 + P6 ──────────→ P12 (Governance)
                         T12.1 governance

P12 ──────────────→ P14 (Node App)
                         T14.1 node + CLI

P14 ──────────────→ P15 (Telemetry)
                         T15.1 telemetry

P0 + P6 ──────────→ P16 (Light Client)
                         T16.1 light client

P14 ──────────────→ P17 (Logging)
                         T17.1 logging

P1 ───────────────→ P18 (Oracle)
                         T18.1 oracle

P6 + P12 ─────────→ P19 (Fork/Upgrade)
                         T19.1 fork management

P0 + P1 + P2 ─────→ P20 (State Expiration)
                         T20.1 expiration

P1 + P5 + P7 + P6 → P21 (Security)
                         T21.1 security (incl. MEV protection §13.2)

P1-P18 ───────────→ P22 (Integration Tests)
                         T22.1 integration suite

P1-P22 ───────────→ P23 (E2E Tests)
                         T23.1 E2E suite
```

---

## Implementation Order (Recommended)

1. **Week 1-2**: P0 (T0.1 → T0.4) — All foundation crates
2. **Week 3-6**: P1 (T1.1 → T1.11) — Protocol payment layer + economics + issuer
3. **Week 7-8**: P2 (T2.1 → T2.3) — EVM layer
4. **Week 9**: P3 (T3.1 → T3.2) — Bridge layer
5. **Week 10**: P4 (T4.1) — Agent payments
6. **Week 11-12**: P5 (T5.1) — Shielded Pool (ZK)
7. **Week 13-14**: P6 (T6.1 → T6.2) — Consensus integration
8. **Week 15**: P7 (T7.1) — Network
9. **Week 16**: P8 (T8.1) — Mempool
10. **Week 17**: P9 (T9.1 → T9.2) — Payload builder
11. **Week 18**: P10 (T10.1) — Chain spec / Genesis
12. **Week 19**: P11 (T11.1) — RPC layer
13. **Week 20**: P12 (T12.1) — Governance
14. **Week 21**: P14 (T14.1) — Node app + CLI
15. **Week 22**: P15-P18 (T15.1 → T18.1) — Telemetry, light client, logging, oracle
16. **Week 23**: P19-P20 (T19.1 → T20.1) — Fork, state expiration
17. **Week 24**: P21 (T21.1) — Security hardening + MEV protection
18. **Week 25**: P22 (T22.1) — Integration tests
19. **Week 26**: P23 (T23.1) — E2E tests
20. **Week 27**: Spec cross-validation, audit prep, documentation

---

## Spec Validation Checklist

After all tasks complete, verify against spec.md section-by-section:

- [ ] §1 Overview — dual execution architecture reflected in crate structure
- [ ] §2 Consensus — Simplex BFT, 216 validators, 21 subset, 250ms blocks
- [ ] §3 Protocol Payment — all Instruction variants, GasConfig, FeeCurrency, AuthScheme
- [ ] §4 EVM — Revm integration, ERC-20 templates, independent contracts
- [ ] §5 Internal Bridge — deposit/withdraw, same-block completion
- [ ] §5.6 External Bridge — validator signatures, deposit/withdraw flows
- [ ] §6 Agent — registration, permissions, funding, execution, gas discount
- [ ] §7 Issuer — IssuerAction enum, freeze/unfreeze, transfer ownership, issuer limitations
- [ ] §8 Network — commonware-p2p, gossip priorities
- [ ] §9 Serialization — RLP for P2P/storage, Serde for RPC/config
- [ ] §10 Storage — reth-db, prune strategy, snapshots, fast sync flow
- [ ] §11 RPC — all standard + Callchain + WS methods
- [ ] §12 Economics — token distribution, EIP-1559, fee allocation (CALL burn + stablecoin treasury), staking, gas unit table
- [ ] §13 Security — crypto, MEV (PBS + commit-reveal), governance, attack prevention
- [ ] §14 Performance — TPS target, block time, finality
- [ ] §15 Crate structure — matches §15 directory layout
- [ ] §16 Genesis — JSON format, initialization flow
- [ ] §17 Mempool — pools, priority, capacity, eviction
- [ ] §18 State Transition — apply_block, validity rules, atomicity, receipts (incl. ChangeType, receipt prune)
- [ ] §19 Fork/Upgrade — height activation, governance trigger
- [ ] §20 Telemetry — Prometheus metrics, alerts
- [ ] §21 Boot/Config — CLI, TOML, boot sequence
- [ ] §22 State Expiration — no expiration for protocol, EIP-161 for EVM, expiration vs prune relationship
- [ ] §23 Light Client — header verification, 8 RPC methods, proof types, sync strategy
- [ ] §24 Logging — structured LogEntry, audit log (AppendOnly, ShieldedAuditInfo), compliance report
- [ ] §25 Oracle — submissions, aggregation, precompile, TWAP, full config params
- [ ] §26 Design Decisions — dual ledger, Simplex, Reth rationale preserved
