# Callchain Upgrade and Fork Management

## Overview

The Upgrade/Fork Management system (`crates/consensus/src/fork.rs`) handles protocol version evolution through height-activated upgrades, governance-triggered changes with timelock, block version validation, emergency rollback via validator multi-signature, version-gated feature activation, and validator readiness tracking.

**Key mechanisms:**
- Height-activated protocol upgrades
- Governance proposal integration with timelock
- Block version validation
- Emergency rollback (2/3 validator signatures)
- Network-wide upgrade gossip coordination
- Version-gated protocol feature flags
- Validator upgrade readiness tracking

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  ForkManager                                                 │
│                                                              │
│  ┌────────────────────┐  ┌──────────────────────────────┐   │
│  │ Scheduled Upgrades │  │ Emergency Rollback           │   │
│  │ - version          │  │ - target_height              │   │
│  │ - activation_height│  │ - target_version             │   │
│  │ - proposal_id      │  │ - signatures: HashMap        │   │
│  │ - applied flag     │  │ - total_validators           │   │
│  └────────────────────┘  └──────────────────────────────┘   │
│                                                              │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ Validator Keys (for rollback sig verification)          ││
│  └─────────────────────────────────────────────────────────┘│
│                                                              │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ Protocol Feature Flags                                  ││
│  │ - ShieldedPool, AgentPrecompiles, BridgeOperations      ││
│  │ - SmartAccounts, ComplianceEngine, OracleIntegration    ││
│  └─────────────────────────────────────────────────────────┘│
│                                                              │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ Upgrade Readiness Tracking                              ││
│  │ - validator_readiness: version -> validator set         ││
│  │ - require_validator_readiness (configurable)            ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Upgrade Scheduling

`ForkManager::schedule_upgrade()` adds an `UpgradeEntry` to the scheduled list. `check_upgrades_at_height()` applies any pending upgrades at or below the current height, including multiple upgrades scheduled for the same height.

`ForkManager::schedule_governance_upgrade()` enforces a timelock: activation must be at least `timelock_blocks` (default 10000) after the current height.

### 2. Block Version Validation

`BlockHeader` includes a `version: ProtocolVersion` field. During block production, the current version is read from `ForkManager`. During validation, `BlockHeader::validate()` calls `ForkManager::validate_block_version()` to ensure the block's version matches the expected version at its height.

### 3. Governance Integration

The `NodeProposalExecutor` (in `rpc/src/handlers.rs`) handles `ProposalType::ProtocolUpgrade` by calling `ForkManager::schedule_governance_upgrade()`.

The `ForkManager` is persisted via `save_fork_state()` / `load_fork_state()` to the MDBX database (`CallForkState` table), so scheduled upgrades and rollback state survive node restarts.

### 4. Network-Wide Upgrade Coordination

The node gossips scheduled upgrades via `UPGRADE_CHANNEL`. When a node produces a block, it broadcasts an `UpgradeAnnouncement` containing the next scheduled upgrade's version and activation height. Peers receiving this announcement automatically schedule the upgrade if not already present.

### 5. Emergency Rollback

`ForkManager::submit_rollback_signature()` collects Ed25519 signatures from validators with nonce-based replay protection. When 2/3 quorum is reached, it returns `EmergencyRollbackResult`. The RPC handler stores the `RollbackPlan` in `state.pending_rollback`, and the node's main loop applies it via `apply_rollback_plan()`, which:
- Resets block height and consensus height
- Clears block cache and execution receipts above target
- Resets governance, oracle, and validator state blocks
- Deletes block files above target height

### 6. Feature Flagging

`ProtocolFeature` enum defines version-gated capabilities (e.g., `ShieldedPool`, `AgentPrecompiles`, `SmartAccounts`). Each feature has a `min_version()`. `ForkManager::is_feature_enabled()` checks if the current version satisfies the minimum. This allows protocol changes to be activated conditionally by version rather than hardcoded.

### 7. Upgrade Readiness

Validators can signal readiness for an upcoming upgrade via `ForkManager::signal_upgrade_readiness()`. The node can enable `require_validator_readiness` to gate upgrade activation on a 2/3 quorum of validators having signaled readiness. This ensures sufficient validator adoption before a protocol change activates.

---

## File Map

| File | Role |
|------|------|
| `crates/consensus/src/fork.rs` | `ForkManager`, `UpgradeEntry`, `EmergencyRollback`, `ProtocolFeature`, rollback signatures, readiness tracking |
| `crates/consensus/src/block.rs` | `BlockHeader` with version field, block validation, execution |
| `crates/rpc/src/handlers.rs` | `NodeProposalExecutor` — governance proposal -> fork manager |
| `crates/rpc/src/callchain.rs` | `call_submitRollbackSignature` RPC, rollback plan dispatch |
| `crates/node/src/lib.rs` | Node loop: upgrade gossip, `check_upgrades_at_height`, `apply_rollback_plan` |
| `crates/governance/src/lib.rs` | Governance proposal types and lifecycle |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Height-activated upgrades | Ready | Applies all eligible upgrades; multiple upgrades at same height supported |
| Governance upgrade trigger | Ready | Timelock enforced; ForkManager persisted to MDBX |
| Block version validation | Ready | BlockHeader has version field; validation wired into production and consensus paths |
| Emergency rollback | Ready | Signature collection + nonce replay protection; structural reversion applied by node loop |
| Network coordination | Ready | Upgrade gossip via UPGRADE_CHANNEL; peers auto-schedule received upgrades |
| Feature flagging | Ready | ProtocolFeature enum with min_version checks |
| Upgrade readiness | Ready | Validator readiness tracking with optional quorum-gated activation |

---

## Test Status

- `cargo test -p call-consensus` (fork tests) — covers height-activated upgrade, multi-upgrade-at-same-height, version mismatch rejection, governance trigger with timelock, emergency rollback with 14/21 signatures, nonce replay protection, feature flagging by version, upgrade readiness signaling and quorum-gated activation
