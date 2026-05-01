# Callchain Agent Payment Layer

## Overview

The Agent Layer (`crates/agent`) enables delegated transaction execution on behalf of users. An agent is a registered entity (e.g., a dApp, service, or automated wallet) that can sign and execute protocol transactions within owner-defined constraints.

**Key features:**
- Agent registration with optional domain verification
- Per-agent permissions (asset whitelist, counterparty restrictions, daily/tx limits)
- Agent-specific balance management (owner-funded sub-accounts)
- Agent transaction verification with dual-signature support (agent + owner)
- 0.5x gas discount for agent-mediated transactions

## Precompile Alternative

The **Agent precompile at `0x209`** exposes agent operations via standard EVM transactions:

| Operation | Function | Gas |
|---|---|---|
| Register agent | `register(bytes,string,string)` | 10,000 |
| Grant balance | `grant(uint64,uint64,uint128)` | 10,000 |
| Revoke balance | `revoke(uint64,uint64)` | 10,000 |

See [precompile.md](precompile.md) for the full ABI.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Agent Payment Layer                                         │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ AgentRegistry      │  │ AgentBalances                │  │
│  │ - agents: id→reg   │  │ - (owner, id, asset)→amount │  │
│  │ - by_owner         │  │ - grant / revoke / deduct   │  │
│  │ - by_name          │  │                              │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ AgentPermissions   │  │ AgentNonces                  │  │
│  │ - allowed_assets   │  │ - (owner, id)→nonce          │  │
│  │ - daily_limit      │  │ - check_and_increment        │  │
│  │ - per_tx_limit     │  └──────────────────────────────┘  │
│  │ - allowed_counter- │                                     │
│  │   parties          │  ┌──────────────────────────────┐  │
│  │ - expires_at       │  │ AgentTxContext               │  │
│  └────────────────────┘  │ - agent_id, nonce, sig       │  │
│                          │ - owner_public_key             │  │
│  ┌────────────────────┐  └──────────────────────────────┘  │
│  │ AgentFeeConfig     │  ┌──────────────────────────────┐  │
│  │ - fee_payer        │  │ AgentEvents                  │  │
│  │ - require_owner_   │  │ - event_type, agent_id       │  │
│  │   signature_above  │  │ - asset_id, amount           │  │
│  └────────────────────┘  │ - block_height               │  │
│                          └──────────────────────────────┘  │
│                          verify_agent_tx (5-step)          │
│                          execute_agent_tx (0.5x gas)       │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Agent Registration (`registry.rs`)

`AgentRegistration` fields:
- `agent_id`: auto-incremented unique ID
- `owner`: Address that controls the agent
- `agent_public_key`: secp256k1 public key for agent signatures
- `name`, `url`, `metadata_hash`: descriptive metadata
- `domain_proof`: Optional DNS TXT or HTTP file verification
- `domain_verified`: Whether domain proof passed validation
- `registered_at`: Block number of registration

`AgentRegistry` supports:
- Register by owner with name uniqueness enforcement
- Lookup by ID, name, or owner
- Update config (name, URL, metadata)
- Update domain proof


### 2. Agent Permissions (`permissions.rs`)

`AgentPermissions` controls what an agent can do:

| Field | Default | Behavior |
|-------|---------|----------|
| `allowed_assets` | `[1]` (only CALL) | Asset whitelist |
| `daily_limit` | `10_000` | Daily cumulative amount |
| `per_tx_limit` | `1_000` | Per-transaction cap |
| `allowed_counterparties` | `[]` (empty = all) | Recipient whitelist |
| `allowed_protocols` | `[]` (empty = none) | EVM contract whitelist |
| `expires_at` | `0` (never) | Permission expiry block |

`verify_agent_permissions()` checks:
1. Expiration (block-number based)
2. Asset allowed
3. Counterparty allowed
4. Per-tx limit
5. Daily limit (with auto-reset every 86,400,000 ms = 24 hours, timestamp-based)
6. Owner daily fee limit


### 3. Agent Balances (`balances.rs`)

`AgentBalances`: `HashMap<(owner, agent_id, asset_id), u128>`

- `grant_funds()`: Deducts from owner's protocol balance, then credits agent
- `top_up()`: Same as grant (semantic alias)
- `revoke_funds()`: Removes all balance for an agent, returns amount
- `deduct()`: Subtracts with underflow check
- `credit()`: Adds with `checked_add` overflow protection


### 4. Agent Transaction Verification (`executor.rs`)

`verify_agent_tx()` performs 5-step validation:
1. Agent signature verification (secp256k1)
2. Nonce check (sequential, no gaps)
3. Per-precompile permission checks (all payments in a batch)
4. Expiry check (uses `protocol_tx.expires_at` as block deadline)
5. Owner signature threshold for large amounts

`execute_agent_tx()`:
1. Calculates gas with 0.5x discount
2. Deducts fee from agent balance using the transaction's `fee_currency`
3. Executes precompile calls


### 5. Agent Activity Audit Trail (`lib.rs`, `block.rs`)

`AgentEventType` enum:
- `AgentPay`, `AgentBatchPay`, `AgentCall`, `AgentBridgeDeposit`
- `AgentRegistered`, `AgentRevoked`

`AgentEvent` struct:
- `event_type`, `agent_id`, `tx_hash`, `asset_id`, `amount`, `recipient`, `block_height`

Emitted during `Block::execute` by `execute_agent_precompile()` for every successful agent precompile call. Included in `BlockExecutionResult::agent_events` and hashed into `compute_receipt_root()`.


---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `AgentError`, `AgentFeeConfig`, `FeePayer`, `SignedAgentTx`, `DomainProof`, `AgentEvent`, `AgentEventType` |
| `registry.rs` | `AgentRegistration`, `AgentRegistry`, domain proof handling |
| `permissions.rs` | `AgentPermissions`, `AgentDailyUsage`, permission verification |
| `balances.rs` | `AgentBalances`, `AgentNonces` |
| `executor.rs` | `verify_agent_tx()`, `execute_agent_tx()`, precompile helpers |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Agent registration | 🟢 Ready | Name uniqueness, metadata storage, real DNS/HTTP domain verification, agent revocation, configurable registration fee |
| Permissions | 🟢 Ready | Restrictive defaults, full per-precompile checking including all batch payments |
| Balance management | 🟢 Ready | Grant deducts from owner, overflow-protected credit, underflow-protected deduct |
| Transaction verification | 🟢 Ready | 5-step validation with independent `expires_at` field, no longer conflates fee with time |
| Transaction execution | 🟢 Ready | Proper EVM call execution, wired into block production with inline permission checks, fee_currency-aware gas deduction |
| Persistence | 🟢 Ready | AgentRegistry, AgentBalances, and AgentNonces all persisted to MDBX |

---

## Test Status

- `cargo test -p call-agent` — 46 unit tests covering registration (including fee deduction, insufficient balance rejection), domain proof format, balance operations (grant deducts from owner, overflow protection), nonce tracking, permission checks, precompile call extraction (including batch transfer multi-payment), agent pay/batch pay, bridge deposit failure recovery, tx hash determinism
- `cargo test -p call-consensus` — block execution order test verifies `AgentPay` / `AgentBatchPay` / `AgentCall` / `AgentBridgeDeposit` execute correctly during `Block::execute`; `test_agent_precompile_emits_event` verifies `AgentEvent` emission and receipt root inclusion; `test_expired_transaction_rejected` verifies `expires_at` enforcement
- `cargo test -p call-node --lib` — node startup and state persistence tests verify agent registry, balances, and nonces are saved/loaded to MDBX correctly
- `cargo test -p call-protocol --test test_agent_flow` — integration tests covering registration, domain proof, balance operations, nonce sequential/stale rejection
