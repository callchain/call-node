# Oracle Design

## Overview

The CallChain oracle provides decentralized, on-chain price feeds for smart contracts. Validators submit signed prices for registered **price pairs**, and a quorum-based aggregation produces a median price with outlier detection, TWAP history, and economic incentives.

**Crate**: `crates/oracle/` (`call-oracle`)
**Precompile**: `0x101` (EVM access)
**Spec**: §25

---

## Price Semantics

All oracle prices are quoted as **`PricePair { base, quote }`**:

| Field | Type | Description |
|---|---|---|
| `base` | `AssetId` | The asset being priced |
| `quote` | `AssetId` | The denomination currency |

### Reserved IDs

| ID | Meaning |
|---|---|
| `0` | USD quote currency (reserved, **not a real token**) |
| `1` | Native CALL token |

### Common Pairs

| Pair | Semantics |
|---|---|
| `{ base: 1, quote: 0 }` | CALL/USD — native token price in dollars |
| `{ base: 2, quote: 0 }` | USDC/USD — stablecoin price in dollars |
| `{ base: 2, quote: 1 }` | USDC/CALL — stablecoin price in CALL |

Prices use **6 decimal places** (e.g. `$2.00` = `2_000_000`).

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Block Producer                        │
│  ┌──────────────────────────────────────────────┐       │
│  │ 1. Broadcast OraclePriceRequest (P2P ch=4)   │       │
│  │ 2. Wait oracle_request_delay_ms (default 200)│       │
│  │ 3. Collect OraclePriceSubmission responses   │       │
│  │ 4. Aggregate via OracleTracker               │       │
│  │ 5. Call submitPrice on precompile 0x101      │       │
│  └──────────────────────────────────────────────┘       │
└─────────────────────┬───────────────────────────────────┘
                      │
         ┌────────────┴────────────┐
         │                         │
    ┌────▼────┐             ┌──────▼──────┐
    │Validator│             │Validator    │
    │  fetch  │             │  fetch      │
    │  price  │             │  price      │
    │ (HTTP/  │             │ (HTTP/      │
    │  local) │             │  local)     │
    └────┬────┘             └──────┬──────┘
         │ submit via              │ submit via
         │ P2P + Ed25519           │ P2P + Ed25519
         └────────────┬────────────┘
                      │
              ┌───────▼────────┐
              │  OracleTracker  │
              │  (transient)    │
              │  pending /      │
              │  contributors   │
              └───────┬────────┘
                      │
              ┌───────▼────────┐
              │  EVM Precompile │
              │  0x101          │
              │  (canonical     │
              │   price/TWAP)   │
              └────────────────┘
```

## Precompile Functions

The **Oracle precompile at `0x101`** exposes both read and write operations:

| Operation | Function | Type | Gas |
|---|---|---|---|
| Read | `getPrice(uint64)` | view | 1,000 |
| Read | `getTWAP(uint64)` | view | 1,000 |
| Read | `isStale(uint64)` | view | 1,000 |
| Write | `submitPrice(uint64,uint128,uint64,uint64)` | mutate | 30,000 |
| Write | `setTrackedAssets(uint64[])` | mutate | 50,000 |

Gas is fixed per operation.

`submitPrice` requires the caller to be a registered validator (`validator_id != 0`). Each validator's submission is written directly to EVM storage; the caller is responsible for providing an accurate price. In practice, the block producer aggregates multiple validator submissions via `OracleTracker`, computes the median, and writes the aggregated result via `submitPrice`. See [precompile.md](precompile.md) for the full ABI.

---

## OracleTracker

`OracleManager` has been deleted. Only `OracleTracker` remains. The tracker is **transient** (not persisted) — all canonical price/TWAP data lives in EVM storage under the oracle precompile address (`0x101`).

### State

| Field | Purpose |
|---|---|
| `pending` | Submissions for current period, per `PricePair` |
| `current_contributors` | Validators who contributed to last quorum |
| `last_outliers` | Validators flagged as outliers in last round |

`OracleTracker` does **not** store aggregated prices, TWAP history, validator registration, or config. Those live in EVM storage under `0x101`.

### Submission Flow

1. Validator signs `OracleSubmission` (`validator_id`, `pair`, `price`, `block_number`, `timestamp`, `sources`) using Ed25519
2. `OracleTracker` validates:
   - Validator exists and is active (read from EVM storage)
   - No duplicate submission (same validator, same block)
   - Block is at an update interval boundary (`block % 1000 == 0`)
   - Ed25519 signature is valid (message includes `pair.base` and `pair.quote`)
   - Data source attestation: at least `min_data_sources` sources, passes allowlist if configured
3. Accepted submissions go into `pending[pair]`
4. When submissions for a pair reach `oracle_quorum()`, aggregation triggers

### Quorum

Dynamic: `ceil(2/3 * n)` where `n` is the active validator count. Minimum 1, capped at `n`.

### Aggregation

1. Sort all submitted prices ascending
2. Compute median at index `prices.len() / 2`
3. Detect outliers: any price deviating > 500 bps (5%) from median
4. Record outlier validator IDs in `last_outliers` (transient, in-memory only)
5. Record non-outlier submitters in `current_contributors`

Note: Outlier detection is currently in-memory only. Persistent strikes, automatic disabling, and slashing are not yet implemented.

### Period Advance

Called by block proposer at each `ORACLE_UPDATE_INTERVAL` (every 1000 blocks):

- For pairs without quorum: carry forward the last known price (graceful degradation)
- Clear pending for all pairs

---

## P2P Oracle Channel

### Request

Proposer broadcasts `OraclePriceRequest` on channel 4 at oracle boundaries:

```rust
struct OraclePriceRequest {
    pairs: Vec<PricePair>,  // tracked price pairs
    block: u64,             // current block height
    requester_id: u32,      // proposer validator ID
}
```

### Response

Validators respond with signed `OraclePriceSubmission`:

```rust
struct OraclePriceSubmission {
    validator_id: u32,
    pair: PricePair,        // e.g. { base: 1, quote: 0 } for CALL/USD
    price: u128,
    block_number: u64,
    timestamp: u64,
    signature: [u8; 64],    // Ed25519 (signs base + quote + price + block + timestamp)
    sources: Vec<String>,   // e.g. ["binance", "coinbase"]
}
```

Proposer waits `oracle_request_delay_ms` (default 200ms) after broadcast to collect responses, then aggregates via `OracleTracker` and writes the result to EVM storage via `submitPrice` on the oracle precompile (`0x101`).

---

## Fee Currency Conversion (Phase 2)

Stablecoin fees are converted to CALL equivalent using a **two-lookup model**:

```
stablecoin_fee_in_call = stablecoin_fee * stablecoin_usd_price / call_usd_price
```

| Lookup | Pair | Meaning |
|---|---|---|
| `stablecoin_usd_price` | `{ base: asset_id, quote: 0 }` | Stablecoin price in USD |
| `call_usd_price` | `{ base: 1, quote: 0 }` | CALL price in USD |

This eliminates the semantic ambiguity of the old single-lookup model and makes price derivation explicit.

---

## Economic Incentives

### Rewards

`OracleTracker::distribute_rewards(reward_pool)` distributes rewards proportionally among `current_contributors` (validators whose submissions were included in the last quorum aggregation and were not flagged as outliers). First contributor absorbs the remainder to avoid dust loss.

Note: The actual funding of the reward pool and invocation of `distribute_rewards` during block production is not yet implemented.

### Penalties

Outlier submissions (> 5% deviation from median) are detected during aggregation and recorded in `last_outliers` (transient, in-memory only). Persistent strikes, automatic disabling, and stake slashing are not yet implemented.

---

## TWAP Calculation

The precompute implements an **incremental cumulative average** (not a time-weighted average):

```rust
new_twap = (old_twap * count + price) / (count + 1)
```

- `count` increments with each `submitPrice` call
- First submission: `twap = price`
- Subsequent submissions: cumulative mean of all submitted prices
- Stored per `asset_id` in EVM storage
- The `ORACLE_TWAP_WINDOW_SECS` constant (24 hours) defines the intended policy window but is not enforced by the current TWAP computation

---

## EVM Precompile

**Address**: `0x0000000000000000000000000000000000000101`

The precompile reads canonical price and TWAP data from EVM storage under `0x101`. There is no `set_live_oracle()` or live `OracleManager` instance.

### ABI Interface

```solidity
interface IProtocolOracle {
    function getPrice(uint64 assetId) external view returns (uint128);
    function getTWAP(uint64 assetId) external view returns (uint128);
    function isStale(uint64 assetId) external view returns (uint8);
    function submitPrice(uint64 assetId, uint128 price, uint64 timestamp, uint64 blockNumber) external;
    function setTrackedAssets(uint64[] assetIds) external;
}
```

Selectors are generated automatically by `alloy_sol_types` from the interface definition above.

**Note**: The precompile ABI accepts a single `asset_id` and implicitly quotes in USD (`quote = 0`). For cross-asset pairs (e.g. USDC/CALL), use the `OracleTracker` API directly.

---

## Constants

| Constant | Value | Description |
|---|---|---|
| `ORACLE_UPDATE_INTERVAL` | 1000 | Blocks between oracle periods |
| `ORACLE_PERIOD_SECS` | 240 | Oracle period in seconds |
| `ORACLE_OUTLIER_THRESHOLD_BPS` | 500 | Outlier threshold: 5% deviation |
| `ORACLE_OUTLIER_TOLERANCE` | 10 | Strikes before validator disabled |
| `ORACLE_TWAP_WINDOW_SECS` | 86,400 | TWAP window: 24 hours (policy constant; not enforced by current TWAP computation) |
| `ORACLE_STALENESS_SECS` | 900 | Staleness threshold: 15 minutes (OracleConfig default) |
| `STALE_THRESHOLD_SECS` | 3,600 | Staleness threshold used by precompile `isStale`: 1 hour |
| `ORACLE_MIN_DATA_SOURCES` | 2 | Minimum independent sources per submission |

---

## Price Fetching

Validators implement the `PriceFetcher` trait to supply prices:

```rust
pub trait PriceFetcher: Send + Sync {
    fn fetch_price(&self, pair: PricePair) -> Option<u128>;
    fn sources(&self) -> Vec<String>;
}
```

### Implementations

- **`NoOpPriceFetcher`**: Returns `None`. Default for devnet or when no API is configured.
- **`HttpPriceFetcher`** (`http-fetcher` feature): Queries HTTP endpoints configured per `asset_id` (base). Supports JSON formats with `price`, `lastPrice`, or `last` fields. Implicitly provides USD-quoted prices.

---

## Crate Structure

```
crates/oracle/
├── Cargo.toml
└── src/
    ├── lib.rs           # Types, OracleConfig, OracleError
    ├── precompile.rs    # OraclePrecompile (0x101), OracleStorage
    ├── tracker.rs       # OracleTracker, aggregation logic
    ├── fetcher.rs       # PriceFetcher trait, HttpPriceFetcher
    ├── crypto.rs        # Ed25519 signing helpers
    ├── constants.rs     # Oracle constants
    └── tests.rs         # Additional tests
```

### Dependencies

- `call-primitives` — `Address`, `AssetId`, `Ed25519PublicKey`, `PricePair`
- `call-crypto` — Ed25519 sign/verify
- `alloy-primitives` — `U256` for TWAP arithmetic
- `ed25519-dalek` — signing key type
- `serde`, `serde_bytes` — serialization
- `thiserror` — error types
- `reqwest`, `tokio` (optional, `http-fetcher` feature) — HTTP fetching

### Downstream Crates

| Crate | Usage |
|---|---|
| `call-precompile` | EVM precompile reads from EVM storage under `0x101` |
| `call-protocol` | `OracleTracker`, `OracleSubmission` for precompile execution |
| `call-consensus` | `OracleTracker` for block execution, slashing, rewards |
| `call-rpc` | `OracleSubmission` for RPC endpoint |
| `call-node` | Full oracle lifecycle: P2P, block production |
| `call-chainspec` | Oracle genesis initialization |

---

## Genesis Initialization

Oracle state is initialized during genesis:

1. Genesis config may specify `oracle_assets: Vec<u64>` — tracked asset IDs (all implicitly quoted in USD, i.e. `quote = 0`)
2. `set_tracked_assets(asset_ids)` stores the tracked list in EVM storage under `0x101`

Note: Validator registration for oracle purposes reuses the validator staking precompile (`0x204`). There is no separate oracle validator registry.

---

## Edge Cases

- **No quorum reached**: Last known price is carried forward. Oracle never stalls.
- **All validators disabled**: Price becomes stale; contracts reading `isStale()` can handle gracefully.
- **Node restart**: `OracleTracker` state is lost (pending submissions, contributors, outliers). Canonical prices and TWAP history are restored from EVM storage on the next block.
- **Empty tracked pairs**: Price request broadcast is skipped; no wasted P2P traffic.
- **Single validator**: Quorum is 1; validator's own price is the median.
- **CALL/USD missing**: Contracts reading `getPrice(1)` receive `0` until the first price is submitted.
