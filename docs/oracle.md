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
│  │ 4. Feed into OracleManager.submit_price()    │       │
│  │ 5. advance_period() at boundary              │       │
│  │ 6. Slash outliers via consensus              │       │
│  │ 7. Distribute rewards to contributors         │       │
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
              │  OracleManager  │
              │  (aggregation)  │
              │  keyed by pair  │
              └───────┬────────┘
                      │
              ┌───────▼────────┐
              │  EVM Precompile │
              │  0x101          │
              └────────────────┘
```

## Precompile Functions

The **Oracle precompile at `0x101`** exposes both read and write operations:

| Operation | Function | Type | Gas |
|---|---|---|---|
| Read | `getPrice(uint64)` | view | 1,000 |
| Read | `getTWAP(uint64,uint64)` | view | 1,500 |
| Read | `isStale(uint64,uint64)` | view | 800 |
| Write | `submitPrice(uint64,uint128,uint64,uint64,bytes,bytes[])` | — | 5,000 |

`submitPrice` allows registered validators to submit prices directly via EVM transactions (e.g., from a Solidity contract or MetaMask). See [precompile.md](precompile.md) for the full ABI.

---

## OracleManager

### State

| Field | Purpose |
|---|---|
| `config` | Update interval, outlier thresholds, TWAP window, staleness |
| `validators` | Per-validator registration, activity, outlier count |
| `tracked_pairs` | `PricePair`s the oracle monitors |
| `pending` | Submissions for current period, per `PricePair` |
| `aggregated` | Current quorum-aggregated median prices, per `PricePair` |
| `history` | Historical prices for TWAP calculation, per `PricePair` |
| `reward_pool` | Accumulated fee pool for oracle rewards |
| `current_contributors` | Validators who contributed to last quorum |
| `last_outliers` | Validators flagged as outliers in last round |

### Submission Flow

1. Validator signs `OracleSubmission` (`validator_id`, `pair`, `price`, `block_number`, `timestamp`, `sources`) using Ed25519
2. `OracleManager.submit_price()` validates:
   - Validator exists and is active
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
4. Strike outlier validators (increment `outlier_count`)
5. Disable validators after 10 strikes (`is_active = false`)
6. Store `AggregatedPrice` (pair, median, submission count, outlier count)
7. Record non-outlier submitters in `current_contributors`
8. Append median to `history[pair]` for TWAP
9. Prune history beyond 24h window

### Period Advance

Called by block proposer at each `ORACLE_UPDATE_INTERVAL` (every 1000 blocks):

- For pairs without quorum: carry forward the last known price (graceful degradation)
- Prune TWAP history beyond window
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

Proposer waits `oracle_request_delay_ms` (default 200ms) after broadcast to collect responses before advancing the period.

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

- Block fees allocate a share to `oracle.reward_pool` (configured via `oracle_fee_share_bps` in `FeeParams`)
- At each oracle period boundary, rewards distribute proportionally among `current_contributors`
- First contributor absorbs remainder to avoid dust loss
- Pool resets to 0 after distribution

### Penalties

- Outlier submissions (> 5% deviation from median) earn the validator a strike
- After 10 strikes, the validator is disabled from oracle participation
- Separately, `SimplexConsensus.slash_oracle_outlier()` slashes 0.1% of the validator's self-stake per outlier event
- Disabled validators can be reset via `OracleManager.reset_validator()` (intended for governance/multi-sig)

---

## TWAP Calculation

Time-weighted average price over the configured window (default 24 hours):

- Each historical price entry is weighted by its duration of validity (time until the next price update, or until `current_timestamp` for the most recent entry)
- Uses `U256` arithmetic to avoid overflow
- Single entry within window returns that price directly
- TWAP is computed per `PricePair`

---

## EVM Precompile

**Address**: `0x0000000000000000000000000000000000000101`

The precompile reads from a live `OracleManager` instance shared via `set_live_oracle()` during node boot.

### Selectors

| Function | Selector | Input | Output |
|---|---|---|---|
| `getPrice` | `0x763e4d8c` | `uint64 asset_id` | `uint128 price` (0 if unknown) |
| `getTWAP` | `0xabcdef01` | `uint64 asset_id, uint64 current_timestamp` | `uint128 twap` (0 if unknown) |
| `isStale` | `0x12345678` | `uint64 asset_id, uint64 current_timestamp` | `bool` |
| `getOracleStatus` | `0x9abcde01` | `uint64 asset_id, uint64 current_timestamp` | `(uint8 status, uint128 price, uint64 timestamp)` |

**Note**: The precompile ABI accepts a single `asset_id` and implicitly quotes in USD (`quote = 0`). For cross-asset pairs (e.g. USDC/CALL), use the `OracleState` API directly.

Status codes: `0 = Disabled` (no price), `1 = Stale`, `2 = Active`

---

## Constants

| Constant | Value | Description |
|---|---|---|
| `ORACLE_UPDATE_INTERVAL` | 1000 | Blocks between oracle periods |
| `ORACLE_PERIOD_SECS` | 240 | Oracle period in seconds |
| `ORACLE_OUTLIER_THRESHOLD_BPS` | 500 | Outlier threshold: 5% deviation |
| `ORACLE_OUTLIER_TOLERANCE` | 10 | Strikes before validator disabled |
| `ORACLE_TWAP_WINDOW_SECS` | 86,400 | TWAP window: 24 hours |
| `ORACLE_STALENESS_SECS` | 900 | Staleness threshold: 15 minutes |
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
    └── lib.rs          # OracleManager, PriceFetcher, types, errors
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
| `call-precompile` | `OracleManager` for EVM precompile (legacy `asset_id` compat) |
| `call-protocol` | `OracleManager`, `OracleSubmission` for precompile execution |
| `call-consensus` | `OracleManager` for block execution, slashing, rewards |
| `call-rpc` | `OracleSubmission` for RPC endpoint |
| `call-node` | Full oracle lifecycle: P2P, block production, persistence |
| `call-chainspec` | Oracle genesis initialization |

---

## Genesis Initialization

Oracle is boot-strapped during genesis:

1. Genesis config specifies `oracle_assets: Vec<u64>` — tracked asset IDs (all implicitly quoted in USD, i.e. `quote = 0`)
2. Genesis validators are registered into `OracleManager` with their Ed25519 public keys
3. `OracleConfig` is initialized from genesis parameters (or defaults)
4. `set_tracked_assets(asset_ids)` converts each ID to `PricePair { base: id, quote: 0 }`
5. The oracle instance is stored in `RpcState` and wired to the precompile via `set_live_oracle()`

---

## Legacy Compatibility

The following methods accept an `AssetId` and implicitly assume USD quotation (`quote = 0`):

- `OracleManager::get_price_by_asset(asset_id)`
- `OracleManager::get_twap_by_asset(asset_id, timestamp)`
- `OracleManager::is_stale_by_asset(asset_id, timestamp)`
- `OracleManager::simple_submit_price_by_asset(asset_id, ...)`
- `OracleManager::record_direct_price_by_asset(asset_id, ...)`
- `OracleManager::set_tracked_assets(asset_ids)`

These are used by the EVM precompile, RPC endpoints, fee currency registry, and genesis setup to minimize API surface changes while the core state is fully `PricePair`-aware.

---

## Edge Cases

- **No quorum reached**: Last known price is carried forward. Oracle never stalls.
- **All validators disabled**: Price becomes stale; contracts reading `isStale()` can handle gracefully.
- **Node restart**: Oracle state rebuilds from genesis; aggregated prices are restored from persistence.
- **Empty tracked pairs**: Price request broadcast is skipped; no wasted P2P traffic.
- **Single validator**: Quorum is 1; validator's own price is the median.
- **CALL/USD missing**: `get_call_price()` falls back to hardcoded `$2.00` (`2_000_000`).
