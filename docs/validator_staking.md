# Validator Staking & Unbonding Parameters

> This document defines the quantitative parameters for validator stake/unstake churn control, unbonding period, and validator-set stability in Callchain.
>
> Last updated: 2026-04-27

## Table of Contents

- [1. Overview](#1-overview)
- [2. Parameter Summary](#2-parameter-summary)
- [3. Parameter Storage Architecture](#3-parameter-storage-architecture)
- [4. Core Mechanisms](#4-core-mechanisms)
  - [4.1 Churn Limit](#41-churn-limit)
  - [4.2 Safety Floor](#42-safety-floor)
  - [4.3 Dynamic Unbonding Period](#43-dynamic-unbonding-period)
  - [4.4 Two-Phase Exit](#44-two-phase-exit)
  - [4.5 Delegation Exit Linkage](#45-delegation-exit-linkage)
- [5. Code Integration Points](#5-code-integration-points)
- [6. Governance Integration](#6-governance-integration)
- [7. Governance Roadmap](#7-governance-roadmap)
- [8. Reference: Chain Comparison](#8-reference-chain-comparison)

---

## 1. Overview

Callchain uses a **DPoS-like model with VRF-based subset rotation** (per `crates/consensus/src/proposer.rs` and `simplex.rs`).
Each epoch, a subset of 21 validators is randomly selected from the qualified validator pool to run BFT consensus.

This design creates a unique stability risk: if a large fraction of validators unstake simultaneously, the pool of
qualified validators may drop below the subset size, breaking the VRF selection and stalling consensus.

To mitigate this, Callchain implements **four protective layers**:

1. **Churn Limit** — rate-limit how many validators can enter/exit per epoch.
2. **Safety Floor** — prevent unstake if it would drop the qualified pool below a safe threshold.
3. **Dynamic Unbonding Period** — automatically extend the lock-up when the exit queue is long.
4. **Two-Phase Exit** — validators remain accountable during the current epoch even after initiating unstake.

---

## 2. Parameter Summary

### 2.1 Environment-Specific Defaults

| Parameter | Symbol | Devnet | Testnet | Mainnet (Genesis) | Unit |
|---|---|---|---|---|---|
| Block time | `BLOCK_TIME_MS` | 250 | 250 | 250 | ms |
| Epoch length | `EPOCH_LENGTH` | 100 | 100 | **2,880** | blocks |
| Subset size | `SUBSET_SIZE` | 21 | 21 | 21 | validators |
| Minimum self-stake | `MIN_SELF_STAKE` | 1,000,000 | 1,000,000 | 1,000,000 | CALL (18 dec) |
| **Unbonding period** | `UNBONDING_PERIOD` | 1,008 | 20,160 | **120,960** | blocks |
| Churn-limit quotient | `CHURN_LIMIT_QUOTIENT` | 8 | 8 | **16** | — |
| Minimum churn limit | `MIN_CHURN_LIMIT` | 2 | 2 | 2 | validators/epoch |
| Safety ratio | `SAFETY_RATIO` | 4/3 | 4/3 | 4/3 | — |
| Offline slash rate | `OFFLINE_SLASH_RATE_BPS` | 10 | 10 | 10 | basis points |
| Slash extend factor | `UNBONDING_SLASH_EXTEND` | 2× | 2× | 2× | multiplier |
| Max unbonding multiplier | `MAX_UNBONDING_MULTIPLIER` | 10× | 10× | 10× | multiplier |

### 2.2 Derived Values

| Derived Value | Formula | Mainnet (Genesis) |
|---|---|---|
| Epoch duration | `EPOCH_LENGTH × BLOCK_TIME_MS` | **12 minutes** |
| Base unbonding duration | `UNBONDING_PERIOD × BLOCK_TIME_MS` | **~8.4 hours** |
| Safety floor | `ceil(SUBSET_SIZE × SAFETY_RATIO)` | **28 validators** |
| Churn limit | `max(MIN_CHURN_LIMIT, qualified / CHURN_LIMIT_QUOTIENT)` | see table below |
| Max unbonding duration | `UNBONDING_PERIOD × MAX_UNBONDING_MULTIPLIER` | **~84 hours** |

### 2.3 Churn Limit Examples (Mainnet)

| Qualified Validators | Churn Limit (`max(2, q/16)`) | Max Exits / Hour | Max Exits / Day |
|---|---|---|---|
| 32 | 2 | 10 | 240 |
| 50 | 4 | 20 | 480 |
| 100 | 7 | 35 | 840 |
| 200 | 13 | 65 | 1,560 |
| 320 | 20 | 100 | 2,400 |

---

## 3. Parameter Storage Architecture

All parameters fall into one of three storage tiers. The goal is to migrate everything **out of compile-time constants** and into either genesis configuration or on-chain governable state.

### 3.1 Tier 1: Genesis Configuration (Already Landed)

These parameters are already part of `ConsensusParams` in `crates/consensus/src/proposer.rs` and are loaded from `genesis.json` at node startup.

| Parameter | Current Code Location | Storage |
|---|---|---|
| `BLOCK_TIME_MS` | `ConsensusParams.block_time_millis` | `genesis.json` → memory |
| `EPOCH_LENGTH` | `ConsensusParams.epoch_length` | `genesis.json` → memory |
| `SUBSET_SIZE` | `ConsensusParams.subset_size` | `genesis.json` → memory |
| `MAX_VALIDATORS` | `ConsensusParams.max_validators` | `genesis.json` → memory |

**Lifecycle**: Loaded once at startup. Changes require a governance proposal that writes a new value into the database; nodes pick it up on next restart.

### 3.2 Tier 2: Compile-Time Constants (Must Migrate)

These are currently hard-coded `pub const` values. They should be moved into `ConsensusParams` so they can be configured per network and modified via governance without a code release.

| Parameter | Current Code Location | Target Location |
|---|---|---|
| `MIN_SELF_STAKE` | `crates/consensus/src/validator.rs:12` | `ConsensusParams.min_self_stake` |
| `UNBONDING_PERIOD_SECS` | `crates/consensus/src/validator.rs:15` | `ConsensusParams.unbonding_period_blocks` |
| `OFFLINE_SLASH_RATE_PER_ROUND` | `crates/consensus/src/validator.rs:18` | `ConsensusParams.offline_slash_rate_bps` |
| `KEY_ROTATION_GRACE_BLOCKS` | `crates/consensus/src/validator.rs:21` | `ConsensusParams.key_rotation_grace_blocks` |

**Migration plan**:
1. Add fields to `ConsensusParams` in `proposer.rs`.
2. Update `Genesis::default()` and all `genesis.json` files to include the new fields.
3. Replace `pub const` references in `validator.rs` with reads from the passed-in `ConsensusParams`.
4. Update `ValidatorStateManager` to hold a `ConsensusParams` reference instead of duplicating `min_self_stake` and `offline_slash_rate_bps` as instance fields.

### 3.3 Tier 3: New Parameters (Not Yet Implemented)

These parameters are defined in this document but do not yet exist in code. They should be added directly to `ConsensusParams`.

| Parameter | Target Location | Scope |
|---|---|---|
| `CHURN_LIMIT_QUOTIENT` | `ConsensusParams.churn_limit_quotient` | Global |
| `MIN_CHURN_LIMIT` | `ConsensusParams.min_churn_limit` | Global |
| `SAFETY_RATIO` (num/den) | `ConsensusParams.safety_ratio_num / safety_ratio_den` | Global |
| `UNBONDING_SLASH_EXTEND` | `ConsensusParams.unbonding_slash_extend` | Global |
| `MAX_UNBONDING_MULTIPLIER` | `ConsensusParams.max_unbonding_multiplier` | Global |
| `stake_queue` | `ValidatorStateManager.stake_queue` | Per-node state |
| `exit_queue` | `ValidatorStateManager.exit_queue` | Per-node state |
| `epoch_churn_count` | `ValidatorStateManager.epoch_churn_count` | Per-node state |

**Queue persistence**: `stake_queue`, `exit_queue`, and `epoch_churn_count` are part of validator consensus state. They must be serialized alongside the `validators` HashMap and `unbonding_requests` Vec in `state_persist.rs`. On node restart, they are restored via `SimplexConsensus::restore_from_persisted()`.

### 3.4 Storage Flow Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│  Genesis JSON (devnet/testnet/mainnet)                              │
│  ─────────────────────────────────────                              │
│  consensus_params: {                                                │
│    epoch_length, subset_size, block_time_millis,                   │
│    min_self_stake, unbonding_period_blocks,                        │
│    churn_limit_quotient, safety_ratio_num, ...                     │
│  }                                                                  │
└──────────────────────────────┬──────────────────────────────────────┘
                               │ parse at startup
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│  ConsensusParams (in memory, per-node)                              │
│  ─────────────────────────────────────                              │
│  Loaded from genesis. Governance can overwrite via DB.             │
└──────────────────────────────┬──────────────────────────────────────┘
                               │ pass by reference
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│  ValidatorStateManager (in memory + DB)                             │
│  ─────────────────────────────────────                              │
│  validators: HashMap<ValidatorId, ValidatorStake>                  │
│  unbonding_requests: Vec<UnbondingRequest>                         │
│  stake_queue: Vec<Address>         ← NEW                           │
│  exit_queue: Vec<ValidatorId>      ← NEW                           │
│  epoch_churn_count: u64            ← NEW                           │
└──────────────────────────────┬──────────────────────────────────────┘
                               │ serialize / deserialize
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│  RocksDB (persistent)                                               │
│  ────────────────────                                               │
│  Table: call_consensus_state                                        │
│  Table: call_validator_state  ← extend schema                      │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 4. Core Mechanisms

### 4.1 Churn Limit

**Purpose**: Prevent sudden mass exits from depleting the validator pool faster than the protocol can absorb.

**Rule**: Each epoch, at most `churn_limit` validators may **enter** (stake) and `churn_limit` may **exit** (unstake).
Requests beyond the limit are placed in a FIFO queue and processed in subsequent epochs.

```rust
pub fn churn_limit(qualified_count: u64, params: &ConsensusParams) -> u64 {
    max(params.min_churn_limit, qualified_count / params.churn_limit_quotient)
}
```

**Transaction Response**:
- If within limit: tx is executed, validator enters/exits immediately.
- If queue is full: tx returns `status: "queued"` with `queue_position: N`. The tx hash is reserved; inclusion is guaranteed once the queue advances.

**Queue Persistence**: The stake/exit queues are part of validator state and are persisted to disk alongside block execution results. They are restored on node restart from `state_persist.rs`.

---

### 4.2 Safety Floor

**Purpose**: Guarantee that even under extreme churn, enough qualified validators remain to form a consensus subset.

**Rule**: `unstake` is rejected if the post-operation qualified count would drop below:

```
safety_floor = ceil(SUBSET_SIZE * SAFETY_RATIO)
             = ceil(21 * 4 / 3)
             = 28
```

**Error Response**:
```json
{
  "status": "reverted",
  "revertReason": "unstake rejected: would drop below safety threshold (28)"
}
```

**Rationale**: With 21 validators in the active subset, a floor of 28 guarantees at least 7 spare validators.
This provides a buffer for:
- One full subset rotation (all 21 replaced)
- Simultaneous offline events
- Partial slashing reducing stake below `MIN_SELF_STAKE`

---

### 4.3 Dynamic Unbonding Period

**Purpose**: Discourage coordinated mass exits by making the lock-up duration proportional to exit-queue congestion.

**Rule**:

```rust
fn dynamic_unbonding_period(base: u64, queue_len: u64, churn_limit: u64, params: &ConsensusParams) -> u64 {
    let queue_factor = max(1, ceil_div(queue_len, churn_limit));
    let raw = base * queue_factor;
    min(raw, base * params.max_unbonding_multiplier as u64)
}
```

**Example Table** (Mainnet, base = 120,960 blocks, churn_limit = 4):

| Exit Queue Length | Queue Factor | Actual Unbonding Period | Duration |
|---|---|---|---|
| 1–4 | 1× | 120,960 blocks | ~8.4 h |
| 5–8 | 2× | 241,920 blocks | ~16.8 h |
| 9–12 | 3× | 362,880 blocks | ~25.2 h |
| 13–16 | 4× | 483,840 blocks | ~33.6 h |
| >40 | 10× (cap) | 1,209,600 blocks | ~84 h |

**Key property**: A single validator exiting alone pays the base period. A coordinated group of 40+ pays up to 10×.
This makes flash-loan-based validator exodus economically unattractive.

---

### 4.4 Two-Phase Exit

**Current behavior**: `ValidatorUnstake` immediately removes the validator from `get_active_validators()`,
meaning the validator stops participating in consensus the moment unstake is accepted.

**Target behavior**:

#### Phase 1: Epoch Grace (Current Epoch)
- `unstake` tx is accepted; `unbonding_start` is set.
- The validator **remains in the active set until the current epoch ends**.
- If already selected into the current subset, they must continue producing blocks and voting.
- Missed rounds during this grace period are subject to normal `slash_offline`.

#### Phase 2: Unbonding (Subsequent Epochs)
- New epochs no longer include this validator in subset selection.
- The validator record remains in `validators` map for `dynamic_unbonding_period` blocks.
- During this period:
  - **Double-sign reports still valid** → 100% slash + immediate removal.
  - **Any slash event** → unbonding period multiplied by `UNBONDING_SLASH_EXTEND` (2×).
  - **Offline is irrelevant** (not in subset), but any pre-epoch-end missed rounds still count.

#### Phase 3: Claim
- After `eligible_at_block` is reached, the validator (or anyone) may submit `ValidatorClaimUnbonded`.
- On success, stake is returned from `STAKING_ESCROW` to the original staker address.
- Validator record is permanently deleted.

---

### 4.5 Delegation Exit Linkage

> **Note**: Delegation is not yet exposed as a precompile, but the `delegate()` method exists in `ValidatorStateManager`.
> This section defines the intended behavior once delegation is enabled.

| Scenario | Behavior |
|---|---|
| Delegator undelegates | Undelegation enters the **same exit queue** as validator unstake. The unbonding period is identical to the target validator's current dynamic period. |
| Undelegate causes `staked_call < MIN_SELF_STAKE` | Validator is **automatically entered into the exit queue** with the same dynamic period. No additional tx required. |
| Validator is slashed below `MIN_SELF_STAKE` | Same as above — automatic queue entry. |
| Validator claims unbonded while delegators still pending | Delegators' undelegation requests remain in queue; funds are released to them independently when their own periods elapse. |

---

## 5. Code Integration Points

### 5.1 `crates/consensus/src/proposer.rs` — Expand `ConsensusParams`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusParams {
    // --- Existing fields ---
    pub max_validators: u32,
    pub subset_size: u32,
    pub block_time_millis: u64,
    pub slashing_window: u64,
    pub oracle_request_delay_ms: u64,
    pub epoch_length: u64,

    // --- Migrated from pub const (Tier 2) ---
    pub min_self_stake: u128,
    pub unbonding_period_blocks: u64,
    pub offline_slash_rate_bps: u128,
    pub key_rotation_grace_blocks: u64,

    // --- New parameters (Tier 3) ---
    pub churn_limit_quotient: u64,
    pub min_churn_limit: u64,
    pub safety_ratio_num: u32,
    pub safety_ratio_den: u32,
    pub unbonding_slash_extend: u32,
    pub max_unbonding_multiplier: u32,
}
```

Update `Default` impl to match the Mainnet (Genesis) column in §2.1.

### 5.2 `crates/consensus/src/validator.rs` — `ValidatorStateManager`

Replace duplicated instance fields with a `ConsensusParams` reference:

```rust
pub struct ValidatorStateManager {
    validators: HashMap<ValidatorId, ValidatorStake>,
    next_validator_id: ValidatorId,
    unbonding_requests: Vec<UnbondingRequest>,
    current_block: u64,
    key_rotations: Vec<KeyRotation>,

    // NEW: queues and churn tracking
    stake_queue: Vec<Address>,
    exit_queue: Vec<ValidatorId>,
    epoch_churn_count: u64,

    // Replaced: hold a reference to shared params instead of duplicating fields
    pub params: ConsensusParams,
}
```

**Modified methods**:
- `unstake()` — check safety floor against `params`, check churn limit, queue or execute.
- `stake()` — check churn limit against `params`, queue or execute.
- `claim_unbonded()` — unchanged; period already encoded in `eligible_at_block`.
- **New**: `process_epoch_churn()` — called at epoch boundary by `simplex.rs` or `bft_loop.rs`.

**Serialization**: Add `Serialize`/`Deserialize` derives (or custom impl) for `stake_queue`, `exit_queue`, and `epoch_churn_count` so they survive node restarts.

### 5.3 `crates/node/src/state_persist.rs` — Persistence

The validator state is already saved and loaded here:

```rust
// Save (existing pattern)
db_put::<CallValidatorState>(db, b"consensus", &serialized)?;

// Load (existing pattern)
Ok(SimplexConsensus::restore_from_persisted(state, validators.clone()))
```

**Required change**: Ensure `ValidatorStateManager`'s serialization includes the new queue fields. If using `serde`, this is automatic once the fields are added. If using custom encoding, extend the codec.

### 5.4 `crates/consensus/src/simplex.rs`

- `slash_offline()`: if `validator.is_unbonding()`, extend `eligible_at_block` by `params.unbonding_slash_extend`.
- Epoch transition: call `validator_state.process_epoch_churn()` to drain queues up to `churn_limit`.
- `active_validators()` / `qualified_validators()`: include validators whose `unbonding_start` is in the current epoch.

### 5.5 `crates/consensus/src/exec/validator.rs`

- `ValidatorUnstake` precompile: tx result may now include `queue_position` when queued.
- `ValidatorClaimUnbonded` precompile: no changes needed.

---

## 6. Governance Integration

All Tier 2 and Tier 3 parameters must be modifiable via governance without a binary upgrade.

### 6.1 New Governance Proposal Type

Add to `crates/governance/src/lib.rs`:

```rust
pub enum ProposalType {
    // ... existing variants ...

    /// Update a consensus parameter at runtime.
    /// Executed immediately after timelock; no contract call needed.
    UpdateConsensusParam {
        param: ConsensusParamKey,
        new_value: u64, // u64 covers all param ranges; cast where needed
    },
}

pub enum ConsensusParamKey {
    MinSelfStake,              // u128 (stored as u64 if fits, else use bytes)
    UnbondingPeriodBlocks,     // u64
    OfflineSlashRateBps,       // u128
    ChurnLimitQuotient,        // u64
    MinChurnLimit,             // u64
    SafetyRatioNum,            // u32
    SafetyRatioDen,            // u32
    UnbondingSlashExtend,      // u32
    MaxUnbondingMultiplier,    // u32
    KeyRotationGraceBlocks,    // u64
}
```

### 6.2 Execution Path

When a `UpdateConsensusParam` proposal passes timelock:

1. `GovernanceManager::execute()` decodes the proposal.
2. It acquires a write lock on the shared `ConsensusParams` (or sends a message to `bft_loop`).
3. The new value is written to both:
   - **In-memory** `ConsensusParams` (effective immediately for new blocks)
   - **RocksDB** under a well-known key (e.g. `call_governance_params`) so it survives restart
4. `bft_loop.rs` or `block_producer.rs` reads the updated params on the next block.

### 6.3 Governance Thresholds

| Action | Validator Quorum | Timelock |
|---|---|---|
| Normal parameter change | > 2/3 | 7 days |
| Emergency freeze (suspend churn) | > 1/2 | 24 hours |
| Unfreeze | > 2/3 | 7 days |

**Safety note**: Parameter changes affecting `MIN_SELF_STAKE` or `UNBONDING_PERIOD` must not retroactively alter validators already in the unbonding queue. The new value applies only to **new** stake/unstake requests.

---

## 7. Governance Roadmap

All parameters below are **governable** via `GovernanceSubmitProposal` (per `docs/governance.md`).

| Phase | Time | Parameter Changes | Rationale |
|---|---|---|---|
| **Genesis** | T+0 | Use Mainnet (Conservative) column | Prioritize safety over liquidity at launch |
| **Stabilization** | T+3 months | `UNBONDING_PERIOD` 120,960 → 60,480 | Lower if >100 validators active and 0 incidents |
| **Liquidity** | T+6 months | `CHURN_LIMIT_QUOTIENT` 16 → 12 | Allow faster validator rotation |
| **Maturity** | T+12 months | Remove `SAFETY_RATIO` hard floor | Replace with dynamic economic constraint (e.g. minimum stake-weighted participation) |
| **Optional** | Any | `SUBSET_SIZE` 21 → 31 | If validator count grows >300 and latency permits |

---

## 8. Reference: Chain Comparison

| Chain | Unbonding Period | Churn Limit | Safety Floor | Dynamic Period |
|---|---|---|---|---|
| **Callchain (proposed)** | 8.4h base, up to 84h | `max(2, q/16)` / epoch | `ceil(21 × 4/3) = 28` | Yes |
| Ethereum (PoS) | ~27 hours | `max(4, n/65536)` / epoch | Implicit via activation queue | No |
| Cosmos (Tendermint) | ~21 days | None (period itself limits) | None | No |
| Polkadot (NPoS) | ~28 days | Per-era election limit | Minimum backing stake | No |
| Solana | ~2–8 days | Warm-up / cool-down epochs | Minimum stake for leader schedule | No |
| Aptos | Configurable | Per-epoch limit | Minimum validator set size | No |

**Callchain's differentiators**:
- **Dynamic period**: Most chains use fixed unbonding; Callchain makes it congestion-dependent.
- **Safety floor as hard reject**: Ethereum queues; Callchain rejects below threshold — simpler for BFT safety.
- **Two-phase exit**: Most chains remove validator from active set immediately; Callchain keeps them accountable through the current epoch.

---

## Appendix A: Quick Reference Card

```
┌─────────────────────────────────────────────────────────────┐
│  Callchain Validator Exit Flow                              │
├─────────────────────────────────────────────────────────────┤
│  1. Submit ValidatorUnstake tx                              │
│     ├─ qualified - 1 >= 28 ?                                │
│     │   └─ No → REVERT "below safety threshold"             │
│     ├─ epoch_churn < churn_limit ?                          │
│     │   └─ No → QUEUE, return queue_position                │
│     └─ Yes → ACCEPT, set unbonding_start                    │
│                                                              │
│  2. Epoch Grace (current epoch)                             │
│     └─ Validator still in subset, must vote/propose         │
│        └─ Missed rounds → slash_offline()                   │
│                                                              │
│  3. Unbonding Period                                        │
│     └─ duration = base × ceil(queue_len / churn_limit)      │
│        └─ Capped at base × 10                               │
│        └─ Double-sign → 100% slash + immediate remove       │
│        └─ Any slash → period *= 2                           │
│                                                              │
│  4. Claim                                                   │
│     └─ After eligible_at_block, submit ValidatorClaim       │
│        └─ Stake returned from escrow to original staker     │
│        └─ Validator record deleted                          │
└─────────────────────────────────────────────────────────────┘
```

## Appendix B: Parameter Migration Checklist

When implementing this design, use the following checklist:

- [ ] Add new fields to `ConsensusParams` in `proposer.rs`
- [ ] Remove `pub const MIN_SELF_STAKE`, `UNBONDING_PERIOD_SECS`, `OFFLINE_SLASH_RATE_PER_ROUND`, `KEY_ROTATION_GRACE_BLOCKS` from `validator.rs`
- [ ] Update `ValidatorStateManager` to hold `ConsensusParams` instead of individual fields
- [ ] Add `stake_queue`, `exit_queue`, `epoch_churn_count` to `ValidatorStateManager`
- [ ] Update `ValidatorStateManager` serde (or custom codec) to include new queue fields
- [ ] Verify `state_persist.rs` saves/restores queue state correctly
- [ ] Add `UpdateConsensusParam` proposal type to governance
- [ ] Implement governance execution path that writes to DB + updates in-memory params
- [ ] Update all `genesis.json` files (devnet, testnet) with new parameter values
- [ ] Add consensus tests for churn limit, safety floor, and dynamic period
