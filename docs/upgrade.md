# Callchain Upgrade and Fork Management

## Overview

The Upgrade/Fork Management system (`crates/consensus/src/fork.rs`) handles protocol version evolution through height-activated upgrades, governance-triggered changes with timelock, and emergency rollback via validator multi-signature.

**Key mechanisms:**
- Height-activated protocol upgrades
- Governance proposal integration with timelock
- Block version validation
- Emergency rollback (2/3 validator signatures)

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  ForkManager                                                 │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ Scheduled Upgrades │  │ Emergency Rollback           │  │
│  │ - version          │  │ - target_height              │  │
│  │ - activation_height│  │ - target_version             │  │
│  │ - proposal_id      │  │ - signatures: HashMap        │  │
│  │ - applied flag     │  │ - total_validators           │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ Validator Keys (for rollback sig verification)          ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Upgrade Scheduling

`ForkManager::schedule_upgrade()` adds an `UpgradeEntry` to the scheduled list. `check_upgrades_at_height()` applies any pending upgrade whose activation height has been reached.

`ForkManager::schedule_governance_upgrade()` enforces a timelock: activation must be at least `timelock_blocks` (default 1000) after the current height.

**Gap #1 — `check_upgrades_at_height` applies only the first matching upgrade:** The function iterates `scheduled_upgrades` and returns after applying the first non-applied upgrade at or below the current height. If multiple upgrades are scheduled for the same height, only one is applied. The loop should continue to apply all eligible upgrades.

**Gap #2 — `version_at_height` ignores the `applied` flag:** It considers all scheduled upgrades at or below the height, even those that were never applied (e.g., because `check_upgrades_at_height` was not called). This can produce incorrect version expectations.

**Gap #3 — No actual block version field:** `BlockHeader` does not contain a `version` field. `validate_block_version()` validates a hypothetical version but nothing in the block production or consensus path calls it with actual block data.

### 2. Governance Integration

The `NodeProposalExecutor` (in `rpc/src/handlers.rs`) handles `ProposalType::ProtocolUpgrade` by calling `ForkManager::schedule_governance_upgrade()`.

**Gap #4 — ForkManager is in-memory only:** `ForkManager` is constructed fresh in `RpcState::new()` and never persisted to disk. On node restart, all scheduled upgrades and rollback state are lost.

**Gap #5 — No network-wide upgrade coordination:** Each node has its own `ForkManager` instance. There is no gossip or sync mechanism to ensure all nodes have the same scheduled upgrades. A validator could miss a governance proposal and produce blocks with the wrong version.

### 3. Emergency Rollback

`ForkManager::submit_rollback_signature()` collects Ed25519 signatures from validators. When 2/3 quorum is reached, it returns `EmergencyRollbackResult` and clears the active rollback.

**Gap #6 — Rollback result is not acted upon:** `submit_rollback_signature()` returns the result to the caller, but there is no code that consumes this result to actually perform a chain rollback. The function verifies signatures and counts them but the actual state reversion is unimplemented.

**Gap #7 — No rollback replay protection:** The rollback message includes a domain separator (`CALL-EMERGENCY-ROLLBACK:`) but no nonce or height bound. A validator signature for rollback to height 500 could be replayed indefinitely.

**Gap #8 — No feature flagging based on version:** The codebase does not use `ProtocolVersion` to conditionally enable/disable features. Adding a new instruction type or changing validation rules would require a hardcoded switch, not a version-gated path.

### 4. Timelock

Default timelock: 1000 blocks (~4 minutes at 250ms/block). Minimum: 100 blocks.

**Gap #9 — Timelock default is very short:** 1000 blocks at 250ms is only ~4 minutes. This may not give operators sufficient time to review and react to controversial upgrades.

---

## File Map

| File | Role |
|------|------|
| `crates/consensus/src/fork.rs` | `ForkManager`, `UpgradeEntry`, `EmergencyRollback`, rollback signatures |
| `crates/rpc/src/handlers.rs` | `NodeProposalExecutor` — governance proposal → fork manager |
| `crates/governance/src/lib.rs` | Governance proposal types and lifecycle |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Height-activated upgrades | 🟡 Partial | Scheduling works, but application logic has edge cases |
| Governance upgrade trigger | 🟡 Partial | Timelock enforced, but ForkManager not persisted |
| Block version validation | 🔴 Not ready | BlockHeader has no version field; validation not called |
| Emergency rollback | 🟡 Partial | Signature collection works, but actual rollback unimplemented |
| Network coordination | 🔴 Not ready | No sync/gossip of scheduled upgrades |
| Feature flagging | 🔴 Not ready | Version not used to gate features |

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | **`check_upgrades_at_height` applies only first match** | Medium | Multiple upgrades at same height: only first is applied. |
| 2 | **`version_at_height` ignores `applied` flag** | Medium | Unapplied upgrades still affect version expectations. |
| 3 | **No block version field** | High | `BlockHeader` lacks version. Validation is theoretical. |
| 4 | **ForkManager not persisted** | High | All upgrade/rollback state lost on restart. |
| 5 | **No network-wide upgrade sync** | Critical | Nodes may have divergent upgrade schedules. No gossip. |
| 6 | **Emergency rollback not executed** | Critical | Signature verification succeeds but no state reversion code exists. |
| 7 | **Rollback signatures replayable** | Medium | No nonce or height bound in rollback message. |
| 8 | **No version-gated features** | High | Protocol changes cannot be activated conditionally by version. |
| 9 | **Timelock default very short** | Low | 1000 blocks ≈ 4 minutes. Insufficient for operator review. |
| 10 | **No upgrade readiness check** | Medium | No mechanism to verify all validators have adopted new version before activation. |

---

## Test Status

- `cargo test -p call-consensus` (fork tests) — covers height-activated upgrade, version mismatch rejection, governance trigger with timelock, emergency rollback with 14/21 signatures
- Missing: persistence tests, multi-upgrade-at-same-height tests, network sync tests, actual rollback execution tests
