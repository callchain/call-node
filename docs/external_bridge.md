# Callchain External Bridge

## Overview

The External Bridge (`crates/bridge`) manages cross-chain asset flow between Callchain and external chains (primarily Ethereum). It is a high-value, high-risk system that requires robust replay protection, rate limiting, and challenge periods to mitigate compromise scenarios.

**Supported flows:**

- **External Deposit:** User locks assets on Ethereum → validators observe and attest → deposit queued in challenge period → finalized and minted on Callchain
- **External Withdrawal:** User burns assets on Callchain → validators sign release attestation → assets released on Ethereum

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
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

## Precompile Alternative

The **Bridge precompile at `0x103`** exposes external bridge operations via standard EVM transactions:

| Operation | Precompile Function | Gas |
|---|---|---|
| External Deposit | `externalBridgeDeposit(bytes32,uint8,uint64,bytes,address,uint64,uint128,bytes)` | 50,000 |
| External Withdraw | `externalBridgeWithdraw(uint8,bytes,uint64,uint128)` | 30,000 |
| Challenge | `challengeBridgeDeposit(bytes32,bytes)` | 20,000 |

See [precompile.md](precompile.md) for the full ABI.

## Key Components

### 1. Bridge Operations (`lib.rs`)

```rust
pub enum ExternalBridgeOp {
    Deposit {
        source_chain,
        source_tx_hash,
        source_block_number,
        sender,
        recipient,
        asset_id,
        amount,
        signatures,
    },
    Withdraw {
        target_chain,
        target_address,
        asset_id,
        sender,
        amount,
    },
    LightClientDeposit {
        source_chain,
        header,
        tx_proof,
        receipt_proof,
        recipient,
        asset_id,
        amount,
    },
}
```

Bridge operations are included in the `bridge_operations` field of `Block` or submitted as `ExternalBridgeDeposit` / `ExternalBridgeWithdraw` instructions within `ProtocolTransaction`. Execution happens atomically during `Block::execute`.

### 2. BridgeStateManager (`lib.rs`)

Tracks:
- **Pending ops:** Operations submitted but not yet finalized
- **Daily usage:** Per-asset cumulative volume (auto-resets per `blocks_per_day`)
- **Paused assets:** Emergency pause list
- **Processed external txs:** Replay protection for cross-chain txs (`HashMap<B256, u64>` mapping tx_hash → processed_at_block)
- **Pending external deposits:** Deposits in challenge period
- **External withdrawals per period:** Per-challenge-period volume tracking
- **Bridge events:** All deposits, withdrawals, finalizations, and challenges recorded as `BridgeEvent` for audit

**Rate limiting:**
- Per-transaction maximum
- Daily limit per asset
- External withdrawal limit per challenge period

**Challenge period:** Default 10,080 blocks (~7 days at 1 block/min). During this period, anyone can revoke a suspicious deposit by providing proof of fraud (e.g., source chain reorganization).

**Daily usage auto-reset:** `check_and_update_daily_limit` takes `current_block` and `blocks_per_day`. When a new day starts, `daily_usage` automatically clears.

**Challenge period auto-finalization:** `Block::execute` calls `bridge_state.on_block_finalized()` after system transactions, which automatically finalizes pending deposits past the challenge period and credits protocol balances.

**Processed tx pruning:** Old entries beyond `processed_tx_retention_blocks` (default ~14 days) are pruned automatically during block finalization.

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
| `blocks_per_day` | 345,600 | ~1 day at 250ms block time |
| `processed_tx_retention_blocks` | 4,838,400 | ~14 days replay protection pruning |
| `authorized_contracts` | chain_id → `[Address]` | Authorized Ethereum-side bridge contracts |

### 4. External Bridge Flow (`external.rs`)

The external bridge requires:

1. **Validator multi-signature** attestation (14 of 21) — `verify_bridge_signatures` recovers secp256k1 signers and validates against the validator set
2. **Bridge contract registry** — deposits must originate from authorized Ethereum-side contracts (`BridgeContractRegistry` / `BridgeConfig::authorized_contracts`)
3. **Challenge period** for dispute resolution — deposits are queued, then finalized after expiration
4. **Bridge event indexing** — all deposits, withdrawals, finalizations, and challenges are recorded as `BridgeEvent` for audit
5. **Global external pause** — `external_paused` flag stops all external deposits/withdrawals in emergencies
6. **Bridge fee collection** — fees are deducted from deposits (`net = amount - fee`) and added to withdrawals (`total = amount + fee`)
7. **Merkle Patricia Trie (MPT) proof** verification of Ethereum state — available via the `light-client-bridge` feature (`process_light_client_deposit`)

### 5. Signature Validation

`verify_bridge_signatures` in `external.rs` validates secp256k1 signatures, recovers signers, enforces distinct validators, and checks validator set membership. The signed payload is defined by `bridge_event_hash` and produced by `sign_bridge_event`.

### 6. Bridge Contract Registry

`BridgeContractRegistry` and `BridgeConfig::authorized_contracts` manage authorized Ethereum-side bridge contracts. `verify_bridge_contract` rejects unauthorized sources.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `BridgeOp`, `ExternalBridgeOp`, `BridgeStateManager`, `BridgeConfig`, `PendingExternalDeposit`, `BridgeEvent`, `BridgeEventType`, `BridgeContractRegistry` |
| `external.rs` | External cross-chain bridge (`ExternalBridgeOp`, `BridgeSignature`, `BridgeDepositProof`, `verify_bridge_signatures`, MPT proof) |
| `deposit.rs` | Internal bridge deposit (also used for external deposit finalization) |
| `withdraw.rs` | Internal bridge withdraw (also used for external withdrawal initiation) |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Bridge state tracking | Ready | Daily limits auto-reset; replay protection with auto-pruning; pause/unpause; event indexing |
| Rate limiting | Ready | Per-tx, daily, and per-period limits enforced automatically with auto-reset |
| Challenge period | Ready | Deposits auto-finalize during `Block::execute` via `on_block_finalized`; permissionless challenge/revoke during period |
| Signature validation | Ready | secp256k1 signature verification with 14-of-21 validator threshold; `verify_bridge_signatures` enforces distinct validators and set membership |
| External bridge | Ready | Contract registry, signature validation, event indexing, global pause, fee collection, auto-finalization, auto-daily-reset, and tx hash pruning all implemented. MPT proof verification available via `light-client-bridge` feature. |

---

## Test Status

- `cargo test -p call-bridge` — 29 unit tests covering state tracking, daily limits (with auto-reset), pause/unpause, config defaults, operation accessors, signature validation (14-of-21), deposit/withdraw with deployed ERC-20 contract, challenge period flow (queue/finalize/revoke), period withdrawal limits, processed tx pruning
- `cargo test -p call-consensus` — `test_block_execution_order` verifies bridge ops execute atomically during block processing
- `cargo test -p call-payload-builder` — `test_payload_execution_order` verifies bridge ops are included and executed correctly during payload construction
- `cargo test -p call-protocol --test test_bridge_flow` — integration tests covering external deposit end-to-end, withdrawal end-to-end, daily limit enforcement, insufficient signatures, duplicate validator rejection, asset allowlisting, insufficient balance, chain IDs, event hash determinism
