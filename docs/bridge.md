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
│  DepositToEvm ──────┐  → lock protocol balance             │
│  WithdrawToProtocol ┘  → mint wrapped tokens in EVM        │
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

Bridge operations are included in blocks as `SystemTx` or protocol instructions. The actual execution (locking/minting/burning) happens in the protocol and EVM layers; the bridge crate tracks state and enforces limits.

### 2. BridgeStateManager (`lib.rs`)

Tracks:
- **Pending ops:** Operations submitted but not yet finalized
- **Daily usage:** Per-asset cumulative volume (resets manually)
- **Paused assets:** Emergency pause list
- **Processed external txs:** Replay protection for cross-chain txs
- **Pending external deposits:** Deposits in challenge period
- **External withdrawals per period:** Per-challenge-period volume tracking

**Rate limiting:**
- Per-transaction maximum
- Daily limit per asset
- External withdrawal limit per challenge period

**Challenge period:** Default 10,080 blocks (~7 days at 1 block/min). During this period, anyone can revoke a suspicious deposit by providing proof of fraud (e.g., source chain reorganization).

**Gap #1 — Daily usage never auto-resets:** `reset_daily_usage()` exists but is never called automatically. Daily limits are effectively cumulative until manually reset.

**Gap #2 — No automatic challenge period advancement:** `finalize_pending_external_deposits()` must be called explicitly with the current block number. If not called (e.g., node restart, missed block), deposits remain pending indefinitely.

**Gap #3 — `processed_external_txs` is an unbounded HashSet:** Every external transaction hash is stored forever. This will grow without bound and consume unbounded memory/storage.

**Gap #4 — No signature validation in bridge crate:** The `BridgeStateManager` tracks signature counts (`signatures_count`) but does not validate signatures. Signature verification is the responsibility of the caller (protocol instruction execution), but the bridge crate has no integration with the validator set or signature schemes.

### 3. BridgeConfig (`lib.rs`)

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `max_per_tx` | 1,000 tokens | Maximum single transaction |
| `daily_limit_per_asset` | 10,000 tokens | Daily volume cap |
| `eth_min_confirmations` | 12 | Ethereum block confirmations |
| `signature_timeout_secs` | 300 | Validator signature deadline |
| `min_validator_signatures` | 14 | 2/3 of 21 validators |
| `challenge_period_blocks` | 10,080 | ~7 days challenge period |
| `max_external_withdraw_per_period` | 5,000 tokens | Blast radius limit |

**Production ready:** Configuration values are reasonable for a production system. The challenge period and multi-sig threshold provide good security margins.

### 4. External Bridge Flow (`external.rs`)

The external bridge requires:
1. **Merkle Patricia Trie (MPT) proof** verification of Ethereum state
2. **Validator multi-signature** attestation (14 of 21)
3. **Challenge period** for dispute resolution

**Gap #5 — MPT proof verification is not implemented:** `BridgeError::MptProofError` exists but the external bridge module does not contain MPT proof verification logic. Ethereum state proofs cannot be validated.

**Gap #6 — No validator signature scheme defined:** The bridge expects 14 validator signatures but there is no defined signature format, aggregation scheme, or verification key management. The `min_validator_signatures` field is tracked but never enforced.

**Gap #7 — No bridge contract address management:** There is no registry of authorized bridge contracts on Ethereum. Any contract could potentially be treated as a valid source.

**Gap #8 — `BridgeDeposit` instruction has empty proof validation:** In `instructions.rs`, the `BridgeDeposit` instruction checks `if proof.is_empty()` but does not validate the proof content. The comment says "actual sig check in bridge layer" but the bridge layer has no signature verification.

### 5. Deposit/Withdraw (`deposit.rs`, `withdraw.rs`)

These modules handle the internal bridge between Protocol and EVM layers.

**Gap #9 — Internal bridge is a no-op:** `deposit.rs` and `withdraw.rs` contain stub implementations. The actual lock/mint/burn operations are not implemented in the bridge crate; they are expected to be handled by EVM precompiles or protocol instructions.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `BridgeOp`, `BridgeStateManager`, `BridgeConfig`, `PendingExternalDeposit` |
| `deposit.rs` | Internal bridge deposit (stub) |
| `withdraw.rs` | Internal bridge withdraw (stub) |
| `external.rs` | External cross-chain bridge (MPT proof, validator sigs) |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Bridge state tracking | 🟡 Partial | Daily limits, pause, replay protection exist but auto-reset missing |
| Rate limiting | 🟡 Partial | Config values are good but enforcement is manual |
| Challenge period | 🟡 Partial | Mechanism exists but requires explicit caller to finalize |
| Signature validation | 🔴 Not ready | No validator signature scheme, no MPT proof verification |
| Internal bridge | 🔴 Not ready | Deposit/withdraw are stubs |
| External bridge | 🔴 Not ready | No contract address registry, no proof validation |

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | **Daily usage never auto-resets** | Medium | `reset_daily_usage()` is never called. Daily limits accumulate indefinitely. |
| 2 | **Challenge period deposits never auto-finalize** | High | Deposits remain pending until `finalize_pending_external_deposits()` is explicitly called. Node restarts or missed blocks can stall deposits. |
| 3 | **`processed_external_txs` grows unbounded** | Medium | Every external tx hash is stored forever. No pruning strategy. |
| 4 | **No validator signature validation** | Critical | `signatures_count` is tracked but signatures are never validated. Anyone can claim a deposit was attested by validators. |
| 5 | **MPT proof verification unimplemented** | Critical | Ethereum state proofs cannot be validated. A fake proof could claim any Ethereum state. |
| 6 | **No validator signature scheme defined** | Critical | No signature format, no aggregation, no key rotation. The "14 of 21" threshold is a number without enforcement. |
| 7 | **No bridge contract registry** | High | No authorized Ethereum contract addresses. Any contract could be a "source." |
| 8 | **`BridgeDeposit` proof is not validated** | High | Only checks `!proof.is_empty()`. Content is never verified. |
| 9 | **Internal bridge is stubbed** | High | `deposit.rs` and `withdraw.rs` contain no actual lock/mint/burn logic. |
| 10 | **No bridge event indexing** | Medium | No mechanism to index and verify Ethereum events (Deposit, Withdrawal) that trigger bridge operations. |
| 11 | **No emergency pause mechanism for external bridge** | Medium | `pause_asset()` exists but only applies to internal bridge operations. External deposits/withdrawals have no global pause. |
| 12 | **No bridge fee collection** | Low | `bridge_fee` is configured but never deducted from bridge operations. |

---

## Test Status

- `cargo test -p call-bridge` — unit tests cover state tracking, daily limits, pause/unpause, config defaults, operation accessors
- Missing: signature validation tests, MPT proof tests, challenge period flow tests, external deposit/withdrawal E2E tests, contract registry tests
