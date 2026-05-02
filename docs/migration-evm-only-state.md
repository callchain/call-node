# Migration Plan: EVM-Only State Architecture (Tempo-Style Stateful Precompiles)

**Goal:** All on-chain state lives in EVM accounts/storage. All system functionality is exposed through **stateful precompiles** that read/write EVM storage via revm's `Journal`. The block `state_root` is exactly the EVM state root. Protocol-state structs (`AccountState`, `AssetRegistry`, `ComplianceEngine`, etc.) are removed.

**Reference architecture:** This plan follows the `tempo` precompile pattern: native Rust precompiles at fixed addresses, accessed via standard EVM `CALL`, with state stored in EVM storage slots under each precompile's address. No Solidity system contracts, no TLS protocol-state hooks.

**Current state (as of 2026-05-02):**

| Phase | Status | Notes |
|-------|--------|-------|
| Phase 1 — Storage Layout Design | ✅ Done | Slot layouts defined for all 9 precompiles; `storage_slot()` helper + `slot_balance`, `slot_asset_meta`, etc. in `helpers/utils.rs` |
| Phase 2 — BlockHeader Simplification | ✅ Done | `BlockHeader` now has single `state_root` = EVM state root; `compute_payment_root` / `bridge_root` / `receipt_root` removed |
| Phase 3 — Stateful Precompile Infra | ✅ Done | `StorageProvider`, `EvmStorageProvider`, `StorageCtx` (TLS), `StatefulPrecompile` trait, `CallPrecompiles::run`, `input_cost()`, `fill_precompile_output()` all implemented; `state_hook.rs` deleted; `OnceLock` registration removed |
| Phase 4 — Precompile Migration | ✅ Done | All 9 precompiles fully migrated to `StorageCtx::sload/sstore`. No protocol-state dependencies remain. `shielded.rs` uses `call_shielded` for pure cryptography (Poseidon, ZK proofs, Merkle trees) — not protocol state. |
| Phase 5 — Consensus/BFT | ✅ Done | `SimplexConsensus` now reads/writes validator state directly from EVM storage via `evm_instructions.rs`. `ValidatorStateManager` removed from `SimplexConsensus`. `ExecutionState` no longer holds `shielded_state`. All consensus callers (`node`, `rpc`, `payload-builder`) updated. Tests pass. |
| Phase 6 — RpcState | ❌ Not started | `RpcState` still holds `balance_state`, `asset_registry`, `compliance_engine`, `bridge_state`, `validator_state`, `agent_registry`, `shielded_state`, `governance`, `oracle`, `fee_currency_registry` |
| Phase 7 — Persistence | ❌ Not started | Likely still saves/loads multiple protocol-state tables beyond `EvmState` |
| Phase 8 — Genesis | ❌ Not started | Genesis probably still initializes `AccountState`, `AssetRegistry`, `ComplianceEngine`, etc. separately |
| Phase 9 — Testing | 🔄 Partial | Precompile unit tests pass (58/58). Missing: state-root determinism, revert, genesis-hash, RPC-parity tests |
| Phase 10 — Cleanup | ❌ Not started | `AccountState`, `AssetRegistry`, `ComplianceEngine`, `BridgeStateManager`, `ValidatorStateManager`, `AgentRegistry`, `OracleManager`, `GovernanceManager`, `ShieldedState` still exist in other crates |

**Target architecture:**
- `BlockHeader` has a single `state_root` field = `evm_state_root`.
- Precompiles are **stateful**: they receive `calldata`, `caller`, and a `StorageProvider` backed by revm's live `Journal`. Storage reads/writes go through revm's native journal, so gas accounting, revert, and checkpointing are automatic.
- All precompile state is stored in the `storage` map of their respective precompile addresses (e.g., `0x201` storage holds asset balances, `0x204` storage holds validator stakes).
- `RpcState` holds only `evm_state` + non-chain metadata (subscriptions, filters, network handles, etc.).
- Persistence only saves/loads `EvmState`.
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

**Files:** `crates/consensus/src/block.rs`, `crates/consensus/src/block.rs` (BlockHeader struct)

### 2.1 Simplify BlockHeader

```rust
pub struct BlockHeader {
    pub parent_hash: BlockHash,
    pub height: u64,
    pub timestamp_millis: u64,
    pub state_root: Hash,        // ← this IS the EVM state root
    pub proposer: ValidatorId,
    pub signature: BlockSignature,
    pub version: ProtocolVersion,
    pub bls_aggregate_signature: Option<Vec<u8>>,
    pub bls_signer_bitmap: Vec<u8>,
}
```

- Remove `payment_root`, `evm_state_root`, `bridge_root`, `receipt_root`.
- Rename the single root to `state_root`.
- Update `BlockHeader::hash()` to hash only `state_root`.

### 2.2 Simplify BlockExecutionResult

```rust
pub struct BlockExecutionResult {
    pub transaction_results: Vec<TransactionResult>,
    pub state_root: Hash,        // ← EVM state root
    pub evm_tx_count: usize,
    pub protocol_tx_count: usize,  // keep for metrics, should be 0
    pub bridge_op_count: usize,
    pub system_tx_count: usize,
    pub total_validator_reward: Balance,
    pub evm_gas_used: u64,
    pub agent_events: Vec<call_agent::AgentEvent>,
    pub pending_rollback: Option<RollbackPlan>,
    pub protocol_priority_fees: Vec<u128>,
    pub evm_tx_results: Vec<EvmTxResult>,
}
```

### 2.3 Simplify Block::execute root computation

```rust
// After all txs execute:
result.state_root = compute_evm_state_root(state.evm_state);
```

Remove `compute_payment_root`, `compute_bridge_root`, `compute_receipt_root` functions. Remove the aggregate keccak256 step.

### 2.4 Update Block::finalize

```rust
pub fn finalize(&mut self, result: &BlockExecutionResult) {
    self.header.state_root = result.state_root;
}
```

### 2.5 Update BFT/signature verification

Any code that verifies block signatures or checks root equality must be updated to compare only `state_root`. Search for `.payment_root`, `.bridge_root`, `.receipt_root` usage in consensus, sync, and light-client code.

---

## Phase 3: Stateful Precompile Infrastructure (Tempo Pattern)

**Files:** `crates/precompiles/src/lib.rs`, `crates/precompiles/src/storage.rs` (new file), all precompile files.

### 3.1 The Problem

revm's standard precompile API is stateless:
```rust
fn execute(input: &[u8], gas_limit: u64) -> PrecompileResult
```
It receives no `Journal` reference, no `caller`, and cannot access EVM storage. This is why callchain currently uses `StateHookGuard` with raw pointers to protocol state — a workaround that is unsafe, bypasses revm gas accounting, and does not support revert.

### 3.2 The Solution (as used in tempo)

Replace stateless precompiles with **stateful precompiles** that:
1. Receive `calldata`, `caller`, `gas_limit`, `is_static`, and a `StorageProvider`
2. Read/write EVM storage through the provider, which wraps revm's live `Journal`
3. Have gas deducted automatically per storage operation (cold/warm SLOAD/SSTORE)
4. Support revert via journal checkpoints

### 3.3 StorageProvider Abstraction

Create `crates/precompiles/src/storage.rs`:

```rust
use revm::primitives::{Address, U256, Log, LogData};
use revm::context_interface::JournalTr;
use revm::state::JournalCheckpoint;

/// Abstracts EVM storage access for precompiles.
///
/// Implemented by a journal-backed provider in production and by a test
/// double in unit tests.
pub trait StorageProvider {
    fn sload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError>;
    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<(), PrecompileError>;
    fn tload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError>;
    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<(), PrecompileError>;
    fn emit_event(&mut self, address: Address, event: LogData) -> Result<(), PrecompileError>;
    fn checkpoint(&mut self) -> JournalCheckpoint;
    fn checkpoint_commit(&mut self);
    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint);
    fn deduct_gas(&mut self, gas: u64) -> Result<(), PrecompileError>;
    fn refund_gas(&mut self, gas: i64);
    fn gas_used(&self) -> u64;
    fn gas_refunded(&self) -> i64;
    fn is_static(&self) -> bool;
    fn chain_id(&self) -> u64;
    fn timestamp(&self) -> U256;
    fn block_number(&self) -> u64;
    fn beneficiary(&self) -> Address;
}
```

### 3.4 EvmStorageProvider (Journal-backed)

```rust
/// Production [`StorageProvider`] backed by revm's live journal.
pub struct EvmStorageProvider<'a, J: JournalTr> {
    journal: &'a mut J,
    gas_remaining: u64,
    gas_refunded: i64,
    gas_limit: u64,
    is_static: bool,
}

impl<'a, J: JournalTr> EvmStorageProvider<'a, J> {
    pub fn new(journal: &'a mut J, gas_limit: u64, is_static: bool) -> Self {
        Self {
            journal,
            gas_remaining: gas_limit,
            gas_refunded: 0,
            gas_limit,
            is_static,
        }
    }

    pub fn new_max_gas(journal: &'a mut J) -> Self {
        Self::new(journal, u64::MAX, false)
    }
}

impl<'a, J: JournalTr> StorageProvider for EvmStorageProvider<'a, J> {
    fn sload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        let result = self.journal.sload(address, key)
            .map_err(|e| PrecompileError::Other(e.to_string()))?;
        // Warm read = 100, cold read = 2100 (Cancun)
        self.deduct_gas(if result.is_cold { 2100 } else { 100 })?;
        Ok(result.data)
    }

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other("static call cannot mutate state".into()));
        }
        let result = self.journal.sstore(address, key, value)
            .map_err(|e| PrecompileError::Other(e.to_string()))?;
        self.deduct_gas(result.data.gas_cost())?;
        self.refund_gas(result.data.gas_refund());
        Ok(())
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<(), PrecompileError> {
        self.deduct_gas(375 + 375 * event.topics().len() as u64 + 8 * event.data.len() as u64)?;
        self.journal.log(Log { address, data: event });
        Ok(())
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        self.journal.checkpoint()
    }

    fn checkpoint_commit(&mut self) {
        self.journal.checkpoint_commit();
    }

    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) {
        self.journal.checkpoint_revert(checkpoint);
    }

    fn deduct_gas(&mut self, gas: u64) -> Result<(), PrecompileError> {
        self.gas_remaining = self.gas_remaining
            .checked_sub(gas)
            .ok_or(PrecompileError::OutOfGas)?;
        Ok(())
    }

    fn refund_gas(&mut self, gas: i64) {
        self.gas_refunded = self.gas_refunded.saturating_add(gas);
    }

    fn gas_used(&self) -> u64 { self.gas_limit - self.gas_remaining }
    fn gas_refunded(&self) -> i64 { self.gas_refunded }
    fn is_static(&self) -> bool { self.is_static }
    // ... chain_id, timestamp, block_number, beneficiary via journal.host()
}
```

**Note:** The exact revm v36 `JournalTr` API may differ slightly (e.g., `sload` return type, `checkpoint` method names). Adapt the above to match your revm version.

### 3.5 StorageCtx (Thread-Local Context)

Precompile business logic should not carry around `&mut StorageProvider`. Use a TLS singleton (the tempo pattern):

```rust
use std::cell::RefCell;

thread_local! {
    static TL_STORAGE: RefCell<Option<*mut dyn StorageProvider>> = RefCell::new(None);
}

/// Thread-local storage context for precompiles.
///
/// Precompile code calls `StorageCtx::sload(...)` / `StorageCtx::sstore(...)`
/// without needing a `&mut StorageProvider` parameter.
pub struct StorageCtx;

impl StorageCtx {
    /// Enter a storage context. `provider` must outlive the closure.
    pub fn enter<R>(provider: &mut dyn StorageProvider, f: impl FnOnce() -> R) -> R {
        TL_STORAGE.with(|t| *t.borrow_mut() = Some(provider as *mut _));
        let result = f();
        TL_STORAGE.with(|t| *t.borrow_mut() = None);
        result
    }

    pub fn sload(address: Address, key: U256) -> Option<U256> {
        TL_STORAGE.with(|t| {
            t.borrow().and_then(|ptr| unsafe { (*ptr).sload(address, key).ok() })
        })
    }

    pub fn sstore(address: Address, key: U256, value: U256) -> Option<()> {
        TL_STORAGE.with(|t| {
            t.borrow().and_then(|ptr| unsafe { (*ptr).sstore(address, key, value).ok() })
        })
    }

    pub fn tload(address: Address, key: U256) -> Option<U256> {
        TL_STORAGE.with(|t| {
            t.borrow().and_then(|ptr| unsafe { (*ptr).tload(address, key).ok() })
        })
    }

    pub fn tstore(address: Address, key: U256, value: U256) -> Option<()> {
        TL_STORAGE.with(|t| {
            t.borrow().and_then(|ptr| unsafe { (*ptr).tstore(address, key, value).ok() })
        })
    }

    pub fn emit_event(address: Address, event: LogData) -> Option<()> {
        TL_STORAGE.with(|t| {
            t.borrow().and_then(|ptr| unsafe { (*ptr).emit_event(address, event).ok() })
        })
    }

    pub fn is_static() -> bool {
        TL_STORAGE.with(|t| {
            t.borrow().map(|ptr| unsafe { (*ptr).is_static() }).unwrap_or(false)
        })
    }

    pub fn gas_used() -> u64 {
        TL_STORAGE.with(|t| {
            t.borrow().map(|ptr| unsafe { (*ptr).gas_used() }).unwrap_or(0)
        })
    }

    pub fn gas_refunded() -> i64 {
        TL_STORAGE.with(|t| {
            t.borrow().map(|ptr| unsafe { (*ptr).gas_refunded() }).unwrap_or(0)
        })
    }

    pub fn deduct_gas(gas: u64) -> Option<()> {
        TL_STORAGE.with(|t| {
            t.borrow_mut().and_then(|ptr| unsafe { (*ptr).deduct_gas(gas).ok() })
        })
    }
}
```

**Safety:** The raw pointer is only valid during `StorageCtx::enter`. The guard pattern ensures it is cleared on exit, including during panic unwinding (use `std::panic::catch_unwind` inside `enter` if needed, or rely on `Drop` if you wrap it in a struct).

### 3.6 StatefulPrecompile Trait

```rust
/// Trait implemented by all Callchain custom precompiles.
pub trait StatefulPrecompile {
    /// Dispatch an EVM call to this precompile.
    ///
    /// `calldata` is ABI-encoded (4-byte selector + args).
    /// `msg_sender` is the EVM caller (`tx.origin` for top-level, or the calling contract).
    ///
    /// Implementations should:
    /// 1. Deduct `input_cost(calldata.len())` for calldata decoding
    /// 2. Decode the 4-byte selector
    /// 3. Route to the matching method
    /// 4. Use `StorageCtx::sload` / `StorageCtx::sstore` for state access
    /// 5. Return `PrecompileResult` (use `fill_precompile_output` to inject gas)
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult;
}
```

### 3.7 CallPrecompiles::run (Rewritten)

Replace the current `CallPrecompiles` (which uses `revm_precompile::Precompiles`) with a custom implementation that holds `Box<dyn StatefulPrecompile>`:

```rust
use std::collections::HashMap;

pub struct CallPrecompiles {
    precompiles: HashMap<Address, Box<dyn StatefulPrecompile>>,
    spec: SpecId,
}

impl CallPrecompiles {
    pub fn new(spec: SpecId) -> Self {
        let mut precompiles = HashMap::new();
        // Standard Ethereum precompiles (ecrecover, sha256, etc.)
        let eth = revm_precompile::Precompiles::new(spec.into());
        // ... wrap or delegate standard ones ...

        // Custom stateful precompiles
        precompiles.insert(ORACLE_ADDRESS, Box::new(OraclePrecompile));
        precompiles.insert(BRIDGE_ADDRESS, Box::new(BridgePrecompile));
        precompiles.insert(ASSET_ADDRESS, Box::new(AssetPrecompile));
        precompiles.insert(SHIELDED_ADDRESS, Box::new(ShieldedPrecompile));
        precompiles.insert(GOVERNANCE_ADDRESS, Box::new(GovernancePrecompile));
        precompiles.insert(VALIDATOR_ADDRESS, Box::new(ValidatorPrecompile));
        precompiles.insert(COMPLIANCE_ADDRESS, Box::new(CompliancePrecompile));
        precompiles.insert(SWITCH_ADDRESS, Box::new(SwitchPrecompile));
        precompiles.insert(AGENT_ADDRESS, Box::new(AgentPrecompile));

        Self { precompiles, spec }
    }
}

impl<CTX: revm::context::ContextTr> revm::handler::PrecompileProvider<CTX>
    for CallPrecompiles
{
    type Output = revm::interpreter::InterpreterResult;

    fn set_spec(&mut self, spec: <CTX::Cfg as revm::context::Cfg>::Spec) -> bool { /* ... */ }

    fn run(
        &mut self,
        context: &mut CTX,
        inputs: &revm::interpreter::CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        let Some(precompile) = self.precompiles.get_mut(&inputs.bytecode_address) else {
            return Ok(None);
        };

        let mut result = revm::interpreter::InterpreterResult {
            result: revm::interpreter::InstructionResult::Return,
            gas: revm::interpreter::Gas::new(inputs.gas_limit),
            output: revm::primitives::Bytes::new(),
        };

        // Extract calldata
        let input_bytes = match &inputs.input {
            revm::interpreter::CallInput::SharedBuffer(range) => {
                context.local()
                    .shared_memory_buffer_slice(range.clone())
                    .map(|s| s.to_vec())
                    .unwrap_or_default()
            }
            revm::interpreter::CallInput::Bytes(bytes) => bytes.0.to_vec(),
        };

        // Extract journal from context and create storage provider
        let journal = context.journal_mut(); // revm v36 API
        let mut provider = EvmStorageProvider::new(journal, inputs.gas_limit, inputs.is_static);

        // Execute precompile with storage context
        let exec_result = StorageCtx::enter(&mut provider, || {
            precompile.call(&input_bytes, inputs.caller)
        });

        // Fill gas from provider
        let gas_used = provider.gas_used();
        let gas_refunded = provider.gas_refunded();

        match exec_result {
            Ok(output) => {
                result.gas.record_refund(gas_refunded as u64);
                let underflow = result.gas.record_cost(gas_used);
                assert!(underflow, "Gas underflow is not possible");
                result.result = if output.reverted {
                    revm::interpreter::InstructionResult::Revert
                } else {
                    revm::interpreter::InstructionResult::Return
                };
                result.output = output.bytes;
            }
            Err(revm_precompile::PrecompileError::Fatal(e)) => return Err(e),
            Err(e) => {
                result.result = if e.is_oog() {
                    revm::interpreter::InstructionResult::PrecompileOOG
                } else {
                    revm::interpreter::InstructionResult::PrecompileError
                };
                if !e.is_oog() {
                    context.local_mut().set_precompile_error_context(e.to_string());
                }
            }
        }
        Ok(Some(result))
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        Box::new(self.precompiles.keys().cloned())
    }

    fn contains(&self, address: &Address) -> bool {
        self.precompiles.contains_key(address)
    }
}
```

### 3.8 Helper Functions

```rust
/// Gas cost for decoding calldata (per 32-byte word).
pub const INPUT_PER_WORD_COST: u64 = 6;

pub fn input_cost(len: usize) -> u64 {
    len.div_ceil(32).saturating_mul(INPUT_PER_WORD_COST as usize) as u64
}

/// Compute a storage slot from multiple concatenated byte slices.
pub fn storage_slot(parts: &[&[u8]]) -> U256 {
    let mut hasher = alloy_primitives::Keccak256::new();
    for part in parts {
        hasher.update(part);
    }
    U256::from_be_slice(hasher.finalize().as_slice())
}

/// Fill gas accounting on a PrecompileOutput from the current StorageCtx.
pub fn fill_precompile_output(mut output: PrecompileOutput) -> PrecompileOutput {
    output.gas_used = StorageCtx::gas_used();
    if !output.reverted {
        output.gas_refunded = StorageCtx::gas_refunded() as u64;
    }
    output
}
```

### 3.9 Delete Old Infrastructure

- **Delete** `crates/precompiles/src/state_hook.rs` entirely.
- **Delete** from `crates/consensus/src/block.rs` inside `Block::execute`:
  - `StateHookGuard::from_raw` call with protocol-state pointers
  - `ValidatorStateHookGuard` injection
  - `AgentStateHookGuard` injection
  - `BridgeStateHookGuard` injection
- **Delete** from `crates/precompiles/src/lib.rs`:
  - `VALIDATOR_PRECOMPILE_FN` `OnceLock`
  - `AGENT_PRECOMPILE_FN` `OnceLock`
  - `BRIDGE_EXT_PRECOMPILE_FN` `OnceLock`
  - `ORACLE_VALIDATOR_CHECK` `OnceLock`
  - `set_validator_precompile_fn`, `set_agent_precompile_fn`, etc.
  - `is_oracle_validator`
  - `get_live_oracle` / `set_live_oracle` / `ORACLE` `OnceLock`
  - `get_live_bridge` / `set_live_bridge` / `BRIDGE` `OnceLock`

---

## Phase 4: Precompile Migration (Per-Precompile)

For each precompile, replace protocol-state hook calls with `StorageCtx::sload` / `StorageCtx::sstore`. Remove all dependencies on `call_protocol`, `call_agent`, `call_bridge`, etc. state structs from the precompile layer.

### 4.0 Dispatch Pattern (Shared by All Precompiles)

Each precompile follows the same dispatch pattern:

```rust
impl StatefulPrecompile for AssetPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }

        StorageCtx::deduct_gas(input_cost(calldata.len()));

        let selector = &calldata[..4];
        match selector {
            &[0xd2, 0x14, 0x25, 0xdf] => asset_get_balance(calldata),
            &[0xd1, 0x5d, 0xcd, 0x62] => asset_transfer(calldata, msg_sender),
            // ... other selectors
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
        .map(fill_precompile_output)
    }
}
```

### 4.1 Asset precompile (`0x201`)

**Current:** Uses `with_account_state()` for balances/allowances, `with_registry()` for asset metadata.

**Migration:**

```rust
fn asset_get_balance(calldata: &[u8]) -> PrecompileResult {
    let asset_id = decode_u64(calldata, 4)?;
    let addr = decode_address(calldata, 36)?;

    let slot = storage_slot(&[&asset_id.to_be_bytes(), addr.as_slice()]);
    let balance = StorageCtx::sload(ASSET_ADDRESS, slot).unwrap_or(U256::ZERO);

    Ok(PrecompileOutput {
        bytes: encode_u256(balance.to::<u128>()),
        gas_used: 0, // filled by fill_precompile_output
        gas_refunded: 0,
        reverted: false,
    })
}

fn asset_transfer(calldata: &[u8], sender: Address) -> PrecompileResult {
    if StorageCtx::is_static() {
        return Err(PrecompileError::Other("static call".into()));
    }

    let asset_id = decode_u64(calldata, 4)?;
    let to = decode_address(calldata, 36)?;
    let amount = decode_u128(calldata, 68)?;

    // Deduct sender
    let sender_slot = storage_slot(&[&asset_id.to_be_bytes(), sender.as_slice()]);
    let sender_bal = StorageCtx::sload(ASSET_ADDRESS, sender_slot).unwrap_or(U256::ZERO);
    let new_sender = sender_bal.checked_sub(U256::from(amount))
        .ok_or_else(|| PrecompileError::Other("insufficient balance".into()))?;
    StorageCtx::sstore(ASSET_ADDRESS, sender_slot, new_sender);

    // Credit recipient
    let to_slot = storage_slot(&[&asset_id.to_be_bytes(), to.as_slice()]);
    let to_bal = StorageCtx::sload(ASSET_ADDRESS, to_slot).unwrap_or(U256::ZERO);
    StorageCtx::sstore(ASSET_ADDRESS, to_slot, to_bal + U256::from(amount));

    Ok(PrecompileOutput {
        bytes: Bytes::new(),
        gas_used: 0,
        gas_refunded: 0,
        reverted: false,
    })
}

fn asset_approve(calldata: &[u8], sender: Address) -> PrecompileResult {
    let asset_id = decode_u64(calldata, 4)?;
    let spender = decode_address(calldata, 36)?;
    let amount = decode_u128(calldata, 68)?;

    let slot = storage_slot(&[&asset_id.to_be_bytes(), sender.as_slice(), spender.as_slice()]);
    StorageCtx::sstore(ASSET_ADDRESS, slot, U256::from(amount));

    Ok(PrecompileOutput::new(Bytes::new(), false))
}

fn asset_mint(calldata: &[u8], sender: Address) -> PrecompileResult {
    let asset_id = decode_u64(calldata, 4)?;
    let to = decode_address(calldata, 36)?;
    let amount = decode_u128(calldata, 68)?;

    // Verify sender is issuer
    let issuer_slot = storage_slot(&[&asset_id.to_be_bytes(), b"issuer"]);
    let issuer = Address::from_slice(&StorageCtx::sload(ASSET_ADDRESS, issuer_slot)
        .unwrap_or(U256::ZERO).to_be_bytes::<32>()[12..]);
    if sender != issuer {
        return Err(PrecompileError::Other("not issuer".into()));
    }

    // Credit balance
    let bal_slot = storage_slot(&[&asset_id.to_be_bytes(), to.as_slice()]);
    let bal = StorageCtx::sload(ASSET_ADDRESS, bal_slot).unwrap_or(U256::ZERO);
    StorageCtx::sstore(ASSET_ADDRESS, bal_slot, bal + U256::from(amount));

    // Increase total supply
    let supply_slot = storage_slot(&[&asset_id.to_be_bytes(), b"supply"]);
    let supply = StorageCtx::sload(ASSET_ADDRESS, supply_slot).unwrap_or(U256::ZERO);
    StorageCtx::sstore(ASSET_ADDRESS, supply_slot, supply + U256::from(amount));

    Ok(PrecompileOutput::new(Bytes::new(), false))
}
```

### 4.2 Validator precompile (`0x204`)

**Current:** Fallback stub in `call-precompiles`, real impl registered from `call-consensus` via `OnceLock`. Uses `ValidatorStateHookGuard`.

**Migration:**
- Move the validator precompile entirely into `crates/precompiles/src/validator.rs`. Remove the `OnceLock` registration pattern.
- `stake`: `sload` current stake, `sstore` new stake. Deduct caller's native balance (sstore on caller's EVM balance slot at `ASSET_ADDRESS` for asset_id=0, or directly use `journal.balance_sub` / `journal.balance_incr`).
- `unstake`: `sstore` stake decrease, append to unbonding queue.
- `claimUnbonded`: read unbonding queue, check unlock height against current block (via `StorageCtx::block_number()`), `sstore` balance credit.

```rust
impl StatefulPrecompile for ValidatorPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        // ... selector dispatch ...
    }
}

fn validator_stake(calldata: &[u8], sender: Address) -> PrecompileResult {
    let amount = decode_u128(calldata, 4)?;

    // Read current stake
    let stake_slot = storage_slot(&[sender.as_slice(), b"stake"]);
    let current = StorageCtx::sload(VALIDATOR_ADDRESS, stake_slot).unwrap_or(U256::ZERO);
    StorageCtx::sstore(VALIDATOR_ADDRESS, stake_slot, current + U256::from(amount));

    // Add to validator list if new
    let count_slot = U256::ZERO;
    let count = StorageCtx::sload(VALIDATOR_ADDRESS, count_slot).unwrap_or(U256::ZERO);
    if current.is_zero() {
        StorageCtx::sstore(VALIDATOR_ADDRESS, count_slot, count + U256::from(1));
        let addr_slot = U256::from(count.to::<u64>()) + storage_slot(&[b"validators"]);
        StorageCtx::sstore(VALIDATOR_ADDRESS, addr_slot, U256::from_be_slice(sender.as_slice()));
    }

    Ok(PrecompileOutput::new(Bytes::new(), false))
}
```

### 4.3 Bridge precompile (`0x103`)

**Current:** Fallback stub in `call-precompiles`, real impl registered from `call-bridge` via `OnceLock`.

**Migration:**
- Merge bridge precompile logic into `crates/precompiles/src/bridge.rs`.
- `getTotalDeposits`: `StorageCtx::sload(BRIDGE_ADDRESS, keccak256(asset_id ‖ "deposits"))`
- `externalBridgeDeposit`: verify validator signatures (read validator list from `VALIDATOR_ADDRESS`), `sstore` deposit increase, `sstore` credit to recipient balance (at `ASSET_ADDRESS`).
- `externalBridgeWithdraw`: `sstore` balance deduction, `sstore` withdrawal increase.

### 4.4 Oracle precompile (`0x101`)

**Current:** Uses `get_live_oracle()` / `OracleManager` via `OnceLock`.

**Migration:**
- Remove `get_live_oracle()`, `set_live_oracle()`, and the `ORACLE` `OnceLock`.
- `getPrice`: `StorageCtx::sload(ORACLE_ADDRESS, keccak256(asset_id ‖ "price"))`
- `submitPrice`: verify caller is validator (read validator set from `VALIDATOR_ADDRESS` storage), update price/timestamp/block slots.

### 4.5 Governance precompile (`0x203`)

**Current:** Uses `with_governance()` hook.

**Migration:**
- `submitProposal`: read proposal count from slot 0, increment, write proposal metadata.
- `vote`: `sstore` individual vote, update tally.
- `queue`: update proposal status slot.
- `execute`: read proposal metadata, perform action (e.g., update validator params via `sstore` on `VALIDATOR_ADDRESS`).

### 4.6 Compliance precompile (`0x205`)

**Current:** Uses `with_compliance()` and `with_registry()` hooks.

**Migration:**
- `updateCompliance`: write policy rules hash.
- `checkCompliance`: `sload` address status for policy.

### 4.7 Shielded precompile (`0x202`)

**Current:** Uses `with_shielded_state()` hook.

**Migration:**
- `deposit`: verify proof, `sload` commitment root, compute new root, `sstore` new root, `sstore` nullifier check.
- `withdraw`: verify proof, `sstore` nullifier insertion, credit balance via `ASSET_ADDRESS` storage.
- `transfer`: verify proof, update commitment/nullifier state.

**Note:** Shielded state is complex. If Merkle tree updates are too expensive in EVM storage, consider storing only the tree root and a batch counter in EVM storage, with the full tree in a sidecar indexed by root. Precompiles validate proofs against the root.

### 4.8 Agent precompile (`0x209`)

**Current:** Fallback stub in `call-precompiles`, real impl registered from `call-agent` via `OnceLock`.

**Migration:**
- Merge agent logic into `crates/precompiles/src/agent.rs`.
- `register`: write agent info slots.
- `grant` / `revoke`: update permission bits in agent info slot.
- Balance operations read/write `AGENT_ADDRESS` storage slots.

### 4.9 Switch precompile (`0x207`)

**Keep disabled as-is.** The migration to EVM-only state does not change the switch precompile's status. Once the atomic dual-write design is ready, switch will use EVM storage for both protocol and EVM-side accounting.

---

## Phase 5: Consensus / BFT Layer Changes

**Files:** `crates/consensus/src/validator.rs`, `crates/consensus/src/block.rs`, `crates/consensus/src/simplex.rs` (or equivalent).

### 5.1 Validator set access

Wherever consensus reads the active validator set (for quorum, proposer selection, BFT message validation):

**Before:** `validator_state.get_all_validators()` from `ValidatorStateManager`

**After:** Read from `EvmState` storage at `VALIDATOR_ADDRESS`:

```rust
fn get_active_validators(evm_state: &EvmState) -> Vec<(Address, u128)> {
    let count = evm_state.get_storage(VALIDATOR_ADDRESS, U256::ZERO);
    let base = storage_slot(&[b"validators"]);
    let mut validators = Vec::new();
    for i in 0..count.to::<u64>() {
        let addr_slot = base + U256::from(i);
        let addr_bytes = evm_state.get_storage(VALIDATOR_ADDRESS, addr_slot);
        let addr = Address::from_slice(&addr_bytes.to_be_bytes::<32>()[12..]);
        let stake_slot = storage_slot(&[addr.as_slice(), b"stake"]);
        let stake = evm_state.get_storage(VALIDATOR_ADDRESS, stake_slot);
        validators.push((addr, stake.to::<u128>()));
    }
    validators
}
```

### 5.2 Stake/unstake in consensus

Validator churn (epoch boundaries) currently updates `ValidatorStateManager`. After migration, epoch transitions become EVM transactions that call the validator precompile to process the unbonding queue and update the active validator bitmap.

### 5.3 Remove `Subsystems` from `Block::execute`

Currently `Block::execute` takes `subsystems: &mut Subsystems` which holds `Option<&mut OracleManager>`, `Option<&mut GovernanceManager>`, `Option<&mut ValidatorStateManager>`, etc. Since all state is in `EvmState`, `Subsystems` can be greatly simplified or removed. Only non-state parameters (like `bridge_config`, `fee_params`, `consensus_params`) are still needed.

---

## Phase 6: RpcState & RPC Handlers

**Files:** `crates/rpc/src/handlers/state.rs`, all handler files.

### 6.1 Simplify RpcState

**Before:** 30+ fields including `balance_state`, `asset_registry`, `compliance_engine`, `bridge_state`, `validator_state`, `agent_registry`, `agent_balances`, `shielded_state`, `governance`, `oracle`.

**After:**
```rust
pub struct RpcState {
    pub evm_state: RwLock<EvmState>,   // ← single source of truth
    pub receipts: RwLock<HashMap<TxHash, ProtocolReceipt>>,
    pub current_block: RwLock<u64>,
    pub fee_params: RwLock<FeeParams>,
    pub consensus_params: RwLock<ConsensusParams>,
    pub mempool: Arc<RwLock<Mempool>>,
    pub mempool_defense: RwLock<MempoolDefense>,
    pub chain_id: u64,
    pub subscriptions: SubscriptionManager,
    pub fork_manager: RwLock<ForkManager>,
    pub fee_currency_registry: RwLock<FeeCurrencyRegistry>,
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

### 6.2 Update RPC helper methods

`get_balance(asset_id, address)`:
**Before:** `self.balance_state.read().map(|s| s.get_balance(...))`
**After:** Read from `evm_state` at `ASSET_ADDRESS` storage slot.

`get_nonce(address)`:
**Before:** `self.balance_state.read().map(|s| s.get_nonce(address))`
**After:** `self.evm_state.read().map(|s| s.get_nonce(address))` (EvmAccount already has nonce).

`get_asset_info(asset_id)`:
**Before:** `self.asset_registry.read().map(|r| r.get_asset(...))`
**After:** Read metadata slots from `ASSET_ADDRESS` storage.

`get_validator_set()`:
**Before:** `self.validator_state.read().map(|s| s.get_all_validators())`
**After:** Read validator list from `VALIDATOR_ADDRESS` storage.

Repeat for all RPC handlers that touch protocol state.

### 6.3 Update `RpcState::new`

Remove all protocol-state arguments. Only take `evm_state: EvmState` plus the existing non-state params.

---

## Phase 7: Persistence Layer

**Files:** `crates/node/src/state_persist.rs`, `crates/storage/src/lib.rs` (or equivalent).

### 7.1 Simplify LoadedState

```rust
pub(crate) struct LoadedState {
    pub evm_state: EvmState,
    pub validators: ValidatorStateManager, // ← remove once consensus reads from EvmState
}
```

Eventually `LoadedState` is just `EvmState`.

### 7.2 Remove separate protocol-state persistence

In `load_state_from_db`:
- Remove loading of balances, allowances, bridge state, shielded state, governance, compliance, asset registry, fee params, fee currency registry as separate operations.
- Keep loading `EvmState` from `CallEvmAccounts`.
- For the transition period, you can keep loading validators from `CallValidators` until Phase 5 is complete.

In `save_state_to_db`:
- Remove saving of all protocol state except `EvmState`.
- `EvmState` is already persisted via `save_evm_accounts_inner`.

### 7.3 Remove or deprecate storage tables

The following reth-db tables can be removed once migration is complete:
- `CallOracleState`
- `CallBridgeOps`
- `CallShieldedNullifiers`
- `CallShieldedCommitments`
- `CallGovernanceState`
- `CallComplianceState`
- `CallProtocolAssets`
- `CallFeeParams`
- `CallFeeCurrencyRegistry`
- `CallAgentBalances`
- `CallAgentNonces`
- `CallConsensusState` (or keep for BFT meta, not validator stakes)
- `CallValidatorMeta` (validator stakes move to EVM)

**Keep:**
- `CallEvmAccounts` — the single state table
- `CallReceipts` / `CallReceiptsByBlock` — for RPC indexing
- `CallValidators` — only during transition, remove after Phase 5
- `CallForkState` / `CallCheckpoint` — consensus metadata

---

## Phase 8: Genesis Initialization

**Files:** `crates/chainspec/src/genesis.rs`.

### 8.1 Genesis becomes EVM account initialization

**Before:** Genesis creates `AccountState`, `AssetRegistry`, `ComplianceEngine`, `EvmState`, `BridgeStateManager`, `ValidatorStateManager`, etc. separately.

**After:** Genesis only creates `EvmState` with pre-initialized accounts:

```rust
let mut evm_state = EvmState::new();

// Precompile addresses exist with empty code but may have storage
for addr in all_precompiles() {
    evm_state.create_account(*addr); // ensures account exists
}

// Seed initial balances
for (asset_id, address, balance) in genesis.balances {
    let slot = storage_slot(&[&asset_id.to_be_bytes(), address.as_slice()]);
    evm_state.set_storage(ASSET_ADDRESS, slot, U256::from(balance));
}

// Seed initial validators
for (i, (validator_addr, stake)) in genesis.validators.iter().enumerate() {
    let addr_slot = U256::from(i); // simplified
    evm_state.set_storage(VALIDATOR_ADDRESS, addr_slot, U256::from_be_slice(validator_addr.as_slice()));
    evm_state.set_storage(VALIDATOR_ADDRESS, storage_slot(&[validator_addr.as_slice(), b"stake"]), U256::from(stake));
}

// Seed initial assets
for asset in genesis.assets {
    // write asset metadata slots...
}
```

### 8.2 Remove genesis protocol-state setup

Delete all code that initializes `AccountState`, `AssetRegistry`, `ComplianceEngine`, `BridgeStateManager`, `ValidatorStateManager`, `GovernanceManager`, `OracleManager`, `ShieldedState`, `AgentRegistry`, `AgentBalances` from genesis config.

---

## Phase 9: Testing & Verification

### 9.1 Unit tests for each migrated precompile

For each precompile, write tests that:
1. Create an `EvmState` with seeded storage slots
2. Create an `EvmStorageProvider` backed by a mock journal (or use revm's `InMemoryDB` + journal)
3. Call `StorageCtx::enter` and invoke the precompile
4. Verify storage slots after the call
5. Verify gas usage

Example test pattern:
```rust
#[test]
fn test_asset_transfer() {
    let mut db = InMemoryDB::new();
    let mut journal = Journal::new(SpecId::CANCUN, &mut db);

    // Seed initial balance
    journal.sstore(ASSET_ADDRESS, storage_slot(&[1u64.to_be_bytes(), ALICE.as_slice()]), U256::from(1000));

    let mut provider = EvmStorageProvider::new(&mut journal, 1_000_000, false);

    let input = encode_transfer(1, BOB, 500); // asset_id=1, to=BOB, amount=500
    let result = StorageCtx::enter(&mut provider, || {
        AssetPrecompile.call(&input, ALICE)
    });

    assert!(result.is_ok());
    assert_eq!(provider.gas_used(), /* expected */);

    // Verify balances
    let alice_bal = journal.sload(ASSET_ADDRESS, storage_slot(&[1u64.to_be_bytes(), ALICE.as_slice()])).unwrap().data;
    let bob_bal = journal.sload(ASSET_ADDRESS, storage_slot(&[1u64.to_be_bytes(), BOB.as_slice()])).unwrap().data;
    assert_eq!(alice_bal, U256::from(500));
    assert_eq!(bob_bal, U256::from(500));
}
```

### 9.2 State root determinism test

Execute the same block on two independent nodes (different order of tx insertion, different peer sets). Verify both produce identical `state_root`.

### 9.3 Revert test

EVM tx that calls a precompile, then reverts (e.g., out of gas in a nested call). Verify precompile storage changes are reverted by revm.

### 9.4 Genesis hash stability

Compute genesis block hash before and after migration. If the genesis state is equivalent, the hash must be identical (or document the change).

### 9.5 RPC parity test

For every RPC endpoint that reads protocol state, verify it returns the same values as before migration for a given EVM state.

### 9.6 Build verification

```bash
cargo check --workspace --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-features
```

---

## Phase 10: Cleanup & Deprecation

### 10.1 Remove dead code

After all phases are verified:
- Delete `call_protocol::AccountState` (or keep only if used by non-migrated code)
- Delete `call_protocol::AssetRegistry`
- Delete `call_protocol::ComplianceEngine`
- Delete `call_protocol::FeeCurrencyRegistry` (or merge into EVM storage)
- Delete `call_bridge::BridgeStateManager` (or reduce to thin wrapper)
- Delete `call_consensus::ValidatorStateManager` (or reduce to EVM reader)
- Delete `call_agent::AgentRegistry`, `AgentBalances`
- Delete `call_oracle::OracleManager` (or reduce to EVM reader)
- Delete `call_governance::GovernanceManager`
- Delete `call_shielded::ShieldedState` (or reduce to EVM reader)

### 10.2 Remove protocol-state persistence functions

Delete `db_save_balances`, `db_load_balances`, and all other protocol-state save/load helpers from `call-storage`.

### 10.3 Update documentation

- `docs/spec.md` / `docs/spec_cn.md`: Update BlockHeader section to show single `state_root`.
- `docs/precompiles.md`: Document the storage-slot layout for each precompile.
- `docs/review-copilot.md`: Mark blockers #2 (validator restore), #4 (switch), and related items as resolved by architecture migration.

---

## Migration Order (Recommended)

Execute phases in this order to minimize breakage:

1. **Phase 1** — Design storage layouts. No code changes. Write slot layout specs.
2. **Phase 2** — BlockHeader simplification. This is a mechanical change. Update consensus tests.
3. **Phase 3** — Build the stateful precompile infrastructure (`StorageProvider`, `EvmStorageProvider`, `StorageCtx`, `StatefulPrecompile`, rewrite `CallPrecompiles::run`). Do NOT migrate any precompile logic yet — keep the old stubs returning errors.
4. **Phase 4a** — Migrate one precompile end-to-end: **Asset precompile (`0x201`)**. This is the most-used precompile and validates the pattern.
5. **Phase 4b** — Migrate Oracle precompile (`0x101`). Relatively simple, high impact.
6. **Phase 4c** — Migrate Validator precompile (`0x204`). Requires consensus-layer changes in Phase 5.
7. **Phase 5** — Update consensus to read validators from EVM storage. Run BFT tests.
8. **Phase 4d** — Migrate remaining precompiles (Bridge, Governance, Compliance, Shielded, Agent).
9. **Phase 6** — Simplify `RpcState` and update all RPC handlers.
10. **Phase 7** — Simplify persistence. Keep old tables as no-ops during transition.
11. **Phase 8** — Update genesis.
12. **Phase 9** — Comprehensive testing.
13. **Phase 10** — Cleanup.

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
