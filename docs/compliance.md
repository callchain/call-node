# CallChain Compliance System

**Crate**: `crates/protocol/` (`call-protocol`)
**Spec**: §3.4, §7

---

## Goal

Provide a layered, asset-specific compliance framework that:

1. Enforces **per-asset compliance policies** — each registered asset declares its own policy (none, blacklist, KYC, whitelist, or custom).
2. Checks **both sender and recipient** on every value-moving precompile call.
3. Supports **issuer-managed address compliance** — asset issuers can flag individual addresses as Restricted, UnderReview, etc.
4. Allows **runtime custom policy handlers** for integrations (e.g. on-chain KYC oracle, geographic restriction).
5. All compliance state lives in EVM storage and is committed atomically with the EVM state root.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Compliance Check Flow                            │
│                                                                     │
│  AssetRegistry        Compliance state (EVM)       PrecompileExec  │
│  ┌────────────┐       ┌──────────────┐        ┌────────────────┐   │
│  │ asset_id   │──────▶│ policy_id    │        │ Transfer       │   │
│  │ compliance │       │ (0–4)        │        │ BatchTransfer  │   │
│  │ _policy    │       │              │        │ TransferFrom   │   │
│  └────────────┘       │  sanctioned  │        │ Mint / Burn    │   │
│                       │  kyc_verified│        │ BridgeDeposit  │   │
│  IssuerState          │  whitelisted │        └────────────────┘   │
│  ┌────────────┐       │  address_    │               │              │
│  │ frozen     │       │  states      │               ▼              │
│  │ (EVM)      │       └──────────────┘    check_compliance_by_      │
│  └────────────┘                           policy_id(sender + to)    │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Policy Model

Each `Asset` in `AssetRegistry` stores a `compliance_policy: u8`:

| policy_id | Policy | Check |
|---|---|---|
| 0 | `None` | Always pass |
| 1 | `OfacBlacklist` | Address NOT in sanctioned set (EVM storage) |
| 2 | `KycRequired` | Address IS in kyc_verified set (EVM storage) |
| 3 | `Whitelist` | Address IS in whitelisted set (EVM storage) |
| 4 | `Custom` | All registered `CustomComplianceHandler`s return `true` |

Policy is set at asset registration (`register_asset`) and can be updated by the issuer via `IssuerAction::UpdatePolicy`.

### Address Compliance Status

Beyond the policy-level checks, each `(address, policy_id)` pair can have a granular status:

| Status | Meaning |
|---|---|
| `Clear` | Default — no restriction beyond policy check |
| `UnderReview` | Informational — policy check still applies |
| `Flagged` | Informational — policy check still applies |
| `Restricted` | **Hard block** — transaction fails regardless of policy check |

The Compliance precompile (`0x205`) `updateCompliance(uint64,address,uint8)` allows the asset issuer to set this status for any address under their asset's policy. When `status == Restricted`, `check_compliance_by_policy_id` returns `Err(ProtocolError::Compliance(...))` immediately.

## Precompile Alternative

The **Compliance precompile at `0x205`** provides the same functionality via standard EVM transactions:

| Operation | Precompile Function | Gas |
|---|---|---|
| `UpdateCompliance` | `updateCompliance(uint64,address,uint8)` | base + storage |
| — | `checkCompliance(uint64,address)` (read-only) | base + storage |

Gas is dynamically metered: `gas_used = base_gas + sloads*50 + sstores*500`.

See [precompile.md](precompile.md) for the full ABI.

### Compliance State in EVM Storage

All compliance state — sanctioned sets, KYC verified sets, whitelisted sets, and per-address compliance statuses — lives in EVM storage slots under the Compliance precompile address (`0x205`). There is no in-memory `ComplianceEngine` struct with `HashSet`s, no `ComplianceEngineSnapshot`, and no separate `CallComplianceState` database table.

State is read and written via `StorageRef` during precompile execution and is committed atomically with the EVM state root. Revm's Journal handles automatic rollback on transaction failure; there is no manual clone/snapshot mechanism.

### Precompile-Level Enforcement

The precompile dispatcher checks compliance on every value-moving precompile call:

| Precompile Call | Compliance Check |
|---|---|
| `transfer` | sender + recipient |
| `batchTransfer` | sender + every recipient |
| `transferFrom` | sender (spender) + from + to |
| `mint` | recipient |
| `burn` | from |
| `switchToEvm` | recipient |
| `shieldedDeposit` / `shieldedWithdraw` | target address |

Non-value precompile calls (`approve`, `submitPrice`, `governance*`, `agent*`, `updateCompliance` itself) do not trigger compliance checks.

### RPC Surface

| Method | Type | Description |
|---|---|---|
| `call_compliancePolicy(asset_id)` | query | Returns the `compliance_policy` u8 for an asset |

There is no direct RPC mutation endpoint for compliance state. All changes flow through EVM precompile calls:
- `updateCompliance(uint64,address,uint8)` on `0x205` — issuer sets address status
- Asset policy updates are done via the Asset precompile (`0x201`)

---

## Current Status

### What Works

| Component | Status | Details |
|---|---|---|
| **Policy types** | Ready | All 5 policies (None, OfacBlacklist, KycRequired, Whitelist, Custom) implemented and tested |
| **Sender + recipient checks** | Ready | `Transfer`, `BatchTransfer`, `TransferFrom` check both sender and recipient compliance |
| **Per-address status** | Ready | `Clear`/`UnderReview`/`Flagged`/`Restricted` statuses; `Restricted` blocks unconditionally |
| **Custom handlers** | Ready | `CustomComplianceHandler` trait; all registered handlers must approve |
| **Issuer authorization** | Ready | `UpdateCompliance` and `UpdatePolicy` require `asset.issuer == sender` |
| **Atomic rollback** | Ready | Revm Journal handles automatic rollback on transaction failure |
| **State persistence** | Ready | All state in EVM storage under `0x205`, committed with EVM state root |

---

## Gaps & Suggestions

### Known Limitations

#### Custom Handler Registration

**Problem**: `Box<dyn CustomComplianceHandler>` cannot be serialized. After a node restart, custom handlers must be re-registered.

**Impact**: Assets using `CompliancePolicy::Custom` will silently pass all checks until handlers are re-registered.

**Mitigation**: Custom handlers must be re-registered during node initialization. A future improvement could add a registry of named handler factories (e.g. `HashMap<String, fn() -> Box<dyn CustomComplianceHandler>>`) that can be serialized and re-instantiated automatically.

#### No Dedicated Compliance RPC Mutations

**Problem**: There are no direct RPC endpoints to add/remove addresses from the blacklist, KYC list, or whitelist. The only way to modify these sets is via the Compliance precompile (`0x205`) `updateCompliance` function, submitted as a standard EVM transaction.

**Impact**: Operators must construct and sign an EVM transaction calling the Compliance precompile to update compliance state.

**Status**: By design — compliance mutations go through consensus to ensure auditability and replay protection. The `updateCompliance` precompile call is the intended path.

#### Frozen Addresses (IssuerState) Are Separate

**Problem**: `IssuerState.frozen` (per-asset address freezing) and compliance policy enforcement are separate systems in EVM storage. An address can be frozen for asset A but still pass compliance for asset B.

**Impact**: Operators must manage two independent restriction mechanisms.

**Status**: By design — `IssuerState` freezing is asset-specific issuer discretion; compliance policy enforcement is policy-driven. They serve different use cases. Both live in EVM storage.

---

## Production Readiness Assessment

| Component | Status | Blocker |
|---|---|---|
| Policy enforcement | Ready | None |
| Address status tracking | Ready | None |
| Custom handlers | Ready | Requires re-registration after restart |
| Persistence | Ready | None |
| Precompile integration | Ready | None |

---

*Last updated: 2026-05-07*
