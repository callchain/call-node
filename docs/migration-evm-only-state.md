# Migration Plan: EVM-Only State Architecture (Tempo-Style Stateful Precompiles)

**Goal:** All on-chain state lives in EVM accounts/storage. All system functionality is exposed through **stateful precompiles** that read/write EVM storage via revm's `Journal`. The block `state_root` is exactly the EVM state root. Protocol-state structs (`AccountState`, `AssetRegistry`, `ComplianceEngine`, etc.) are removed from `RpcState`.

**Reference architecture:** This plan follows the `tempo` precompile pattern: native Rust precompiles at fixed addresses, accessed via standard EVM `CALL`, with state stored in EVM storage slots under each precompile's address. No Solidity system contracts, no TLS protocol-state hooks.

**Current state (as of 2026-05-03):**

| Phase | Status | Notes |
|-------|--------|-------|
| Phase 1 — Storage Layout Design | ✅ Done | Slot layouts defined for all 9 precompiles; `storage_slot()` helper + `slot_balance`, `slot_asset_meta`, etc. in `helpers/utils.rs` |
| Phase 2 — BlockHeader Simplification | ✅ Done | `BlockHeader` now has single `state_root` = EVM state root; `compute_payment_root` / `bridge_root` / `receipt_root` removed |
| Phase 3 — Stateful Precompile Infra | ✅ Done | `StorageProvider`, `EvmStorageProvider`, `StorageCtx` (TLS), `StatefulPrecompile` trait, `CallPrecompiles::run`, `input_cost()`, `fill_precompile_output()` all implemented; `state_hook.rs` deleted; `OnceLock` registration removed |
| Phase 4 — Precompile Migration | ✅ Done | All 9 precompiles fully migrated to `StorageCtx::sload/sstore`. No protocol-state dependencies remain. `shielded.rs` uses `call_shielded` for pure cryptography (Poseidon, ZK proofs, Merkle trees) — not protocol state. |
| Phase 5 — Consensus/BFT | ✅ Done | `SimplexConsensus` now reads/writes validator state directly from EVM storage via `evm_instructions.rs`. `ValidatorStateManager` removed from `SimplexConsensus`. `ExecutionState` no longer holds `shielded_state`. All consensus callers (`node`, `rpc`, `payload-builder`) updated. Tests pass. |
| Phase 6 — RpcState | ✅ Done | All protocol-state fields removed from `RpcState`: `balance_state`, `asset_registry`, `compliance_engine`, `bridge_state`, `validator_state`, `agent_registry`, `shielded_state`, `governance`, `oracle`, `fee_currency_registry`. Remaining: `evm_state` + non-chain metadata (receipts, filters, network handles, etc.). |
| Phase 7 — Persistence | ✅ Done | `LoadedState` simplified to `evm_state` + `agent_nonces` + `governance` + `fee_params`. Protocol-state save/load removed. Unused DB table types cleaned up from `reth_db.rs`. Only 14 used table types remain. |
| Phase 8 — Genesis | ✅ Done | Genesis initializes only `EvmState` with pre-seeded system storage slots. No separate `AccountState`, `AssetRegistry`, `ComplianceEngine`, etc. initialization. |
| Phase 9 — Testing | ✅ Done | Precompile unit tests pass (58/58). All crate tests pass: `call-consensus` 76/76, `call-rpc` 17/17, `call-node` lib 65/65, `call-node` integration all pass. |
| Phase 10 — Cleanup | ✅ Done | All dead protocol-state structs removed: `AccountState`, `AssetRegistry`, `ComplianceEngine`, `BridgeStateManager`, `FeeCurrencyRegistry`, `ValidatorStateManager`/`ValidatorMetaSnapshot`, `AgentRegistry`/`AgentBalances`, `IssuerState`, `SponsorRegistry`. Unused DB table types removed from `reth_db.rs`. `save_balances`/`load_balances` deleted. `OracleManager` and `GovernanceManager` remain as sidecars. `docs/spec.md` and `docs/spec_cn.md` BlockHeader sections updated. |

**Target architecture:**
- `BlockHeader` has a single `state_root` field = `evm_state_root`.
- Precompiles are **stateful**: they receive `calldata`, `caller`, and a `StorageProvider` backed by revm's live `Journal`. Storage reads/writes go through revm's native journal, so gas accounting, revert, and checkpointing are automatic.
- All precompile state is stored in the `storage` map of their respective precompile addresses (e.g., `0x201` storage holds asset balances, `0x204` storage holds validator stakes).
- `RpcState` holds only `evm_state` + non-chain metadata (subscriptions, filters, network handles, etc.).
- Persistence saves/loads `EvmState` + sidecars (oracle, governance) + consensus meta + receipts.
- Genesis initializes EVM accounts with pre-seeded system storage slots.
- Switch precompile (`0x207`) remains disabled as-is.

---

## Phase 1: EVM Storage Layout Design

Before writing any migration code, define the storage-slot layout for every system function. This is the contract ABI for system state. Use standard Solidity storage semantics so RPC and external tools can decode it.

### 1.1 Asset precompile (`0x201`)

| Data | Slot key (U256) | Value (U256) |
|---|---|---|
| Balance of `(asset_id, address)` | `keccak256(asset_id ‖ address)` | balance (u128 in low 128 bits) |
| Allowance of `(asset_id, owner, spender)` | `keccak256(asset_id ‖ owner ‖ spender)` | allowance (u128) |
| Asset metadata count | `0` | next asset_id |
| Asset metadata for `asset_id` | `keccak256(asset_id ‖ "meta")` | packed symbol hash, decimals, status, issuer |
| Asset total supply for `asset_id` | `keccak256(asset_id ‖ "supply")` | total supply (u128) |
| Asset issuer for `asset_id` | `keccak256(asset_id ‖ "issuer")` | issuer address |

### 1.2 Validator precompile (`0x204`)

| Data | Slot key | Value |
|---|---|---|
| Validator list length | `0` | count |
| Validator address at index | `keccak256("validators") + index` | address |
| Stake of validator | `keccak256(address ‖ "stake")` | stake amount (u128) |
| Unbonding queue length | `1` | count |
| Unbonding entry | `keccak256("unbonding") + index` | packed (address, amount, unlock_height) |
| Active validator bitmap | `keccak256("active")` | bit-packed bitmap |

### 1.3 Bridge precompile (`0x103`)

| Data | Slot key | Value |
|---|---|---|
| Total deposits for asset | `keccak256(asset_id ‖ "deposits")` | amount (u128) |
| Total withdrawals for asset | `keccak256(asset_id ‖ "withdrawals")` | amount (u128) |
| Pending ops Merkle root | `keccak256("pending_root")` | bytes32 |
| Authorized Ethereum contract | `keccak256("eth_contract")` | address |

### 1.4 Oracle precompile (`0x101`)

| Data | Slot key | Value |
|---|---|---|
| Median price for asset | `keccak256(asset_id ‖ "price")` | price (u128) |
| TWAP for asset | `keccak256(asset_id ‖ "twap")` | twap (u128) |
| Last update timestamp | `keccak256(asset_id ‖ "ts")` | timestamp (u64) |
| Last update block | `keccak256(asset_id ‖ "block")` | block number (u64) |

### 1.5 Governance precompile (`0x203`)

| Data | Slot key | Value |
|---|---|---|
| Proposal count | `0` | next proposal_id |
| Proposal metadata | `keccak256(proposal_id ‖ "prop")` | packed (proposer, start_block, end_block, status, action_type) |
| Vote tally | `keccak256(proposal_id ‖ "tally")` | packed (for_votes, against_votes) |
| Individual vote | `keccak256(proposal_id ‖ voter ‖ "vote")` | vote value (u8) |

### 1.6 Compliance precompile (`0x205`)

| Data | Slot key | Value |
|---|---|---|
| Policy count | `0` | next policy_id |
| Policy rules hash | `keccak256(policy_id ‖ "rules")` | rules hash (bytes32) |
| Address compliance status | `keccak256(policy_id ‖ address ‖ "ok")` | bool (0/1) |

### 1.7 Shielded precompile (`0x202`)

| Data | Slot key | Value |
|---|---|---|
| Nullifier set root | `keccak256("nullifier_root")` | bytes32 |
| Commitment tree root | `keccak256("commitment_root")` | bytes32 |
| Next tree index | `keccak256("tree_index")` | index (u64) |

### 1.8 Agent precompile (`0x209`)

| Data | Slot key | Value |
|---|---|---|
| Agent registry length | `0` | count |
| Agent info | `keccak256(agent_id ‖ "info")` | packed (owner, domain_hash, permissions) |
| Agent balance | `keccak256(agent_id ‖ asset_id ‖ "bal")` | amount (u128) |
| Agent nonce | `keccak256(agent_id ‖ "nonce")` | nonce (u64) |

**Design rule:** All storage layouts use 32-byte slots (U256 keys → U256 values). Precompiles encode/decode the same way a Solidity contract would. This makes `eth_getStorageAt` meaningful for every system contract.

---

## Phase 2: BlockHeader & Consensus Root Simplification

**Status:** ✅ Complete

**Files:** `crates/consensus/src/block.rs`

`BlockHeader` now has a single `state_root` field. `payment_root`, `evm_state_root`, `bridge_root`, `receipt_root` were removed. `Block::finalize` sets `header.state_root = result.state_root`.

---

## Phase 3: Stateful Precompile Infrastructure (Tempo Pattern)

**Status:** ✅ Complete

**Files:** `crates/precompiles/src/lib.rs`, `crates/precompiles/src/storage.rs`

- `StorageProvider` trait with `sload`, `sstore`, `emit_event`, `checkpoint`, `deduct_gas`
- `EvmStorageProvider` backed by revm's live journal
- `StorageCtx` TLS singleton for precompile business logic
- `StatefulPrecompile` trait with `call(calldata, msg_sender)`
- `CallPrecompiles::run` wraps standard + custom precompiles
- `input_cost()`, `storage_slot()`, `fill_precompile_output()` helpers
- Old `state_hook.rs` deleted; `OnceLock` registration pattern removed

---

## Phase 4: Precompile Migration (Per-Precompile)

**Status:** ✅ Complete

All 9 precompiles migrated to `StorageCtx::sload/sstore`. No protocol-state dependencies remain in the precompile layer.

| Precompile | Address | Status |
|---|---|---|
| Asset | `0x201` | ✅ Migrated |
| Validator | `0x204` | ✅ Migrated |
| Bridge | `0x103` | ✅ Migrated |
| Oracle | `0x101` | ✅ Migrated |
| Governance | `0x203` | ✅ Migrated |
| Compliance | `0x205` | ✅ Migrated |
| Shielded | `0x202` | ✅ Migrated (root-only in EVM; full tree in sidecar) |
| Agent | `0x209` | ✅ Migrated |
| Switch | `0x207` | ⏸️ Disabled (unchanged) |

---

## Phase 5: Consensus / BFT Layer Changes

**Status:** ✅ Complete

**Files:** `crates/consensus/src/validator.rs`, `crates/consensus/src/block.rs`, `crates/consensus/src/simplex.rs`

- `SimplexConsensus` reads/writes validator state directly from EVM storage via `evm_instructions.rs`
- `ValidatorStateManager` removed from `SimplexConsensus`
- `ExecutionState` no longer holds `shielded_state`
- All consensus callers (`node`, `rpc`, `payload-builder`) updated
- Tests pass

---

## Phase 6: RpcState & RPC Handlers

**Status:** ✅ Complete

**Files:** `crates/rpc/src/handlers/state.rs`, all handler files

### 6.1 Simplified RpcState

All protocol-state fields removed. Current `RpcState`:

```rust
pub struct RpcState {
    pub evm_state: RwLock<EvmState>,        // single source of truth
    pub agent_nonces: RwLock<call_agent::AgentNonces>,
    pub receipts: RwLock<HashMap<TxHash, ProtocolReceipt>>,
    pub current_block: RwLock<u64>,
    pub fee_params: RwLock<FeeParams>,
    pub consensus_params: RwLock<ConsensusParams>,
    pub mempool: Arc<RwLock<Mempool>>,
    pub mempool_defense: RwLock<MempoolDefense>,
    pub chain_id: u64,
    pub subscriptions: SubscriptionManager,
    pub fork_manager: RwLock<ForkManager>,
    pub require_governance_auth: AtomicBool,
    pub signer: RwLock<Option<SignerRef>>,
    pub bls_secret_key: RwLock<Option<call_crypto::BlsSecretKey>>,
    #[cfg(feature = "light-client-bridge")]
    pub light_client: RwLock<Option<call_light_client::EthLightClient>>,
    pub pending_rollback: RwLock<Option<RollbackPlan>>,
    pub log_index: RwLock<HashMap<Address, Vec<(u64, TxHash, usize)>>>,
    pub data_dir: RwLock<Option<PathBuf>>,
    pub peer_heights: Arc<RwLock<HashMap<String, u64>>>,
    pub engine_restart_signal: AtomicBool,
    pub network: Arc<RwLock<Option<Arc<dyn call_network::Network>>>>,
    pub fee_history: RwLock<VecDeque<(u64, BlockFeeEntry)>>,
    pub block_hash_index: RwLock<HashMap<Hash, u64>>,
    pub filter_manager: FilterManager,
    pub sync_progress: Arc<RwLock<Option<SyncProgress>>>,
}
```

### 6.2 Removed fields

| Field | Removed from RpcState | Reads now go to |
|---|---|---|
| `balance_state` | ✅ | EVM `ASSET_ADDRESS` storage |
| `asset_registry` | ✅ | EVM `ASSET_ADDRESS` storage |
| `compliance_engine` | ✅ | EVM `COMPLIANCE_ADDRESS` storage |
| `bridge_state` | ✅ | EVM `BRIDGE_ADDRESS` storage |
| `validator_state` | ✅ | EVM `VALIDATOR_ADDRESS` storage |
| `agent_registry` | ✅ | EVM `AGENT_ADDRESS` storage |
| `agent_balances` | ✅ | EVM `AGENT_ADDRESS` storage |
| `shielded_state` | ✅ | EVM `SHIELDED_ADDRESS` storage |
| `governance` | ✅ | Sidecar on `CallNode`; RPC reads from EVM `GOVERNANCE_ADDRESS` |
| `oracle` | ✅ | Sidecar on `CallNode`; RPC reads from EVM `ORACLE_ADDRESS` |
| `fee_currency_registry` | ✅ | Removed (no-op in governance) |

---

## Phase 7: Persistence Layer

**Status:** ✅ Complete

**Files:** `crates/node/src/state_persist.rs`, `crates/storage/src/reth_db.rs`

### 7.1 Simplified LoadedState

```rust
pub(crate) struct LoadedState {
    pub evm_state: EvmState,
    pub agent_nonces: call_agent::AgentNonces,
    pub governance: GovernanceManager,   // sidecar, still persisted
    pub fee_params: FeeParams,
}
```

### 7.2 Still persisted

| Data | Table | Reason |
|---|---|---|
| EVM accounts | `CallEvmAccounts` | Single source of truth |
| Agent nonces | `CallAgentNonces` | Not in EVM (agent nonce tracking) |
| Governance | `CallGovernanceState` | Sidecar state machine |
| Oracle | `CallOracleState` | Sidecar state |
| Consensus | `CallConsensusState` | BFT meta (not validator stakes) |
| Fee params | `CallFeeParams` | Not in EVM |
| Receipts | `CallReceipts` / `CallReceiptsByBlock` | RPC indexing |
| Fork state | `CallForkState` | Protocol upgrade scheduling |
| Prune state | `CallPruneState` | Storage pruning |
| Checkpoint | `CallCheckpoint` | Crash recovery |

### 7.3 Dead code removed

- `load_receipts_by_block`, `delete_receipts_by_block` deleted from `state_persist.rs`
- Balance/asset/bridge/shielded/agent/validator/compliance persistence removed

### 7.4 Cleanup completed

- Unused DB table types removed from `reth_db.rs` (e.g., `CallProtocolAssets`, `CallBridgeOps`, `CallShieldedNullifiers`, etc.)

---

## Phase 8: Genesis Initialization

**Status:** ✅ Complete

**Files:** `crates/chainspec/src/genesis.rs`

Genesis now initializes only `EvmState` with pre-seeded system storage slots:
- Precompile addresses created with empty code
- Initial balances seeded via `ASSET_ADDRESS` storage
- Initial validators seeded via `VALIDATOR_ADDRESS` storage
- Asset metadata seeded via `ASSET_ADDRESS` storage

No separate `AccountState`, `AssetRegistry`, `ComplianceEngine`, `BridgeStateManager`, `ValidatorStateManager`, `GovernanceManager`, `OracleManager`, `ShieldedState`, `AgentRegistry`, `AgentBalances` initialization.

---

## Phase 9: Testing & Verification

**Status:** ✅ Complete

| Test Suite | Result |
|---|---|
| `cargo test -p call-consensus` | 76 passed |
| `cargo test -p call-rpc` | 17 passed |
| `cargo test -p call-node --lib` | 65 passed |
| `cargo test -p call-node --tests` | All integration tests passed |
| Precompile unit tests | 58/58 passed |

All precompiles, consensus, RPC, and integration tests pass after migration.

---

## Phase 10: Cleanup & Deprecation

**Status:** ✅ Complete

### 10.1 Dead structs (all removed)

| Struct | Location | Status |
|---|---|---|
| `AccountState` | `call_protocol::account` | ✅ Deleted |
| `AssetRegistry` | `call_protocol::registry` | ✅ Deleted |
| `ComplianceEngine` | `call_protocol::compliance` | ✅ Gutted — `CompliancePolicy`/`ComplianceStatus` enums kept for precompile |
| `FeeCurrencyRegistry` | `call_protocol::fee_currency` | ✅ Deleted |
| `BridgeStateManager` | `call_bridge::lib` | ✅ Deleted |
| `ValidatorStateManager`/`ValidatorMetaSnapshot` | `call_consensus::validator` | ✅ Removed from `validator.rs` |
| `AgentRegistry` | `call_agent::registry` | ✅ Deleted |
| `AgentBalances` | `call_agent::balances` | ✅ Deleted |
| `IssuerState` | `call_protocol::issuer` | ✅ Deleted |
| `SponsorRegistry` | `call_protocol::sponsor` | ✅ Deleted |
| `ShieldedState` | `call_shielded::lib` | ✅ Retained in `call_shielded` (active, not protocol-state) |
| `OracleManager` | `call_oracle::manager` | ✅ **Used as sidecar** |
| `GovernanceManager` | `call_governance` | ✅ **Used as sidecar** |

### 10.2 Cleanup completed

- ✅ Deleted unused DB table types from `reth_db.rs`
- ✅ Deleted dead protocol-state structs from source crates
- ✅ Removed `save_balances` / `load_balances` from `call_storage`
- ✅ Updated `docs/spec.md` / `docs/spec_cn.md` BlockHeader section

---

## Migration Order (Recommended)

Execute phases in this order to minimize breakage:

1. ✅ **Phase 1** — Design storage layouts. No code changes. Write slot layout specs.
2. ✅ **Phase 2** — BlockHeader simplification. This is a mechanical change. Update consensus tests.
3. ✅ **Phase 3** — Build the stateful precompile infrastructure (`StorageProvider`, `EvmStorageProvider`, `StorageCtx`, `StatefulPrecompile`, rewrite `CallPrecompiles::run`). Do NOT migrate any precompile logic yet — keep the old stubs returning errors.
4. ✅ **Phase 4a** — Migrate one precompile end-to-end: **Asset precompile (`0x201`)**. This is the most-used precompile and validates the pattern.
5. ✅ **Phase 4b** — Migrate Oracle precompile (`0x101`). Relatively simple, high impact.
6. ✅ **Phase 4c** — Migrate Validator precompile (`0x204`). Requires consensus-layer changes in Phase 5.
7. ✅ **Phase 5** — Update consensus to read validators from EVM storage. Run BFT tests.
8. ✅ **Phase 4d** — Migrate remaining precompiles (Bridge, Governance, Compliance, Shielded, Agent).
9. ✅ **Phase 6** — Simplify `RpcState` and update all RPC handlers.
10. ✅ **Phase 7** — Simplify persistence. Keep old tables as no-ops during transition.
11. ✅ **Phase 8** — Update genesis.
12. ✅ **Phase 9** — Comprehensive testing.
13. 🔄 **Phase 10** — Final cleanup (delete dead structs and unused DB table types).

---

## Risk Mitigation

| Risk | Mitigation |
|---|---|
| revm v36 `JournalTr` API differs from assumed | Verify `context.journal_mut()`, `journal.sload()`, `journal.sstore()`, and `JournalCheckpoint` API exist in your revm version before committing. If unavailable, use `revm::state::Journal` directly. |
| Gas double-counting (precompile fixed gas + storage gas) | Use the tempo pattern: precompile methods do NOT set `gas_used` manually. Instead, `EvmStorageProvider` tracks gas consumed by each operation. `CallPrecompiles::run` reads `provider.gas_used()` and fills it into the output. |
| Precompile storage changes not reverted on tx revert | Because `EvmStorageProvider` uses revm's live `Journal`, all sload/sstore operations are part of revm's checkpoint/revert system. If the outer EVM tx reverts, the journal reverts, and precompile storage changes go with it. |
| Shielded tree too large for EVM storage | Store only Merkle roots in EVM storage. Store full tree nodes in a sidecar indexed by root. Precompiles validate proofs against the root. |
| Performance regression from storage slot hashing | Precompute common slot keys (e.g., `"validators"` hash) as constants. Use `keccak256` const evaluation where possible. |
| Genesis hash changes | Document the breaking change. Testnets can reset. Mainnet would require a hard fork or coordinated migration block. |
| Validator state divergence during migration | Keep `ValidatorStateManager` as a read-only cache of EVM validator state during Phase 4c-5 transition. Consensus reads from cache; cache is rebuilt from EVM at epoch boundaries. |
