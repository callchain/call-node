# Oracle System Improvement Plan

## Current State

The oracle system (`crates/protocol/src/oracle.rs`) implements validator-submitted price feeds with quorum-based aggregation, outlier detection, and TWAP. It was wired into the boot process in commit `d500736` so genesis validators are registered and assets are tracked.

### What Works

- `OracleManager` with quorum aggregation (median), outlier detection, TWAP history
- RPC endpoint `call_oracleSubmitPrice` accepting Ed25519-signed submissions
- Genesis validators registered into oracle during boot (both `chainspec::GenesisExecutor` and `boot::Genesis`)
- `oracle_assets` field in genesis config to specify tracked assets
- EVM oracle precompile stub at `0x101` with `getPrice` selector implemented
- Structurally complete: signature verification, dedup, period gating, staleness checks

### Critical Problems

#### 1. In-memory only, no on-chain record

Prices live only in `RpcState` (an `RwLock<OracleManager>`). A node restart loses all oracle data. There's no way to audit historical submissions, verify consensus was reached, or replay prices after a crash. The oracle state is not persisted to the database and not included in blocks.

**Impact**: Oracle is non-deterministic across nodes. Two nodes receiving submissions in different orders could have different aggregated prices.

#### 2. No transaction inclusion — not consensus-validated

Oracle submissions bypass the mempool, block production, and consensus. They are direct RPC writes to in-memory state. This means:

- Validators cannot verify the oracle state from block data
- Light clients cannot verify oracle prices
- There's no cryptographic link between oracle prices and the chain's consensus state

#### 3. Quorum of 14 is impossible for small validator sets

`ORACLE_QUORUM = 14` is hardcoded (`oracle.rs:19`). The devnet has 4 validators. For any validator set < 14, the oracle will **never** aggregate prices.

#### 4. No economic incentive or penalty

- Validators are not rewarded for submitting correct prices
- The outlier detection increments `outlier_count` and disables after 10 strikes, but there's no stake slashing
- There's no fee reward for participation
- Validators have zero economic reason to report prices

#### 5. Oracle precompile returns stale data

`crates/precompiles/src/oracle.rs:78` — the precompile creates `OracleState::default()` which instantiates a fresh `OracleManager::new(OracleConfig::default())`. This empty oracle has zero prices, zero validators, zero tracked assets. The precompile always returns zeros/None because it's disconnected from the live oracle in `RpcState`.

#### 6. Validators self-report with no data source attestation

The submission only contains `(validator_id, asset_id, price, signature)`. There's no field indicating which data source the price came from.

---

## Phase 2: Configurable Quorum (do this first — 1 file, 3 lines)

**Why first**: It's the smallest change that immediately makes the devnet oracle functional.

### File: `crates/protocol/src/oracle.rs`

#### Change 1: Remove hardcoded `ORACLE_QUORUM`

Delete the constant at line 19:
```rust
- pub const ORACLE_QUORUM: usize = 14;
```

#### Change 2: Add dynamic quorum helper

After the constants block, add:
```rust
/// Compute the oracle quorum from the number of active validators.
/// Returns ceil(2/3 * n), minimum 2, capped at n.
pub fn oracle_quorum(active_count: usize) -> usize {
    if active_count <= 3 { return active_count.min(2); }
    ((2 * active_count + 2) / 3).min(active_count)
}
```

#### Change 3: Use dynamic quorum in `submit_price`

In `submit_price()` around line 199, replace:
```rust
- if submissions.len() >= self.config.quorum {
+ if submissions.len() >= oracle_quorum(self.validators.len()) {
```

Also remove `quorum` from `OracleConfig` and `Default` impl since it's no longer needed.

### Tests

Add to `crates/protocol/src/oracle.rs` tests:
```rust
#[test]
fn test_oracle_quorum_small() {
    assert_eq!(oracle_quorum(2), 2);  // devnet-style
    assert_eq!(oracle_quorum(3), 2);
    assert_eq!(oracle_quorum(4), 3);  // ceil(2/3*4) = 3
    assert_eq!(oracle_quorum(6), 4);  // ceil(2/3*6) = 4
    assert_eq!(oracle_quorum(21), 14); // production subset
    assert_eq!(oracle_quorum(216), 144);
}
```

---

## Phase 1: Deterministic On-Chain Oracle

**Goal**: Oracle submissions are protocol transactions included in blocks. All nodes compute the same oracle state from the same block data.

### 1.1 Add `OracleSubmit` to `Instruction` enum

**File**: `crates/protocol/src/instructions.rs`

Add a new variant to the `Instruction` enum (after `ShieldedDeposit`, around line 97):
```rust
OracleSubmit {
    asset_id: AssetId,
    price: u128,
    block_number: u64,
    timestamp: u64,
    signature: [u8; 64],
},
```

### 1.2 Execute `OracleSubmit` in the instruction dispatcher

**File**: `crates/protocol/src/instructions.rs`

The `execute_protocol_instructions` function at line 171 takes `(balances, registry, compliance, shielded_state, sender)`. It does NOT have access to the `OracleManager`.

Two approaches:

**Approach A (simpler)**: Pass an optional `&mut Option<OracleManager>` to `execute_protocol_instructions` and `execute_instruction`. The block executor provides it; non-oracle code passes `None`.

**Approach B (cleaner)**: Create a separate `OracleExecutor` struct with a `submit_price_from_instruction()` method that takes the same validation inputs but without the RPC layer.

Use Approach A — fewer types, minimal structural change.

Modify the function signature:
```rust
pub fn execute_protocol_instructions(
    instructions: &[Instruction],
    balances: &mut BalanceState,
    registry: &AssetRegistry,
    compliance: &mut ComplianceEngine,
    shielded_state: &mut ShieldedState,
    sender: Address,
    oracle: Option<&mut OracleManager>,   // NEW
) -> ProtocolResult<Vec<InstructionResult>>
```

Pass `oracle` through to `execute_instruction`, and in the match, add:
```rust
Instruction::OracleSubmit { asset_id, price, block_number, timestamp, signature } => {
    let oracle = oracle.ok_or(ProtocolError::InvalidInstruction(
        "oracle not available".into(),
    ))?;
    let submission = OracleSubmission {
        validator_id: sender_to_validator_id(sender)?,  // derive from sender address
        asset_id: *asset_id,
        price: *price,
        block_number: *block_number,
        timestamp: *timestamp,
        signature: *signature,
    };
    oracle.submit_price(submission)
        .map_err(|e| ProtocolError::InvalidInstruction(format!("oracle: {e}")))?;
    Ok(InstructionResult::Success)
}
```

### 1.3 Wire oracle through block execution

**File**: `crates/consensus/src/block.rs`

The `Block::execute` method (line 219) receives `(balances, registry, compliance, bridge_state, shielded_state, fee_params, current_block_height, evm_state)`.

Add an `oracle` parameter:
```rust
pub fn execute(
    &self,
    balances: &mut BalanceState,
    registry: &AssetRegistry,
    compliance: &mut call_protocol::compliance::ComplianceEngine,
    bridge_state: &mut call_bridge::BridgeStateManager,
    shielded_state: &mut ShieldedState,
    fee_params: &mut FeeParams,
    current_block_height: u64,
    evm_state: &mut EvmState,
    oracle: Option<&mut call_protocol::oracle::OracleManager>,  // NEW
) -> Result<BlockExecutionResult, ConsensusError>
```

In the protocol transaction execution section (around line 274), pass `oracle` to `execute_protocol_instructions`:
```rust
let tx_results = execute_protocol_instructions(
    &tx.instructions,
    balances,
    registry,
    compliance,
    shielded_state,
    tx.sender,
    oracle.as_deref_mut(),  // NEW
)
```

### 1.4 Update all callers of `Block::execute`

**File**: `crates/node/src/lib.rs`

Two callers:
1. `block_production_loop` (line 848) — pass `Some(&mut *state.oracle.write().unwrap())`
2. `start_sync` block execution (line 316) — pass `None` (sync doesn't need oracle since prices are already in block state)

Update the call in `block_production_loop`:
```rust
let oracle_guard = state.oracle.write().ok();
// ... pass oracle_guard.as_deref_mut() to block.execute()
```

### 1.5 Change `call_oracleSubmitPrice` to build + broadcast a transaction

**File**: `crates/rpc/src/callchain.rs`

Currently (line 745), `call_oracleSubmitPrice` writes directly to `state.oracle`. Change it to build a protocol transaction containing an `OracleSubmit` instruction and insert it into the mempool:

```rust
let instr = Instruction::OracleSubmit {
    asset_id,
    price,
    block_number,
    timestamp,
    signature,
};
let tx = ProtocolTransaction {
    sender: derive_sender_from_sig(&signature)?,  // or require caller to provide
    nonce: get_next_nonce(&state)?,
    instructions: vec![instr],
    gas_config: GasConfig::SelfPay,
    fee_currency: FeeCurrency::Call,
    gas_limit: 50_000,
    max_fee: 1_000_000,
    auth: AuthScheme::SingleSig { signature },
};
let mut mempool = state.mempool.write().map_err(|_| internal_error("lock poisoned".into()))?;
let _ = mempool.insert_protocol_tx(tx);
```

### 1.6 Persist oracle aggregated prices to database

**File**: `crates/storage/src/reth_db.rs`

Add a new table after `CallAgents` (around line 110):
```rust
/// Oracle aggregated prices: serialized asset_id -> serialized AggregatedPriceEntry
#[derive(Debug)]
pub struct CallOraclePrices;
impl Table for CallOraclePrices {
    const NAME: &'static str = "call_oracle_prices";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}
```

Add `CallOraclePrices` to the `CallTables` table set.

Add save/load functions:
```rust
pub fn save_oracle_prices(db_env: &Arc<DatabaseEnv>, prices: &HashMap<AssetId, AggregatedPrice>) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = prices
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    db_clear::<CallOraclePrices>(db_env).map_err(|e| e.to_string())?;
    db_batch_put::<CallOraclePrices>(db_env, entries).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_oracle_prices(db_env: &Arc<DatabaseEnv>) -> Result<HashMap<AssetId, AggregatedPrice>, String> {
    let data = db_iter_all::<CallOraclePrices>(db_env).map_err(|e| e.to_string())?;
    let mut prices = HashMap::new();
    for (k, v) in data {
        let asset_id: AssetId = serde_json::from_slice(&k).map_err(|e| e.to_string())?;
        let price: AggregatedPrice = serde_json::from_slice(&v).map_err(|e| e.to_string())?;
        prices.insert(asset_id, price);
    }
    Ok(prices)
}
```

### 1.7 Add oracle persistence to the node

**File**: `crates/node/src/lib.rs`

In `load_state_from_db` (line 412), add oracle price loading:
```rust
let oracle_prices = match load_oracle_prices(db_env) {
    Ok(p) => p,
    Err(e) => {
        tracing::warn!(error = %e, "failed to load oracle prices");
        HashMap::new()
    }
};
```

In `persist_state_incremental` (line 689), add oracle price persistence after each block:
```rust
{
    let oracle = state.oracle.read().map_err(|_| "oracle lock poisoned".to_string())?;
    // Persist aggregated prices
    // ... build HashMap from oracle.aggregated (needs getter method)
}
```

Add a getter to `OracleManager`:
```rust
pub fn get_all_aggregated(&self) -> &HashMap<AssetId, AggregatedPrice> {
    &self.aggregated
}
```

### Tests

**File**: `crates/protocol/src/instructions.rs` tests

Add a test:
```rust
#[test]
fn test_oracle_submit_instruction() {
    let mut balances = BalanceState::new();
    let mut manager = OracleManager::new(OracleConfig::default());
    let (pubkey, signing_key) = ed25519_generate_keypair();
    manager.register_validator(0, pubkey);

    let registry = AssetRegistry::new();
    let mut compliance = ComplianceEngine::new();
    let mut shielded_state = ShieldedState::new();

    // Build a signed submission
    let sig = sign_oracle_submission(&signing_key, 0, 1, 2_000_000, 1000, 1_000_000);
    let instructions = vec![Instruction::OracleSubmit {
        asset_id: 1,
        price: 2_000_000,
        block_number: 1000,
        timestamp: 1_000_000,
        signature: sig,
    }];

    let results = execute_protocol_instructions(
        &instructions, &mut balances, &registry, &mut compliance,
        &mut shielded_state, test_addr(1), Some(&mut manager),
    ).expect("execute");

    assert_eq!(results.len(), 1);
    assert!(manager.get_price(1).is_some());
}
```

---

## Phase 4: EVM Oracle Precompile

**Goal**: Smart contracts can read oracle prices from within EVM execution.

The precompile stub at `crates/precompiles/src/oracle.rs` already has the logic. The problem is it creates a fresh empty `OracleManager` instead of reading from the live state.

### 4.1 Fix the precompile to read live state

**File**: `crates/precompiles/src/lib.rs`

The `oracle_precompile_fn` (line 67) creates `OracleState::default()` — a fresh empty oracle. This must be replaced with a reference to the live `RpcState.oracle`.

The challenge: REVM precompile functions are pure `fn(input, gas_limit) -> Result`. They cannot access external state by default.

**Solution**: Use REVM's `Env` custom handler or pass oracle state through a thread-local/global. The simplest approach: store a `Arc<RwLock<Option<OracleManager>>>` in a static `AtomicPtr` or use a `once_cell::sync::Lazy` that gets set during node boot.

Add to `crates/precompiles/src/oracle.rs`:
```rust
use std::sync::RwLock;

static LIVE_ORACLE: std::sync::OnceLock<Arc<RwLock<OracleManager>>> = std::sync::OnceLock::new();

pub fn set_live_oracle(oracle: Arc<RwLock<OracleManager>>) {
    let _ = LIVE_ORACLE.set(oracle);
}

fn get_live_oracle() -> Option<Arc<RwLock<OracleManager>>> {
    LIVE_ORACLE.get().cloned()
}
```

### 4.2 Update precompile to read from live oracle

In `oracle_precompile_fn` (line 67), replace:
```rust
- let _state = OracleState::default();
+ let Some(oracle_guard) = get_live_oracle() else {
+     return Err(PrecompileError::Other("oracle not initialized".into()));
+ };
+ let oracle = oracle_guard.read().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;
```

Then use `oracle.get_price(asset_id)` instead of `_state.get_price(asset_id)`.

### 4.3 Wire up additional selectors

The precompile currently only implements `getPrice` (selector `0x763e4d8c`). Add:

```rust
// getTWAP(uint64 assetId, uint64 currentTimestamp) -> uint128
&[0xab, 0xcd, 0xef, 0x01] => {
    let asset_id = read_u64(&input, 28);
    let current_ts = read_u64(&input, 60);
    if let Some(twap) = oracle.get_twap(asset_id, current_ts) {
        output[16..].copy_from_slice(&twap.to_be_bytes());
    }
}

// isStale(uint64 assetId, uint64 currentTimestamp) -> bool
&[0x12, 0x34, 0x56, 0x78] => {
    let asset_id = read_u64(&input, 28);
    let current_ts = read_u64(&input, 60);
    let stale = oracle.is_stale(asset_id, current_ts);
    output[31] = if stale { 1 } else { 0 };
}
```

### 4.4 Initialize live oracle on boot

**File**: `crates/node/src/lib.rs`

After `RpcState` is created in `CallNode::new()`, call:
```rust
call_precompiles::set_live_oracle(Arc::clone(&state.oracle));
```

### Tests

**File**: `crates/precompiles/src/oracle.rs`

Update existing tests to not depend on the live oracle, and add a new integration test:
```rust
#[test]
fn test_live_oracle_precompile() {
    // Set up a live oracle with a registered validator and submitted price
    let manager = OracleManager::new(OracleConfig::default());
    // ... register validator, submit price
    let shared = Arc::new(RwLock::new(manager));
    set_live_oracle(shared);

    // Call precompile
    let mut input = vec![0u8; 36];
    input[0..4].copy_from_slice(&[0x76, 0x3e, 0x4d, 0x8c]); // getPrice
    input[28..36].copy_from_slice(&1u64.to_be_bytes()); // asset_id = 1

    let result = oracle_precompile_fn(&input, 10000).unwrap();
    // Should return the aggregated price
}
```

---

## Phase 3: Economic Incentives

**Goal**: Reward correct submissions, penalize bad behavior.

### 3.1 Add oracle reward pool to `OracleManager`

**File**: `crates/protocol/src/oracle.rs`

Add to `OracleManager` struct:
```rust
/// Accumulated fee pool for oracle rewards (reset each period)
pub reward_pool: u128,
/// Validators who contributed to the current quorum
pub current_contributors: Vec<u32>,
```

### 3.2 Distribute rewards at quorum

In `aggregate_and_publish_price` (around line 207), after successful aggregation:

```rust
// Distribute rewards to contributors
if self.reward_pool > 0 {
    let per_validator = self.reward_pool / submissions.len() as u128;
    for vid in submissions.keys() {
        // Reward tracked externally — oracle doesn't modify balances directly
        // Instead, record the reward event for the consensus layer to process
        self.current_contributors.push(*vid);
    }
}
```

### 3.3 Slash oracle outliers

**File**: `crates/consensus/src/validator.rs`

Add a new slashing method after `slash_offline`:
```rust
/// Slash for submitting oracle price outliers
pub fn slash_oracle_outlier(
    &mut self,
    validator_id: ValidatorId,
) -> Result<u128, ConsensusError> {
    let validator = self
        .validators
        .get_mut(&validator_id)
        .ok_or(ConsensusError::ValidatorNotFound(validator_id))?;

    // Slash 0.1% of self_stake per outlier event
    let slashed = (validator.self_stake * 10) / 10_000; // 0.1%
    validator.slash_history.push(SlashEvent {
        reason: "oracle outlier".into(),
        amount_slashed: slashed,
        block: self.current_block,
    });
    validator.self_stake = validator.self_stake.saturating_sub(slashed);
    validator.staked_call = validator.staked_call.saturating_sub(slashed);
    Ok(slashed)
}
```

### 3.4 Hook slashing into oracle outlier detection

**File**: `crates/protocol/src/oracle.rs`

In `aggregate_and_publish_price`, when an outlier is detected, call the validator's slashing function. This requires a callback or a post-processing step in block execution.

Add to `OracleManager`:
```rust
/// Returns the list of validator IDs that were outliers in the last aggregation
pub fn last_outliers(&self) -> Vec<u32> {
    // track during aggregate_and_publish_price
}
```

Then in block execution, after processing oracle instructions:
```rust
let outliers = oracle.last_outliers();
for vid in outliers {
    let _ = consensus.slash_oracle_outlier(vid);
}
```

### 3.5 Oracle fee share in block fees

**File**: `crates/protocol/src/transaction.rs`

Add to `FeeParams`:
```rust
/// Percentage of block fees allocated to oracle rewards (basis points, default 100 = 1%)
pub oracle_fee_share_bps: u16,
```

In block execution, after fee collection:
```rust
let oracle_reward = total_fees * fee_params.oracle_fee_share_bps as u128 / 10_000;
oracle.reward_pool += oracle_reward;
```

### Tests

Add to `crates/consensus/src/validator.rs`:
```rust
#[test]
fn test_slash_oracle_outlier() {
    let mut manager = ValidatorStateManager::new();
    manager.stake(test_addr(1), test_pubkey(1), 100_000).unwrap();
    let slashed = manager.slash_oracle_outlier(0).unwrap();
    assert_eq!(slashed, 100); // 0.1% of 100,000
}
```

---

## Phase 5: Data Source Attestation (Future)

Lower priority — add after Phases 1-4. Extend `OracleSubmit` instruction with optional `sources: Vec<String>` field. Validators report which APIs they queried. Genesis can include an `oracle_approved_sources: Vec<String>` allowlist.

---

## Implementation Order

| Phase | Files | Complexity | Priority |
|-------|-------|-----------|----------|
| Phase 2: Configurable Quorum | `oracle.rs` (1 file) | ~5 lines | **Highest** — unblocks devnet |
| Phase 1: Deterministic On-Chain | `instructions.rs`, `block.rs`, `lib.rs`, `callchain.rs`, `reth_db.rs`, `oracle.rs` (~6 files) | Medium — wire oracle through existing execution path | **High** — core architectural fix |
| Phase 4: EVM Precompile | `oracle.rs` (precompiles), `lib.rs` (precompiles), `lib.rs` (node) (~3 files) | Medium — live state wiring | **High** — enables DeFi contracts |
| Phase 3: Economic Incentives | `oracle.rs`, `validator.rs`, `transaction.rs`, `block.rs` (~4 files) | Medium — adds fee distribution + slashing | Medium |
| Phase 5: Data Source Attestation | TBD | Low | Low |
