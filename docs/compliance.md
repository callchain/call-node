# CallChain Compliance System

**Crate**: `crates/protocol/` (`call-protocol`)
**Spec**: §3.4, §7

---

## Goal

Provide a layered, asset-specific compliance framework that:

1. Enforces **per-asset compliance policies** — each registered asset declares its own policy (none, blacklist, KYC, whitelist, or custom).
2. Checks **both sender and recipient** on every value-moving instruction.
3. Supports **issuer-managed address compliance** — asset issuers can flag individual addresses as Restricted, UnderReview, etc.
4. Allows **runtime custom policy handlers** for integrations (e.g. on-chain KYC oracle, geographic restriction).
5. Survives node restarts via **database persistence** of all compliance state.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Compliance Check Flow                            │
│                                                                     │
│  AssetRegistry        ComplianceEngine         InstructionExec      │
│  ┌────────────┐       ┌──────────────┐        ┌────────────────┐   │
│  │ asset_id   │──────▶│ policy_id    │        │ Transfer       │   │
│  │ compliance │       │ (0–4)        │        │ BatchTransfer  │   │
│  │ _policy    │       │              │        │ TransferFrom   │   │
│  └────────────┘       │  sanctioned  │        │ Mint / Burn    │   │
│                       │  kyc_verified│        │ BridgeDeposit  │   │
│  IssuerState          │  whitelisted │        └────────────────┘   │
│  ┌────────────┐       │  address_    │               │              │
│  │ frozen     │       │  states      │               ▼              │
│  │ (separate) │       └──────────────┘    check_compliance_by_      │
│  └────────────┘                           policy_id(sender + to)    │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Policy Model

Each `Asset` in `AssetRegistry` stores a `compliance_policy: u8`:

| policy_id | Policy | Check |
|---|---|---|
| 0 | `None` | Always pass |
| 1 | `OfacBlacklist` | Address NOT in `ComplianceEngine.sanctioned` |
| 2 | `KycRequired` | Address IS in `ComplianceEngine.kyc_verified` |
| 3 | `Whitelist` | Address IS in `ComplianceEngine.whitelisted` |
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

`Instruction::UpdateCompliance { asset_id, target, status }` allows the asset issuer to set this status for any address under their asset's policy. When `status == Restricted`, `check_compliance_by_policy_id` returns `Err(ProtocolError::Compliance(...))` immediately.

## Precompile Alternative

The **Compliance precompile at `0x205`** provides the same functionality via standard EVM transactions:

| Instruction | Precompile Function | Gas |
|---|---|---|
| `UpdateCompliance` | `updateCompliance(uint64,address,uint8)` | 10,000 |
| — | `checkCompliance(uint64,address)` (read-only) | 1,000 |

See [precompile.md](precompile.md) for the full ABI.

### Compliance Engine State

`ComplianceEngine` (`crates/protocol/src/compliance.rs`):

```rust
pub struct ComplianceEngine {
    sanctioned: HashSet<Address>,      // OFAC-style blacklist
    kyc_verified: HashSet<Address>,    // KYC registry
    whitelisted: HashSet<Address>,     // Whitelist registry
    custom_handlers: HashMap<u8, Box<dyn CustomComplianceHandler + Send + Sync>>,
    address_states: HashMap<(Address, u8), AddressComplianceState>,
}
```

**Persistence**: `ComplianceEngineSnapshot` (derived `Serialize`/`Deserialize`) captures all sets and address states. Custom handlers are runtime-only `Box<dyn>` trait objects and are intentionally excluded — they must be re-registered at node boot. The snapshot is stored as a JSON blob under key `[0]` in the `CallComplianceState` MDBX table.

**Atomic rollback**: `ComplianceEngine` implements `Clone`. During instruction execution, the engine is cloned before execution; if any instruction fails, the clone replaces the live instance, rolling back all compliance-side mutations.

### Instruction-Level Enforcement

`execute_protocol_instructions` checks compliance on every value-moving instruction:

| Instruction | Compliance Check |
|---|---|
| `Transfer` | sender + recipient |
| `BatchTransfer` | sender + every recipient |
| `TransferFrom` | sender (spender) + from + to |
| `Mint` | recipient |
| `Burn` | from |
| `BridgeDeposit` | recipient |
| `ShieldedDeposit` / `ShieldedWithdraw` | target address |

Non-value instructions (`Approve`, `OracleSubmit`, `Governance*`, `Agent*`, `UpdateCompliance` itself) do not trigger compliance checks.

### RPC Surface

| Method | Type | Description |
|---|---|---|
| `call_compliancePolicy(asset_id)` | query | Returns the `compliance_policy` u8 for an asset |

There is no direct RPC mutation endpoint for compliance state. All changes flow through `ProtocolTransaction` instructions:
- `UpdateCompliance` — issuer sets address status
- `IssuerAction::UpdatePolicy` — issuer changes the asset's policy

---

## Current Status

### What Works

| Component | Status | Details |
|---|---|---|
| **Policy types** | Ready | All 5 policies (None, OfacBlacklist, KycRequired, Whitelist, Custom) implemented and tested |
| **Sender + recipient checks** | Ready | `Transfer`, `BatchTransfer`, `TransferFrom` check both sender and recipient compliance (Gap 6 fixed) |
| **Per-address status** | Ready | `Clear`/`UnderReview`/`Flagged`/`Restricted` statuses; `Restricted` blocks unconditionally |
| **Custom handlers** | Ready | `CustomComplianceHandler` trait; all registered handlers must approve (Gap 8 fixed) |
| **Issuer authorization** | Ready | `UpdateCompliance` and `UpdatePolicy` require `asset.issuer == sender` |
| **Atomic rollback** | Ready | `ComplianceEngine` cloned for snapshot-based rollback alongside `BalanceState` and `ShieldedState` (Gap 1 fixed) |
| **State persistence** | Ready | `ComplianceEngineSnapshot` + `CallComplianceState` MDBX table; load/save wired in `CallNode::new()` and `persist_state_to_db()` (Gap 7 fixed) |

### Persistence Details

- **Save**: `save_compliance_state()` calls `engine.snapshot()`, serializes to JSON, writes to `CallComplianceState` table key `[0]`
- **Load**: `load_compliance_state()` reads the blob, deserializes to `ComplianceEngineSnapshot`, then calls `engine.restore_from_snapshot()`
- **Custom handlers**: NOT persisted. Nodes must re-register handlers at boot time (e.g. via `node_init` hooks or config)

---

## Gaps & Suggestions

### Known Limitations

#### Custom Handler Re-registration

**Problem**: `Box<dyn CustomComplianceHandler>` cannot be serialized. After a node restart, all custom handlers are lost even though the rest of compliance state is restored.

**Impact**: Assets using `CompliancePolicy::Custom` will silently pass all checks until handlers are re-registered.

**Mitigation**: Custom handlers must be re-registered during node initialization. A future improvement could add a registry of named handler factories (e.g. `HashMap<String, fn() -> Box<dyn CustomComplianceHandler>>`) that can be serialized and re-instantiated automatically.

#### No Dedicated Compliance RPC Mutations

**Problem**: There are no direct RPC endpoints to add/remove addresses from the blacklist, KYC list, or whitelist. The only way to modify these sets is via `Instruction::UpdateCompliance` submitted as a `ProtocolTransaction`.

**Impact**: Operators must construct and sign a full `ProtocolTransaction` to update compliance state.

**Status**: By design — compliance mutations go through consensus to ensure auditability and replay protection. The `UpdateCompliance` instruction is the intended path.

#### Frozen Addresses (IssuerState) Are Separate

**Problem**: `IssuerState.frozen` (per-asset address freezing) and `ComplianceEngine` (global policy enforcement) are separate systems. An address can be frozen for asset A but still pass compliance for asset B.

**Impact**: Operators must manage two independent restriction mechanisms.

**Status**: By design — `IssuerState` freezing is asset-specific issuer discretion; `ComplianceEngine` is policy-driven. They serve different use cases.

---

## Production Readiness Assessment

| Component | Status | Blocker |
|---|---|---|
| Policy enforcement | Ready | None |
| Address status tracking | Ready | None |
| Custom handlers | Ready | Requires re-registration after restart |
| Persistence | Ready | None |
| Instruction integration | Ready | None |

---

*Last updated: 2026-04-20*
