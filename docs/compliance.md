# CallChain Compliance System (Governance-Managed)

**Crate**: `crates/compliance/` (`call-compliance`)
**Precompile**: `0x205`
**Governance Proposal Type**: `4` (ComplianceUpdate)

---

## Overview

The Compliance System provides **global, governance-managed address restrictions** for the Callchain protocol. Unlike the previous asset-specific model, compliance is now **address-level and universal** — a restricted address cannot participate in any value-moving operation across the entire chain, regardless of asset type.

**Key principles:**
- **Governance-only management** — Only the Governance precompile (`0x203`) can update compliance status via proposal execution
- **Global scope** — Compliance status is per-address, not per-asset
- **Dual-layer enforcement** — Blocked at EVM execution layer + precompile layer for complete coverage
- **No issuer power** — Asset issuers have no compliance control; all changes go through governance

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    Governance-Managed Compliance                         │
│                                                                         │
│  ┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐   │
│  │  Governance     │     │  EVM Handler    │     │  Precompile     │   │
│  │  (0x203)        │────▶│  Layer          │────▶│  Layer          │   │
│  │                 │     │                 │     │                 │   │
│  │  submitProposal │     │  tx.caller      │     │  transfer.to    │   │
│  │  vote           │     │  check          │     │  transferFrom   │   │
│  │  queue          │     │                 │     │  .from / .to    │   │
│  │  execute ───────┼────▶│  blocklisted?   │     │  batchPay.to    │   │
│  │  (type=4)       │     │  → REVERT       │     │  switch.to      │   │
│  └─────────────────┘     └─────────────────┘     └─────────────────┘   │
│           │                                        │                    │
│           ▼                                        ▼                    │
│    ┌─────────────────────────────────────────────────────┐             │
│    │  COMPLIANCE_ADDRESS (0x205) Storage                  │             │
│    │                                                      │             │
│    │  slot("admin")         → governance_admin address    │             │
│    │  slot(address)         → status (u8)                 │             │
│    │                         0 = Clear (default)          │             │
│    │                         1+ = Restricted              │             │
│    └─────────────────────────────────────────────────────┘             │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Precompile API

The **Compliance precompile at `0x205`** exposes compliance queries and governance-triggered updates.

### Methods

| Operation | Function | Caller | Base Gas |
|---|---|---|---|
| Update compliance | `updateCompliance(address target, uint8 status)` | **GOVERNANCE (0x203) only** | 6,000 |
| Check compliance | `checkCompliance(address target) → bool` | anyone | 1,000 |
| Set admin | `setComplianceAdmin(address newAdmin)` | **current admin only** | 4,000 |

Gas is dynamically metered: `gas_used = base_gas + sloads*50 + sstores*500`.

### Status Values

| Status | Value | Meaning |
|---|---|---|
| `Clear` | `0` | Address is compliant (default for unset addresses) |
| `Restricted` | `1+` | Address is blocked from all value-moving operations |

Any non-zero status is treated as restricted. Specific values (1, 2, 3...) may be used for audit/logging purposes but have the same enforcement effect.

---

## Enforcement Layers

### Layer 1: EVM Handler (Transaction Entry)

**Location**: `crates/evm/src/executor.rs` — per-transaction validation

**Checks**:
1. **`tx.caller` is not blocklisted** — Before any EVM execution, verify the transaction sender's compliance status. If restricted, the entire transaction reverts.
2. **`tx.value` recipient check** — If the transaction carries native value (`tx.value > 0`) and the recipient (`tx.to`) is blocklisted, revert.

**Effect**: A blocklisted address **cannot initiate any EVM transaction** and **cannot receive native CALL transfers**.

### Layer 2: Precompile (Protocol Operations)

**Location**: Precompile dispatch layer

**Checks** for value-moving precompiles:

| Precompile | Method | Checked Addresses |
|---|---|---|
| Asset (`0x201`) | `transfer` | `to` (sender already blocked at Layer 1) |
| Asset (`0x201`) | `batchTransfer` | all `to` addresses |
| Asset (`0x201`) | `transferFrom` | `from` and `to` (spender = caller, already at Layer 1) |
| Asset (`0x201`) | `mint` | `to` |
| Asset (`0x201`) | `burn` | `from` |
| Agent (`0x209`) | `grantBalance` | `caller` (owner) |
| Agent (`0x209`) | `revokeBalance` | `caller` (owner) |
| Agent (`0x209`) | `pay` | `to` |
| Agent (`0x209`) | `batchPay` | all `to` addresses |
| Agent (`0x209`) | `executeSessionTransfer` | `to` |
| Bridge (`0x103`) | `externalDeposit` | `recipient` |
| Bridge (`0x103`) | `externalWithdraw` | `caller` |
| Bridge (`0x103`) | `deposit` (internal) | `targetAddress` |
| Bridge (`0x103`) | `initiateChallenge` | `challenger` |
| Bridge (`0x103`) | `withdrawChallengeBond` | `caller` |
| Shielded (`0x202`) | `deposit` | `caller` (msg_sender) |
| Shielded (`0x202`) | `withdraw` | `target` |
| Switch (`0x207`) | `switchToEvm` | `caller` and `to` |
| Switch (`0x207`) | `switchToProtocol` | `caller` and `to` |
| Validator (`0x204`) | `stake` | `caller` |
| Validator (`0x204`) | `claimUnbonded` | `caller` |
| Governance (`0x203`) | `submitProposal` | `proposer` (caller) |

**Effect**: Even if a transaction passes Layer 1, protocol-level value transfers to/from restricted addresses are blocked at the precompile level.

**Why both layers?**
- Layer 1 prevents blocklisted addresses from consuming gas / spamming the network
- Layer 2 catches cases where a blocklisted address is the `from` or `to` in a precompile call where `msg.sender` is not the restricted party (e.g., `transferFrom` where a non-blocklisted spender tries to move funds from a blocklisted address)

---

## Governance-Managed Update Flow

Compliance changes flow exclusively through the governance proposal system:

### Step 1: Submit Proposal

```solidity
IGovernance(0x203).submitProposal(
    4,                                  // proposalType = ComplianceUpdate
    "Blacklist address 0xabc...",       // title
    "Add sanctioned address to...",     // description
    abi.encode(
        address(0xabcdef1234...),       // target
        uint8(1)                        // status = Restricted
    )
);
```

- Requires 10,000 CALL deposit
- Enters `Pending` → `Active` after review period

### Step 2: Vote

```solidity
IGovernance(0x203).vote(proposalId, 1);  // 1 = For
```

- Voting power: validator 1=1 + CALL balance weighted
- No special issuer weight (removed from previous design)

### Step 3: Queue

```solidity
IGovernance(0x203).queue(proposalId);
```

- Requires quorum + more For than Against
- Enters `Queued` with 100-block timelock

### Step 4: Execute

```solidity
IGovernance(0x203).execute(proposalId);
```

- After timelock elapsed
- Writes to `COMPLIANCE_ADDRESS` storage:
  ```
  slot(target_address) = status
  ```
- Deposit refunded to proposer

### Step 5: Immediate Effect

From the next block, the restricted address:
- Cannot submit any EVM transaction (Layer 1)
- Cannot receive native CALL (Layer 1)
- Cannot be a recipient in any protocol transfer (Layer 2)
- Cannot have its funds moved by `transferFrom` (Layer 2)

---

## Storage Layout

```
COMPLIANCE_ADDRESS (0x205):
  slot("admin")              → address (current admin, defaults to GOVERNANCE_ADDRESS)
  slot(address_1)            → uint8 status
  slot(address_2)            → uint8 status
  ...
```

All state lives in EVM storage, committed atomically with the EVM state root.

---

## Key Design Decisions

### Why remove asset binding?

**Before**: Each asset had its own `policy_id` and issuer-managed compliance. This meant:
- Asset issuers could arbitrarily blacklist addresses for their asset
- No coordination between assets — an address could be blocked for asset A but free for asset B
- Issuer power was unchecked and centralized per-asset

**After**: Global address-level compliance managed by governance:
- One unified blacklist for the entire chain
- No per-asset fragmentation
- Democratic control through governance voting
- Simpler mental model: address is either compliant or not

### Why dual-layer enforcement?

EVM handler alone cannot catch all cases:
- `transferFrom(from, to, amount)`: spender (caller) may be compliant, but `from` or `to` may be restricted
- Precompile internal transfers (protocol balance slots) don't carry native EVM value

Precompile layer alone is insufficient:
- Gas consumption: a blocklisted address could still submit transactions that fail at the precompile, wasting block space
- Native EVM transfers bypass precompiles entirely

### Why keep `setComplianceAdmin`?

For emergency response. If the governance timelock is too slow for an urgent sanction, the current admin (initially governance timelock) can transfer admin to a **security council multi-sig** that can act faster. The multi-sig can later transfer admin back to the timelock.

---

## Production Readiness Assessment

| Component | Status | Notes |
|---|---|---|
| Global address status | Ready | Single slot per address, no asset coupling |
| EVM handler layer | Ready | Caller + value recipient checks per tx |
| Precompile layer | Ready | `from`/`to` checks on value-moving precompiles |
| Governance integration | Ready | Proposal type 4 execution writes compliance state |
| Admin transfer | Ready | `setComplianceAdmin` for emergency council |
| Persistence | Ready | All state in EVM storage under `0x205` |

---

## File Map

| File | Role |
|------|------|
| `crates/compliance/src/lib.rs` | `ComplianceStorage`, storage slot helpers, status read/write |
| `crates/compliance/src/precompile.rs` | `CompliancePrecompile`, ABI dispatch, admin enforcement |
| `crates/evm/src/executor.rs` | EVM handler layer: caller blocklist check |
| `crates/asset/src/precompile.rs` | Asset transfer: `to`/`from` compliance checks |
| `crates/agent/src/precompile.rs` | Agent pay/grant/revoke/session compliance checks |
| `crates/bridge/src/precompile.rs` | Bridge deposit/withdraw/challenge compliance checks |
| `crates/shielded/src/precompile.rs` | Shielded deposit/withdraw compliance checks |
| `crates/switch/src/precompile.rs` | Switch caller/recipient compliance checks |
| `crates/validator/src/precompile.rs` | Validator stake/claim compliance checks |
| `crates/governance/src/precompile.rs` | Proposal submit + type 4 execute |
| `crates/precompile/src/utils.rs` | Shared `check_compliance` helper |

---

*Last updated: 2026-05-14*
