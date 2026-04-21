# Callchain Bridge Layer

## Overview

The Bridge Layer (`crates/bridge`) manages asset flow between Callchain and external chains (primarily Ethereum). It supports:

- **Internal bridge:** Deposit/withdraw between Protocol Payment Layer and EVM Contract Layer (within Callchain)
- **External bridge:** Cross-chain deposit/withdraw with validator multi-signature attestation

Bridge operations are high-value, high-risk transactions that require robust replay protection, rate limiting, and challenge periods to mitigate compromise scenarios.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Internal Bridge (Protocol ↔ EVM)                          │
│                                                             │
│  DepositToEvm       → deduct protocol balance               │
│                     → mint wrapped tokens in EVM            │
│                                                             │
│  WithdrawToProtocol → burn wrapped tokens in EVM            │
│                     → credit protocol balance               │
│                                                             │
├─────────────────────────────────────────────────────────────┤
│  External Bridge (Callchain ↔ Ethereum)                    │
│                                                             │
│  External Deposit:                                          │
│    1. User locks assets on Ethereum                         │
│    2. Validators observe and sign attestation              │
│    3. Deposit queued in challenge period (~7 days)         │
│    4. After challenge period → mint on Callchain           │
│                                                             │
│  External Withdrawal:                                       │
│    1. User burns assets on Callchain                        │
│    2. Validators sign release attestation                  │
│    3. Assets released on Ethereum                          │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐ │
│  │ BridgeStateManager │  │ BridgeConfig                 │ │
│  │ - pending_ops      │  │ - max_per_tx                 │ │
│  │ - daily_usage      │  │ - daily_limit_per_asset      │ │
│  │ - paused_assets    │  │ - min_validator_signatures   │ │
│  │ - processed_external_txs │ - challenge_period_blocks │ │
│  │ - pending_external_deposits│ - max_external_withdraw │ │
│  │ - bridge_events    │  │ - blocks_per_day             │ │
│  │ - external_paused  │  │ - processed_tx_retention     │ │
│  └────────────────────┘  └──────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Bridge Operations (`lib.rs`)

```rust
pub enum BridgeOp {
    DepositToEvm { asset_id, from, to, amount },
    WithdrawToProtocol { asset_id, from, to, amount },
}
```

Bridge operations are included in the dedicated `bridge_operations` field of `Block`. Execution happens atomically during `Block::execute` (Step 3, after EVM and protocol transactions):

- `DepositToEvm`: `call_bridge::execute_deposit` deducts the sender's protocol balance, then calls `bridgeMint` on the wrapped ERC-20 contract. If the EVM call reverts, the protocol balance is restored from a snapshot (atomic rollback).
- `WithdrawToProtocol`: `call_bridge::execute_withdraw` calls `bridgeBurn` on the wrapped ERC-20 contract, then credits the recipient's protocol balance.

Both operations look up the token contract address from `AssetRegistry::evm_contract_address`. If no contract is registered, the operation is skipped. The bridge crate tracks state and enforces per-tx and daily limits.

### 2. BridgeStateManager (`lib.rs`)

Tracks:
- **Pending ops:** Operations submitted but not yet finalized
- **Daily usage:** Per-asset cumulative volume (auto-resets per `blocks_per_day`)
- **Paused assets:** Emergency pause list
- **Processed external txs:** Replay protection for cross-chain txs
- **Pending external deposits:** Deposits in challenge period
- **External withdrawals per period:** Per-challenge-period volume tracking

**Rate limiting:**
- Per-transaction maximum
- Daily limit per asset
- External withdrawal limit per challenge period

**Challenge period:** Default 10,080 blocks (~7 days at 1 block/min). During this period, anyone can revoke a suspicious deposit by providing proof of fraud (e.g., source chain reorganization).

**Daily usage auto-reset:** `check_and_update_daily_limit` now takes `current_block` and `blocks_per_day` (default ~1 day at 250ms block time). When a new day starts, `daily_usage` automatically clears.

**Challenge period auto-finalization:** `Block::execute` calls `bridge_state.on_block_finalized()` after system transactions, which automatically finalizes pending deposits past the challenge period and credits protocol balances.

**Processed tx pruning:** `processed_external_txs` is now `HashMap<B256, u64>` mapping tx_hash → processed_at_block. Old entries beyond `processed_tx_retention_blocks` (default ~14 days) are pruned automatically during block finalization.

### 3. AssetRegistry Contract Address (`registry.rs`)

Each bridged asset stores its deployed wrapped ERC-20 contract address in `AssetRegistry::evm_contract_address`. This is set after `EvmExecutor::deploy_erc20_template` succeeds:

```rust
registry.set_evm_contract_address(asset_id, contract_addr);
```

`Block::execute` looks up this address when processing `BridgeOp::DepositToEvm` or `BridgeOp::WithdrawToProtocol`. If no contract is registered, the operation is skipped (graceful degradation).

### 4. BridgeConfig (`lib.rs`)

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `max_per_tx` | 1,000 tokens | Maximum single transaction |
| `daily_limit_per_asset` | 10,000 tokens | Daily volume cap |
| `eth_min_confirmations` | 12 | Ethereum block confirmations |
| `signature_timeout_secs` | 300 | Validator signature deadline |
| `min_validator_signatures` | 14 | 2/3 of 21 validators |
| `challenge_period_blocks` | 10,080 | ~7 days challenge period |
| `max_external_withdraw_per_period` | 5,000 tokens | Blast radius limit |
| `blocks_per_day` | 345,600 | ~1 day at 250ms block time (daily limit reset cadence) |
| `processed_tx_retention_blocks` | 4,838,400 | ~14 days at 250ms block time (replay protection pruning) |
| `authorized_contracts` | chain_id → `[Address]` | Authorized Ethereum-side bridge contracts |

**Production ready:** Configuration values are reasonable for a production system. The challenge period and multi-sig threshold provide good security margins.

### 5. External Bridge Flow (`external.rs`)

The external bridge requires:
1. **Validator multi-signature** attestation (14 of 21) — `verify_bridge_signatures` recovers secp256k1 signers and validates against the validator set
2. **Bridge contract registry** — deposits must originate from authorized Ethereum-side contracts (`BridgeContractRegistry` / `BridgeConfig::authorized_contracts`)
3. **Challenge period** for dispute resolution — deposits are queued, then finalized after expiration
4. **Bridge event indexing** — all deposits, withdrawals, finalizations, and challenges are recorded as `BridgeEvent` for audit
5. **Global external pause** — `external_paused` flag stops all external deposits/withdrawals in emergencies
6. **Bridge fee collection** — fees are deducted from deposits (`net = amount - fee`) and added to withdrawals (`total = amount + fee`)
7. **Merkle Patricia Trie (MPT) proof** verification of Ethereum state — available via the `light-client-bridge` feature (`process_light_client_deposit`)

### 6. Deposit/Withdraw (`deposit.rs`, `withdraw.rs`)

These modules handle the internal bridge between Protocol and EVM layers.

**Implementation:** `deposit.rs` and `withdraw.rs` execute full lock/mint/burn logic:

| Step | Deposit (`DepositToEvm`) | Withdraw (`WithdrawToProtocol`) |
|------|--------------------------|--------------------------------|
| 1 | Validate asset registered | Validate asset registered |
| 2 | Check bridge not paused | Check bridge not paused |
| 3 | Check per-tx limit | Check per-tx limit |
| 4 | Check daily limit | Check daily limit |
| 5 | Deduct protocol balance | Check EVM balance sufficient |
| 6 | Call `bridgeMint` on wrapped ERC-20 | Call `bridgeBurn` on wrapped ERC-20 |
| 7 | — | Credit protocol balance |
| 8 | Record completed deposit | Record completed withdrawal |

**Atomic rollback:** `execute_deposit` takes a snapshot of `protocol_balances` before deduction. If the EVM `bridgeMint` reverts or fails, the snapshot is restored and no balance change occurs. `execute_withdraw` burns first; if the burn succeeds, protocol balance is credited.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `BridgeOp`, `BridgeStateManager`, `BridgeConfig`, `PendingExternalDeposit`, `BridgeEvent`, `BridgeEventType`, `BridgeContractRegistry` |
| `deposit.rs` | Internal bridge deposit (protocol lock + EVM mint) |
| `withdraw.rs` | Internal bridge withdraw (EVM burn + protocol credit) |
| `external.rs` | External cross-chain bridge (`ExternalBridgeOp`, `BridgeSignature`, `BridgeDepositProof`, `verify_bridge_signatures`, MPT proof) |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Bridge state tracking | 🟢 Ready | Daily limits auto-reset per `blocks_per_day`; replay protection with auto-pruning; pause/unpause; event indexing |
| Rate limiting | 🟢 Ready | Per-tx, daily, and per-period limits enforced automatically with auto-reset |
| Challenge period | 🟢 Ready | Deposits auto-finalize during `Block::execute` via `on_block_finalized`; permissionless challenge/revoke during period |
| Signature validation | 🟢 Ready | secp256k1 signature verification with 14-of-21 validator threshold; `verify_bridge_signatures` enforces distinct validators and set membership |
| Internal bridge | 🟢 Ready | Deposit/withdraw execute atomically via `Block::execute` with EVM rollback on revert. Requires wrapped token contract deployment + `AssetRegistry::evm_contract_address` registration. |
| External bridge | 🟢 Ready | Contract registry, signature validation, event indexing, global pause, fee collection, auto-finalization, auto-daily-reset, and tx hash pruning all implemented. MPT proof verification available via `light-client-bridge` feature. |

---

## Production Readiness Gaps

All documented gaps have been resolved.

| # | Fix | Details |
|---|-----|---------|
| 1 | **Daily usage auto-reset** | `check_and_update_daily_limit` now takes `current_block` and `blocks_per_day`. When `current_block >= daily_usage_reset_at + blocks_per_day`, usage auto-clears. No manual intervention needed. |
| 2 | **Challenge period auto-finalization** | `Block::execute` Step 4.5 calls `bridge_state.on_block_finalized(current_block, challenge_period, retention)`, which automatically finalizes pending deposits past the challenge period and credits protocol balances. |
| 3 | **Processed tx hash pruning** | `processed_external_txs` changed from `HashSet<B256>` to `HashMap<B256, u64>` (tx_hash → processed_at_block). `prune_processed_external_txs(before_block)` removes entries older than `processed_tx_retention_blocks` (default ~14 days). Called automatically during block finalization. |
| 4 | **Validator signature validation** | `verify_bridge_signatures` in `external.rs` validates secp256k1 signatures, recovers signers, enforces distinct validators, and checks validator set membership. |
| 5 | **MPT proof verification** | Available via `light-client-bridge` feature in `process_light_client_deposit`. Validates tx inclusion and receipt proofs against Ethereum headers. |
| 6 | **Validator signature scheme** | `bridge_event_hash` defines the signed payload; `sign_bridge_event` produces secp256k1 signatures; 14-of-21 threshold enforced. |
| 7 | **Bridge contract registry** | `BridgeContractRegistry` and `BridgeConfig::authorized_contracts` manage authorized Ethereum-side bridge contracts. `verify_bridge_contract` rejects unauthorized sources. |
| 8 | **`BridgeDeposit` proof validation** | `BridgeDepositProof` struct embeds in legacy `BridgeDeposit` instruction's `proof` field; parsed and validated during `Block::execute` with full signature verification. |
| 9 | **Bridge event indexing** | `BridgeEvent` records all external deposits, withdrawals, finalizations, and challenges with block height for audit and verification. |
| 10 | **Global external bridge pause** | `BridgeStateManager::external_paused` with `pause_external_bridge` / `resume_external_bridge` stops all external deposits/withdrawals. |
| 11 | **Bridge fee collection** | Fees deducted from deposits (`net = amount - fee`) and added to withdrawals (`total = amount + fee`); tracked in `total_fees_collected`. |

---

## Test Status

- `cargo test -p call-bridge` — 29 unit tests covering state tracking, daily limits (with auto-reset), pause/unpause, config defaults, operation accessors, signature validation (14-of-21), deposit/withdraw with deployed ERC-20 contract, challenge period flow (queue/finalize/revoke), period withdrawal limits, processed tx pruning
- `cargo test -p call-consensus` — `test_block_execution_order` verifies bridge ops execute atomically during block processing (deploy contract → deposit → verify protocol balance deduction)
- `cargo test -p call-payload-builder` — `test_payload_execution_order` verifies bridge ops are included and executed correctly during payload construction
- `cargo test -p call-protocol --test test_bridge_flow` — integration tests covering external deposit end-to-end, withdrawal end-to-end, daily limit enforcement, insufficient signatures, duplicate validator rejection, asset allowlisting, insufficient balance, chain IDs, event hash determinism
