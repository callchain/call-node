# Oracle Design

## Overview

The CallChain oracle provides decentralized, on-chain price feeds for smart contracts. Validators submit signed prices for registered assets, and a quorum-based aggregation produces a median price with outlier detection, TWAP history, and economic incentives.

**Crate**: `crates/oracle/` (`call-oracle`)
**Precompile**: `0x101` (EVM access)
**Spec**: §25

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
              └───────┬────────┘
                      │
              ┌───────▼────────┐
              │  EVM Precompile │
              │  0x101          │
              └────────────────┘
```

---

## OracleManager

### State

| Field | Purpose |
|---|---|
| `config` | Update interval, outlier thresholds, TWAP window, staleness |
| `validators` | Per-validator registration, activity, outlier count |
| `tracked_assets` | Asset IDs the oracle monitors |
| `pending` | Submissions for current period, per asset |
| `aggregated` | Current quorum-aggregated median prices |
| `history` | Historical prices for TWAP calculation |
| `reward_pool` | Accumulated fee pool for oracle rewards |
| `current_contributors` | Validators who contributed to last quorum |
| `last_outliers` | Validators flagged as outliers in last round |

### Submission Flow

1. Validator signs `OracleSubmission` (validator_id, asset_id, price, block_number, timestamp, sources) using Ed25519
2. `OracleManager.submit_price()` validates:
   - Validator exists and is active
   - No duplicate submission (same validator, same block)
   - Block is at an update interval boundary (`block % 1000 == 0`)
   - Ed25519 signature is valid
   - Data source attestation: at least `min_data_sources` sources, passes allowlist if configured
3. Accepted submissions go into `pending`
4. When submissions for an asset reach `oracle_quorum()`, aggregation triggers

### Quorum

Dynamic: `ceil(2/3 * n)` where `n` is the active validator count. Minimum 1, capped at `n`.

### Aggregation

1. Sort all submitted prices ascending
2. Compute median at index `prices.len() / 2`
3. Detect outliers: any price deviating > 500 bps (5%) from median
4. Strike outlier validators (increment `outlier_count`)
5. Disable validators after 10 strikes (`is_active = false`)
6. Store `AggregatedPrice` (median, submission count, outlier count)
7. Record non-outlier submitters in `current_contributors`
8. Append median to `history` for TWAP
9. Prune history beyond 24h window

### Period Advance

Called by block proposer at each `ORACLE_UPDATE_INTERVAL` (every 1000 blocks):

- For assets without quorum: carry forward the last known price (graceful degradation)
- Prune TWAP history beyond window
- Clear pending for all assets

---

## P2P Oracle Channel

### Request

Proposer broadcasts `OraclePriceRequest` on channel 4 at oracle boundaries:

```rust
struct OraclePriceRequest {
    asset_ids: Vec<u64>,    // tracked assets
    block: u64,             // current block height
    requester_id: u32,      // proposer validator ID
}
```

### Response

Validators respond with signed `OraclePriceSubmission`:

```rust
struct OraclePriceSubmission {
    validator_id: u32,
    asset_id: u64,
    price: u128,
    block_number: u64,
    timestamp: u64,
    signature: [u8; 64],    // Ed25519
    sources: Vec<String>,   // e.g. ["binance", "coinbase"]
}
```

Proposer waits `oracle_request_delay_ms` (default 200ms) after broadcast to collect responses before advancing the period.

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
    fn fetch_price(&self, asset_id: AssetId) -> Option<u128>;
    fn sources(&self) -> Vec<String>;
}
```

### Implementations

- **`NoOpPriceFetcher`**: Returns `None`. Default for devnet or when no API is configured.
- **`HttpPriceFetcher`** (`http-fetcher` feature): Queries HTTP endpoints configured per asset ID. Supports JSON formats with `price`, `lastPrice`, or `last` fields.

---

## Crate Structure

```
crates/oracle/
├── Cargo.toml
└── src/
    └── lib.rs          # OracleManager, PriceFetcher, types, errors
```

### Dependencies

- `call-primitives` — `Address`, `AssetId`, `Ed25519PublicKey`
- `call-crypto` — Ed25519 sign/verify
- `alloy-primitives` — `U256` for TWAP arithmetic
- `ed25519-dalek` — signing key type
- `serde`, `serde_bytes` — serialization
- `thiserror` — error types
- `reqwest`, `tokio` (optional, `http-fetcher` feature) — HTTP fetching

### Downstream Crates

| Crate | Usage |
|---|---|
| `call-precompiles` | `OracleManager` for EVM precompile |
| `call-protocol` | `OracleManager`, `OracleSubmission` for instruction execution |
| `call-consensus` | `OracleManager` for block execution, slashing, rewards |
| `call-rpc` | `OracleSubmission` for RPC endpoint |
| `call-node` | Full oracle lifecycle: P2P, block production, persistence |
| `call-chainspec` | Oracle genesis initialization |

---

## Genesis Initialization

Oracle is boot-strapped during genesis:

1. Genesis config specifies `oracle_assets: Vec<u64>` — tracked asset IDs
2. Genesis validators are registered into `OracleManager` with their Ed25519 public keys
3. `OracleConfig` is initialized from genesis parameters (or defaults)
4. The oracle instance is stored in `RpcState` and wired to the precompile via `set_live_oracle()`

---

## Edge Cases

- **No quorum reached**: Last known price is carried forward. Oracle never stalls.
- **All validators disabled**: Price becomes stale; contracts reading `isStale()` can handle gracefully.
- **Node restart**: Oracle state rebuilds from genesis; aggregated prices are restored from persistence.
- **Empty tracked assets**: Price request broadcast is skipped; no wasted P2P traffic.
- **Single validator**: Quorum is 1; validator's own price is the median.
