# Callchain Genesis File Reference

This document describes the JSON genesis file format used by Callchain nodes to initialize state at first boot.

---

## Overview

The genesis file is a single JSON document that defines the initial state of a Callchain network. When a node starts for the first time with a `--genesis` path, the `GenesisExecutor` reads this file, validates it, and produces the initial `GenesisState` including balances, validators, assets, EVM state, and computed Merkle roots.

On subsequent restarts, the genesis file is **skipped**; the node resumes from persisted database state.

---

## Top-Level Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `version` | `number` | Yes | Genesis format version. Must be `1`. |
| `chain_name` | `string` | Yes | Human-readable chain name, e.g. `"callchain-devnet"`. |
| `chain_id` | `number` | Yes | Unique chain identifier. Must be non-zero. |
| `timestamp_millis` | `number` | Yes | Genesis timestamp in Unix milliseconds. Must be non-zero. |
| `initial_assets` | `array` | Yes | List of [`GenesisAsset`](#genesisasset) entries. Must contain at least one. |
| `initial_fee_currencies` | `array` | Yes | List of [`GenesisFeeCurrency`](#genesisfeecurrency) entries. Must contain at least one. |
| `validators` | `array` | Yes | List of [`GenesisValidator`](#genesiscurrency) entries. Must contain at least one. |
| `consensus_params` | `object` | Yes | [`ConsensusParams`](#consensusparams) — BFT consensus configuration. |
| `oracle_assets` | `array` | No | Asset IDs (`number`) tracked by the oracle for price submissions. Defaults to empty. |

### Notes

- `fee_params` exists internally but is **not serialized** to JSON; it always uses `FeeParams::default()`.
- Asset IDs in `initial_assets` must be unique. Duplicates are rejected at validation time.

---

## GenesisAsset

Defines an asset to register at genesis and its initial token distribution.

| Field | Type | Required | Description |
|---|---|---|---|
| `asset_id` | `number` | Yes | Unique asset identifier. `1` is reserved for the native CALL token. |
| `symbol` | `string` | Yes | Token ticker, e.g. `"CALL"`. |
| `name` | `string` | Yes | Full token name, e.g. `"Call Token"`. |
| `decimals` | `number` | Yes | Number of decimal places (typically `18`). |
| `initial_supply` | `string` | Yes | Total supply as a decimal string. Used for EVM ERC-20 template deployment. |
| `distribution` | `object` | Yes | Map of `address (hex)` -> `amount (string)`. See [Distribution format](#distribution-format). |

### Distribution format

Each key is a 20-byte Ethereum address with `0x` prefix. Each value is the initial balance as a **decimal string** (no exponent notation).

```json
{
  "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa": "500000000000000000000000000",
  "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb": "500000000000000000000000000"
}
```

### Special behaviour

- The asset with `asset_id == 1` is treated as the native CALL token. Its distribution balances are also injected into the EVM state as native EVM balances.
- All assets are registered in the `AssetRegistry` with `Address::ZERO` as issuer and policy `0`.

---

## GenesisFeeCurrency

Declares which assets may be used to pay transaction fees.

| Field | Type | Required | Description |
|---|---|---|---|
| `symbol` | `string` | Yes | Currency symbol, e.g. `"CALL"`. |
| `asset_id` | `number` | Yes | Asset ID that matches an entry in `initial_assets`. |
| `oracle_address` | `string \| null` | No | Oracle contract/feed address for price lookups. `null` for native CALL. |

### Notes

- If the `asset_id` is not present in `initial_assets`, the executor auto-registers a placeholder asset with 18 decimals.
- All entries are added to the node's `FeeCurrencyRegistry` at block `0`.

---

## GenesisValidator

Registers the initial validator set at genesis.

| Field | Type | Required | Description |
|---|---|---|---|
| `address` | `string` | Yes | Validator's 20-byte address, e.g. `"0xaaaa...aaaa"`. |
| `ed25519_pubkey` | `string` | Yes | 32-byte Ed25519 public key in hex with `0x` prefix. |
| `self_stake` | `string` | Yes | Self-staked amount in CALL (18 decimals). Must be >= `MIN_SELF_STAKE`. |

### Notes

- Validators are automatically registered in the `ValidatorStateManager` (staked), the `OracleManager` (for price submission), and the `GovernanceManager` (for voting).
- Validator IDs are assigned sequentially starting from `0` in the order they appear in the array.

---

## ConsensusParams

BFT consensus and block-production parameters.

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `max_validators` | `number` | Yes | `216` | Hard cap on the active validator set size. |
| `subset_size` | `number` | Yes | `21` | Number of validators selected as proposers per epoch. |
| `block_time_millis` | `number` | Yes | `250` | Target block time in milliseconds. |
| `slashing_window` | `number` | Yes | `10000` | Rounds before the offline-slashing counter resets. |
| `oracle_request_delay_ms` | `number` | Yes | `200` | Delay after broadcasting oracle price requests. |
| `epoch_length` | `number` | Yes | `100` | Blocks per epoch before rotating the proposer subset. |

---

## Full Example: Devnet

```json
{
  "version": 1,
  "chain_name": "callchain-devnet",
  "chain_id": 8886,
  "timestamp_millis": 1751328000000,
  "initial_assets": [
    {
      "asset_id": 1,
      "symbol": "CALL",
      "name": "Call Token",
      "decimals": 18,
      "initial_supply": "1000000000000000000000000000",
      "distribution": {
        "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa": "500000000000000000000000000",
        "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb": "500000000000000000000000000"
      }
    }
  ],
  "initial_fee_currencies": [
    {
      "symbol": "CALL",
      "asset_id": 1,
      "oracle_address": null
    }
  ],
  "validators": [
    {
      "address": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "ed25519_pubkey": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "self_stake": "1000000000000000000000000"
    },
    {
      "address": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "ed25519_pubkey": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "self_stake": "1000000000000000000000000"
    }
  ],
  "consensus_params": {
    "max_validators": 10,
    "subset_size": 3,
    "block_time_millis": 250,
    "slashing_window": 1000,
    "oracle_request_delay_ms": 200,
    "epoch_length": 100
  },
  "oracle_assets": [1]
}
```

---

## Validation Rules

The `GenesisExecutor::execute()` method enforces the following rules before producing state:

1. `chain_name` must be non-empty.
2. `chain_id` must be non-zero.
3. `timestamp_millis` must be non-zero.
4. `validators` must contain at least one entry.
5. `initial_assets` must contain at least one entry.
6. `initial_fee_currencies` must contain at least one entry.
7. All `asset_id` values in `initial_assets` must be unique.

If any rule fails, the node exits with a `GenesisError` during boot.

---

## Genesis Execution Flow

When `GenesisExecutor::execute()` runs:

1. **Validate** the schema (rules above).
2. **Initialize** empty state tables (`BalanceState`, `AssetRegistry`, `EvmState`, `ValidatorStateManager`).
3. **Register assets** — for each `GenesisAsset`, register in `AssetRegistry` and distribute balances. Asset `1` also populates EVM native balances.
4. **Register validators** — stake each `GenesisValidator` into `ValidatorStateManager`.
5. **Register fee currencies** — add each `GenesisFeeCurrency` to the fee-currency registry.
6. **Deploy EVM ERC-20 templates** — for asset `1` (CALL), deploy the system ERC-20 contract into `EvmState`.
7. **Initialize oracle** — register all genesis validators as oracle reporters and set tracked assets.
8. **Compute state roots** — `payment_root`, `evm_state_root`, and `bridge_root` (always `ZERO` in genesis).

---

## Files in Repository

| File | Purpose |
|---|---|
| `chainspec/devnet.json` | Devnet genesis (2 validators, low stakes) |
| `chainspec/testnet.json` | Testnet genesis (3 validators, larger distribution) |
| `crates/chainspec/src/genesis.rs` | `Genesis`, `GenesisExecutor`, and state-root computation |

---

## Related Documentation

- [`docs/release.md`](release.md) — Genesis creation checklist for mainnet
- [`docs/rpc.md`](rpc.md) — RPC endpoints that interact with genesis-derived state
- Spec section 16 — Full protocol specification of genesis initialization
