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
│  All protocol state (balances, assets, validators, etc.)   │
│  lives in EVM storage slots under precompile addresses.    │
│  No separate in-memory protocol state structures.          │
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

**Atomicity:** Precompiles run inside revm, which provides automatic state rollback on revert. Protocol state lives in EVM storage under precompile addresses; writes made through `StorageRef` are subject to the same Journal rollback as any EVM contract. No separate protocol-state snapshot is needed.

### 2. Transaction Model

All transactions are standard EVM transactions (`EvmTransaction`, RLP-encoded). Protocol operations are invoked by setting `to` to a precompile address and encoding the function selector + arguments in the `data` field.

**Signature verification:** Standard Ethereum secp256k1 ECDSA recovery (same as any EVM chain).

**Nonce sequencing:** Standard EVM nonce, managed per address in EVM storage (`CallEvmAccounts`).

### 3. Gas & Fee Model

**Dynamic gas metering:**

Precompile gas is computed dynamically based on storage operations performed:

```
gas_used = base_gas + sloads * 50 + sstores * 500
```

| Precompile | Base Gas | Typical Storage Ops |
|------------|----------|---------------------|
| `0x201` | 5,000 | 2–4 sloads + 2–4 sstores |
| `0x207` | 8,000 | 2 sloads + 2 sstores + EVM call |
| `0x202` | 25,000 | Merkle tree / ZK proof ops |
| `0x101` | 3,000 | 1–2 sloads + 1 sstore |
| `0x103` | 10,000 | Signature verification + sstores |
| `0x209` | 6,000 | 2–3 sloads + 2 sstores |
| `0x205` | 6,000 | 1 sload + 1 sstore |
| `0x203` | 10,000 | 2–5 sloads + 2–5 sstores |
| `0x204` | 20,000 | 3–5 sloads + 3–5 sstores |

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

Operations use `checked_add`/`checked_sub` for overflow/underflow protection. All balance mutations go through EVM storage under the asset precompile (`0x201`).

### 5. Asset Registry (`registry.rs`)

- Asset metadata lives in EVM storage under the Asset precompile (`0x201`) with symbol uniqueness enforcement
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

**Persistence:** Compliance state is stored in EVM storage slots under the compliance precompile address (`0x205`). No separate DB persistence is needed — state is committed atomically with the EVM state root.

---

## Production Readiness Assessment

All protocol components are production-ready. No remaining gaps.

| Component | Status |
|-----------|--------|
| Precompile execution | 🟢 Ready | Atomic rollback covers all EVM storage mutations via revm Journal |
| Balance management | 🟢 Ready | Checked arithmetic, no known gaps |
| Asset registry | 🟢 Ready | `registered_at` set by caller, total supply tracked on mint |
| Gas/fee model | 🟢 Ready | PoolSponsor implemented, all sponsor variants wired, priority fee enforced |
| Compliance engine | 🟢 Ready | Recipient checks, custom handlers, persistence all wired |
| Transaction validation | 🟢 Ready | Signature verification, sequential nonces, precompile limits |

---

## Test Status

- `cargo test -p call-protocol` — ~105 unit tests covering gas calculation, fee dynamics (including priority fee), precompile execution, balance operations, asset registry, compliance policies, memo validation, atomic rollback, signature verification (positive + negative), proptest roundtrip encode/decode
