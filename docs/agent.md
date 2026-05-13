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
| Register agent | `registerAgent(string,string,bytes32)` | 6,000 + storage |
| Grant balance | `grantBalance(uint64,uint64,uint128)` | 6,000 + storage |
| Revoke balance | `revokeBalance(uint64,uint64)` | 6,000 + storage |
| Pay | `pay(uint64,uint64,address,uint128)` | 30,000 + storage |
| Batch pay | `batchPay(uint64,uint64,address[],uint128[])` | 30,000 + storage |
| Withdraw balance | `withdrawBalance(uint64,uint64,uint128)` | 50,000 + storage |
| Create session | `createSession(uint64,address,uint128,uint128,uint64)` | 10,000 + storage |
| Revoke session | `revokeSession(uint64,uint64)` | 6,000 + storage |
| Is session valid | `isSessionValid(uint64,uint64)` | 2,000 + storage |
| Execute session transfer | `executeSessionTransfer(uint64,uint64,uint64,address,uint128)` | 30,000 + storage |

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
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  │  │
│  │  │ AgentRegistry│  │ AgentBalances│  │ AgentNonces │  │  │
│  │  │ - agents    │  │ - (owner,id,│  │ - (owner,id)│  │  │
│  │  │ - by_owner  │  │   asset)→amt│  │ → nonce     │  │  │
│  │  │ - by_name   │  │ - grant     │  │ - increment │  │  │
│  │  │             │  │ - revoke    │  │             │  │  │
│  │  │             │  │ - deduct    │  │             │  │  │
│  │  └─────────────┘  └─────────────┘  └─────────────┘  │  │
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
- `register_agent(owner, name, url)` — registers a new agent, returns `agent_id`
- `grant_balance(owner, agent_id, asset_id, amount)` — credits agent balance, deducts from owner
- `revoke_balance(owner, agent_id, asset_id)` — revokes all agent balance for an asset
- `pay(owner, agent_id, asset_id, recipient, amount)` — pays from agent balance to recipient
- `batch_pay(owner, agent_id, asset_id, recipients[], amounts[])` — batch payment from agent balance
- `withdraw_balance(owner, agent_id, asset_id, amount)` — withdraws from agent balance back to owner
- `revoke_agent(owner, agent_id)` — deregisters an agent

Agents are caller-authenticated only (`msg.sender` is the owner). There are no agent-level signatures.

### 2. Agent Registration

`AgentRegistration` fields:
- `agent_id`: auto-incremented unique ID
- `owner`: Address that controls the agent
- `name`, `url`: descriptive metadata
- `registered_at`: Block number of registration

`AgentStorage` supports:
- Register by owner with name uniqueness enforcement
- Lookup by ID, name, or owner
- Agent revocation

### 3. Agent Permissions

The actual implementation enforces only basic limits:

| Field | Default | Behavior |
|-------|---------|----------|
| `allowed_assets` | `[1]` (only CALL) | Asset whitelist |
| `per_tx_limit` | `1_000` | Per-transaction cap |

There is no `daily_limit`, `allowed_counterparties`, `allowed_protocols`, `expires_at`, or per-precompile permission checking.

### 3a. Session Keys

Session keys allow an agent owner to delegate limited, time-bound spending authority to an external address (delegate). The delegate can then initiate transfers from the agent's balance without requiring the owner to sign every transaction.

#### Session Lifecycle

1. **Create**: Owner calls `createSession(agentId, delegate, perTxLimit, dailyLimit, expiresAt)` → returns `sessionId`
2. **Execute**: Delegate calls `executeSessionTransfer(agentId, sessionId, assetId, to, amount)` → transfers from agent balance to recipient
3. **Revoke**: Owner calls `revokeSession(agentId, sessionId)` → immediately invalidates the session
4. **Auto-expire**: Sessions automatically become invalid after `expiresAt` block

#### Session Constraints

| Field | Type | Description |
|-------|------|-------------|
| `delegate` | `address` | The EOA address authorized to execute on behalf of the agent |
| `perTxLimit` | `uint128` | Maximum amount per single `executeSessionTransfer` |
| `dailyLimit` | `uint128` | Maximum cumulative amount per day (~17,280 blocks) |
| `expiresAt` | `uint64` | Block number after which the session is invalid (0 = never) |

#### Storage Layout

Session data is stored under `AGENT_ADDRESS (0x209)`:

```
slot_session_count(agent_id)                  → uint64
slot_session_delegate(agent_id, session_id)   → address
slot_session_limits(agent_id, session_id)     → packed(perTxLimit, dailyLimit)
slot_session_expires(agent_id, session_id)    → uint64
slot_session_spent(agent_id, session_id)      → uint128
slot_session_last_day(agent_id, session_id)   → uint64
```

#### Gas

| Method | Base Gas |
|--------|----------|
| `createSession` | 10,000 |
| `revokeSession` | 6,000 |
| `isSessionValid` | 2,000 |
| `executeSessionTransfer` | 30,000 |

Note: Gas is paid by the delegate (EVM `msg.sender`) when calling `executeSessionTransfer`. The transferred amount is deducted from the agent's balance, not the delegate's.

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
| Session keys | 🟢 Ready | Create/revoke/validate/execute with per-tx and daily limits, expiration |
| Balance management | 🟢 Ready | Grant deducts from owner, overflow-protected credit, underflow-protected deduct |
| Payments | 🟢 Ready | Single and batch pay from agent balance, session-key delegated transfers |
| Persistence | 🟢 Ready | All state in EVM storage under `0x209`, committed with EVM state root |

---

## Test Status

- `cargo test -p call-agent` — unit tests covering registration, balance operations, nonce tracking, permission checks, precompile call extraction, agent pay/batch pay
