# Callchain Agent Payment Layer

## Overview

The Agent Layer (`crates/agent`) enables delegated balance management and payments on behalf of users. An agent is a registered entity (e.g., a dApp, service, or automated wallet) that operates within owner-defined constraints.

**Key features:**
- Agent registration with metadata
- Agent-specific balance management (owner-funded sub-accounts)
- Agent-mediated payments and batch payments
- Basic per-transaction limits and asset allowlists

## Precompile Alternative

The **Agent precompile at `0x209`** exposes agent operations via standard EVM transactions:

| Operation | Function | Gas |
|---|---|---|
| Register agent | `registerAgent(string,string,address)` | 6,000 + storage |
| Grant balance | `grantBalance(uint64,uint64,uint128)` | 6,000 + storage |
| Revoke balance | `revokeBalance(uint64,uint64)` | 6,000 + storage |
| Pay | `pay(uint64,address,uint128)` | 30,000 + storage |
| Batch pay | `batchPay(uint64,address[],uint128[])` | 30,000 + storage |
| Create session | `createSession(address,uint128,uint128,uint64,uint128,uint64,uint64,uint64,uint64[],address[])` | 10,000 + storage |
| Revoke session | `revokeSession(uint64)` | 6,000 + storage |
| Is session valid | `isSessionValid(uint64)` | 2,000 + storage |
| Execute session | `executeSession(uint64,uint64,address,uint128)` | 30,000 + storage |

Gas is dynamically metered: `gas_used = base_gas + sloads*50 + sstores*500`.

See [precompile.md](precompile.md) for the full ABI.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Agent Payment Layer                                         │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ AgentStorage<B: StorageBackend>                      │  │
│  │                                                      │  │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  │
│  │  │ AgentRegistry│  │ AgentBalances│  │  Sessions   │  │  Reverse    │  │
│  │  │ - agents    │  │ - (owner,id,│  │ - global id │  │  Index      │  │
│  │  │ - by_owner  │  │   asset)→amt│  │ - owner     │  │  (address   │  │
│  │  │ - by_name   │  │ - grant     │  │ - delegate  │  │   → id)     │  │
│  │  │ - by_addr   │  │ - revoke    │  │ - limits    │  │             │  │
│  │  │             │  │ - deduct    │  │ - spent     │  │             │  │
│  │  └─────────────┘  └─────────────┘  └─────────────┘  └─────────────┘  │
│  │                                                      │  │
│  │  Reads / writes EVM storage slots under AGENT_ADDRESS │  │
│  │  (0x209) via StorageRef                               │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Agent Storage (`lib.rs`, `precompile.rs`)

`AgentStorage<B: StorageBackend>` is the single struct that manages all agent state. It reads and writes EVM storage slots under the agent precompile address (`0x209`) using `StorageRef`.

Implemented operations:
- `register_agent(owner, name, url, agent_address)` — registers a new agent, returns `agent_id`
- `grant_balance(owner, agent_id, asset_id, amount)` — credits agent balance, deducts from owner
- `revoke_balance(owner, agent_id, asset_id)` — revokes all agent balance for an asset
- `pay(asset_id, recipient, amount)` — agent address pays from its balance to recipient (caller must be registered `agent_address`)
- `batch_pay(asset_id, recipients[], amounts[])` — agent address batch payment from its balance (caller must be registered `agent_address`)
- `revoke_agent(owner, agent_id)` — deregisters an agent

All operations are caller-authenticated via `msg.sender`. `pay` and `batchPay` look up the agent via reverse index from `msg.sender` and can only be called by the registered `agent_address`, not the owner.

### 2. Agent Registration

`AgentRegistration` fields:
- `agent_id`: auto-incremented unique ID
- `owner`: Address that controls the agent
- `agent_address`: EOA address authorized to perform `pay` and `batchPay` on behalf of the agent
- `name`, `url`: descriptive metadata
- `registered_at`: Block number of registration

`AgentStorage` supports:
- Register by owner with **name and `agent_address` uniqueness enforcement** (one agent per address)
- Lookup by ID, name, owner, or `agent_address` (reverse index)
- Agent revocation (clears reverse index)

### 3. Agent Permissions

The actual implementation enforces only basic limits:

| Field | Default | Behavior |
|-------|---------|----------|
| `allowed_assets` | `[1]` (only CALL) | Asset whitelist |
| `per_tx_limit` | `1_000` | Per-transaction cap |

There is no `daily_limit`, `allowed_counterparties`, `allowed_protocols`, `expires_at`, or per-precompile permission checking.

### 3a. Session Keys

Session keys allow any address (owner) to delegate limited, time-bound spending authority to an external address (delegate). The delegate can then initiate transfers from the owner's balance without requiring the owner to sign every transaction. Sessions are independent of agents — they operate directly on the owner's funds.

#### Session Lifecycle

1. **Create**: Owner calls `createSession(delegate, perTxLimit, dailyLimit, expiresAt, maxTotalSpend, minIntervalBlocks, effectiveAt, maxExecutions, allowedAssets, allowedRecipients)` → returns `sessionId`
2. **Execute**: Delegate calls `executeSession(sessionId, assetId, to, amount)` → transfers from owner balance to recipient
3. **Revoke**: Owner calls `revokeSession(sessionId)` → immediately invalidates the session
4. **Auto-expire**: Sessions automatically become invalid after `expiresAt` block

#### Session Constraints

| Field | Type | Description | Optional |
|-------|------|-------------|----------|
| `delegate` | `address` | The EOA address authorized to execute on behalf of the owner | No |
| `perTxLimit` | `uint128` | Maximum amount per single `executeSession` | No |
| `dailyLimit` | `uint128` | Maximum cumulative amount per day (~17,280 blocks) | No |
| `expiresAt` | `uint64` | Block number after which the session is invalid (0 = never) | Yes |
| `maxTotalSpend` | `uint128` | Lifetime cumulative spending cap (0 = unlimited) | Yes |
| `minIntervalBlocks` | `uint64` | Minimum blocks between two executions (0 = no limit) | Yes |
| `effectiveAt` | `uint64` | Block number before which the session is inactive (0 = immediate) | Yes |
| `maxExecutions` | `uint64` | Maximum number of `executeSession` calls (0 = unlimited) | Yes |
| `allowedAssets` | `uint64[]` | Whitelist of permitted asset IDs (empty = any asset) | Yes |
| `allowedRecipients` | `address[]` | Whitelist of permitted recipient addresses (empty = any address) | Yes |

All optional constraints are disabled when set to `0` or an empty array. The `executeSession` validation chain checks constraints in this order: existence → delegate → expiresAt → effectiveAt → minIntervalBlocks → maxExecutions → perTxLimit → dailyLimit → maxTotalSpend → allowedAssets → allowedRecipients.

#### Storage Layout

Session data is stored under `AGENT_ADDRESS (0x209)` with globally unique session IDs:

```
slot_session_count()                         → uint64
slot_session_owner(session_id)               → address
slot_session_delegate(session_id)            → address
slot_session_limits(session_id)              → packed(perTxLimit, dailyLimit)
slot_session_expires(session_id)             → uint64
slot_session_spent(session_id)               → uint128
slot_session_last_day(session_id)            → uint64
slot_session_allowed_assets_count(session_id) → uint64
slot_session_allowed_asset(session_id, i)    → uint64
slot_session_allowed_recipients_count(session_id) → uint64
slot_session_allowed_recipient(session_id, i) → address
slot_session_max_total_spend(session_id)     → uint128
slot_session_min_interval_blocks(session_id) → uint64
slot_session_effective_at(session_id)        → uint64
slot_session_max_executions(session_id)      → uint64
slot_session_execution_count(session_id)     → uint64
slot_session_last_execution_block(session_id) → uint64
slot_session_total_spent(session_id)         → uint128
```

#### Gas

| Method | Base Gas |
|--------|----------|
| `createSession` | 10,000 |
| `revokeSession` | 6,000 |
| `isSessionValid` | 2,000 |
| `executeSession` | 30,000 |

Note: Gas is paid by the delegate (EVM `msg.sender`) when calling `executeSession`. The transferred amount is deducted from the owner's balance, not the delegate's.

### 4. Agent Balances

Agent balances are stored as EVM storage slots under `0x209`:

- `grant_balance()`: Deducts from owner's EVM balance, credits agent sub-account
- `revoke_balance()`: Removes all balance for an agent/asset
- `deduct()`: Subtracts with underflow check
- `credit()`: Adds with `checked_add` overflow protection

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `AgentStorage`, `AgentError`, `AgentRegistration`, balance and nonce helpers, session key storage |
| `precompile.rs` | Precompile dispatch for `0x209`, selector decoding, EVM storage integration, session key dispatch |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Agent registration | 🟢 Ready | Name uniqueness, metadata storage, agent revocation |
| Permissions | 🟡 Partial | Basic per-tx limit and asset allowlist only |
| Session keys | 🟢 Ready | Create/revoke/validate/execute with 10 optional/permanent constraints |
| Balance management | 🟢 Ready | Grant deducts from owner, overflow-protected credit, underflow-protected deduct |
| Payments | 🟢 Ready | Single and batch pay from agent balance, session-key delegated transfers |
| Persistence | 🟢 Ready | All state in EVM storage under `0x209`, committed with EVM state root |

---

## Test Status

- `cargo test -p call-agent` — unit tests covering registration, balance operations, nonce tracking, permission checks, precompile call extraction, agent pay/batch pay
