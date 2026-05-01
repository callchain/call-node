# Callchain Protocol Payment Layer

## Overview

The Protocol Payment Layer (`crates/protocol`) is Callchain's native state management engine. It handles all protocol-level state — asset balances, staking, shielded transactions, oracle submissions, bridge operations, and compliance enforcement — which is accessed exclusively through EVM precompiles.

All user transactions are standard EVM transactions (`EvmTransaction`). Protocol operations are invoked by calling precompile addresses (`0x101`–`0x209`) within the EVM execution environment. This gives MetaMask, Solidity contracts, and all standard Ethereum tooling native access to protocol features without a separate transaction format.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  EVM Transaction → Precompile Call                          │
│  (standard RLP-encoded Ethereum tx, to = 0x101–0x209)       │
│                                                             │
│  transfer(uint64,address,uint128) ────┐                    │
│  batchTransfer(uint64,address[],uint128[]) ┤              │
│  approve / transferFrom ──────────────┤  → precompile_fn()     │
│  mint / burn ─────────────────────────┤  → atomic rollback     │
│  deposit / withdraw / transfer ┤                    │
│  externalBridgeDeposit / Withdraw ────┤                    │
│  submitPrice ─────────────────────────┤                    │
│  register / grant / revoke ───────────┤                    │
│  updateCompliance ────────────────────┘                    │
│                                                             │
│  ┌──────────────┐  ┌──────────┐  ┌──────────────┐        │
│  │ AccountState │  │AssetRegistry│ │ComplianceEngine│       │
│  │  (balances + │  │ (asset     │  │ (blacklist,   │       │
│  │   allowances)│  │  metadata) │  │  KYC, custom) │       │
│  └──────────────┘  └──────────┘  └──────────────┘        │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Precompile Function Map

All protocol operations are exposed as EVM precompile functions at fixed addresses:

| Address | Function | Action | Authorization |
|---------|----------|--------|---------------|
| `0x201` | `transfer` | Asset transfer | Sender balance + compliance |
| `0x201` | `batchTransfer` | Multi-recipient transfer | Sender balance + compliance |
| `0x201` | `approve` | Set spending allowance | Sender |
| `0x201` | `transferFrom` | Spend allowance + transfer | Allowance holder |
| `0x201` | `mint` | Create new tokens | Asset issuer only |
| `0x201` | `burn` | Destroy tokens | Asset issuer only |
| `0x209` | `register` / `grant` / `revoke` | Agent management | Sender / owner |
| `0x103` | `externalBridgeDeposit` / `externalBridgeWithdraw` | Cross-chain bridge | Bridge proof / validator sigs |
| `0x202` | `deposit` / `withdraw` / `transfer` | Shielded pool ops | ZK proof + nullifier |
| `0x205` | `updateCompliance` | Set address compliance | Asset issuer only |
| `0x101` | `submitPrice` | Price feed submission | Registered validator |
| `0x203` | `submitProposal` / `vote` / `queue` / `execute` | Governance | CALL balance / validator |
| `0x204` | `stake` / `unstake` / `claimUnbonded` | Validator staking | Sender balance |
| `0x207` | `switchToEvm` / `switchToProtocol` | Protocol ↔ EVM bridge | Sender balance |

**Atomicity:** Precompiles run inside revm, which provides automatic state rollback on revert. Precompile write operations also use snapshot-based rollback for protocol state (`BalanceState`, `ComplianceEngine`, `ShieldedState`) when an individual precompile call fails within a larger EVM transaction.

### 2. Transaction Model

All transactions are standard EVM transactions (`EvmTransaction`, RLP-encoded). Protocol operations are invoked by setting `to` to a precompile address and encoding the function selector + arguments in the `data` field.

**Signature verification:** Standard Ethereum secp256k1 ECDSA recovery (same as any EVM chain).

**Nonce sequencing:** Standard EVM nonce, managed per address in `EvmState`.

### 3. Gas & Fee Model

**Gas cost table (per precompile):**

| Precompile | Function | Base Gas |
|------------|----------|----------|
| `0x201` | `transfer` | 5,000 |
| `0x201` | `batchTransfer` | 5,000 per recipient |
| `0x201` | `approve` | 4,000 |
| `0x201` | `transferFrom` | 5,500 |
| `0x201` | `mint` | 6,000 |
| `0x201` | `burn` | 5,000 |
| `0x207` | `switchToEvm` / `switchToProtocol` | 8,000 |
| `0x202` | `deposit` / `withdraw` | 50,000 |
| `0x202` | `transfer` | 100,000 |
| `0x101` | `submitPrice` | 3,000 |
| `0x103` | `externalBridgeDeposit` | 10,000 |
| `0x103` | `externalBridgeWithdraw` | 8,000 |
| `0x103` | `challengeBridgeDeposit` | 6,000 |
| `0x209` | `register` | 6,000 |
| `0x209` | `grant` | 6,000 |
| `0x209` | `revoke` | 6,000 |
| `0x205` | `updateCompliance` | 6,000 |
| `0x203` | `submitProposal` | 20,000 |
| `0x203` | `vote` | 10,000 |
| `0x203` | `queue` / `execute` | 10,000 / 20,000 |
| `0x203` | `emergencyPause` / `emergencyResume` | 20,000 |
| `0x204` | `stake` | 20,000 |
| `0x204` | `unstake` | 20,000 |
| `0x204` | `claimUnbonded` | 20,000 |

**Fee calculation:** `fee = gas_units * (base_fee + priority_fee)`

**Base fee adjustment (EIP-1559-style):**
```
if gas_used > target:  increase = base_fee * (diff/target) * (1/8)
if gas_used < target:  decrease = base_fee * (|diff|/target) * (1/8)
```

**Fee allocation:**
- CALL fees: 50% burned, 50% validator reward, 100% priority fee to proposer
- Stablecoin fees: 50% treasury, 50% validator reward

**Precompile gas costs** are fixed per operation (see [precompile.md](precompile.md) for the full gas table). Standard EVM gas accounting applies: `fee = gas_used * (base_fee + priority_fee)`.

### 4. Balance State (`balances.rs`)

- `ProtocolBalances`: `HashMap<(AssetId, Address), Balance>`
- `Allowances`: `HashMap<(AssetId, Owner, Spender), Balance>`

Operations use `checked_add`/`checked_sub` for overflow/underflow protection. All balance mutations go through `BalanceState` methods.

### 5. Asset Registry (`registry.rs`)

- `AssetRegistry`: `HashMap<AssetId, Asset>` with symbol uniqueness enforcement
- Registration fee: 10 CALL (checked at transaction level, not in registry)
- Asset status: Active / Frozen / Delisted
- Issuer-only operations: freeze, delist, mint supply, transfer ownership, update compliance policy

### 6. Compliance Engine (`compliance.rs`)

Policies per asset:
- `None` — no checks
- `OfacBlacklist` — address-level blacklist
- `KycRequired` — KYC verification required
- `Whitelist` — whitelist-only access
- `Custom` — delegate to registered handler trait

Per-address compliance status (Clear / UnderReview / Flagged / Restricted) is stored under `(Address, policy_id)`.

`CompliancePolicy::Custom` iterates all registered handlers and requires all to pass. `check_compliance()` checks sender and recipient for Transfer, and all three parties (sender, from, to) for TransferFrom.

**Persistence:** Compliance state is persisted to DB via `save_compliance_state` / `load_compliance_state` in the node lifecycle.

---

## Production Readiness Assessment

All protocol components are production-ready. No remaining gaps.

| Component | Status |
|-----------|--------|
| Precompile execution | 🟢 Ready | Atomic rollback covers BalanceState + ComplianceEngine + ShieldedState |
| Balance management | 🟢 Ready | Checked arithmetic, no known gaps |
| Asset registry | 🟢 Ready | `registered_at` set by caller, total supply tracked on mint |
| Gas/fee model | 🟢 Ready | PoolSponsor implemented, all sponsor variants wired, priority fee enforced |
| Compliance engine | 🟢 Ready | Recipient checks, custom handlers, persistence all wired |
| Transaction validation | 🟢 Ready | Signature verification, sequential nonces, precompile limits |

---

## Test Status

- `cargo test -p call-protocol` — ~105 unit tests covering gas calculation, fee dynamics (including priority fee), precompile execution, balance operations, asset registry, compliance policies, memo validation, atomic rollback, signature verification (positive + negative), proptest roundtrip encode/decode
