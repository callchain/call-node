# Version Migration Guide

**Last updated**: 2026-05-22

---

## Overview

This guide provides version-specific migration instructions for Callchain node operators upgrading between releases. It supplements the general operational procedures in [`how-to/for-operator.md`](how-to/for-operator.md) and the protocol upgrade mechanism described in [`upgrade.md`](upgrade.md).

---

## Versioning Scheme

Callchain follows [SemVer](https://semver.org/):

| Component | Meaning | Example |
|-----------|---------|---------|
| **MAJOR** | Breaking protocol changes requiring a hard fork | `v1.0.0` |
| **MINOR** | New features, backward-compatible protocol additions | `v0.2.0` |
| **PATCH** | Bug fixes, security patches | `v0.1.1` |

Pre-release tags (`-testnet`, `-rc.1`) indicate non-production builds. Testnet releases may introduce breaking changes without a major version bump.

### Version Compatibility Rules

- **Same MAJOR.MINOR**: Patch releases are drop-in replacements. Restart with the new binary.
- **MINOR bump**: Usually backward-compatible. Review the migration table for config or RPC changes.
- **MAJOR bump**: Expect breaking changes. Coordinate with other validators and follow the hard fork procedure below.

---

## Pre-Upgrade Checklist

Complete these steps before every upgrade, regardless of version:

- [ ] Read the release notes for the target version.
- [ ] Verify the target version is listed in the migration table below and note any special actions.
- [ ] Take a database snapshot (offline, consistent backup):
  ```bash
  sudo systemctl stop callchaind
  sudo tar czf /backup/callchain-pre-upgrade-$(date +%Y%m%d).tar.gz -C /var/lib/callchain .
  sudo systemctl start callchaind
  ```
- [ ] Back up the current binary:
  ```bash
  sudo cp /usr/local/bin/calld /usr/local/bin/calld.backup
  ```
- [ ] Back up configuration files:
  ```bash
  sudo cp /etc/callchain/config.toml /etc/callchain/config.toml.backup
  ```
- [ ] Announce a maintenance window to users and peers.
- [ ] For validator operators: confirm >= 2/3 of validators are ready to upgrade (see [Hard Fork Coordination](#hard-fork-coordination)).
- [ ] If the migration table notes a database schema change, plan for a longer downtime or a resync.

---

## Rolling Upgrade Procedure

Use this procedure for standard upgrades (patch and minor versions) that do not require a coordinated hard fork.

1. **Download or build the new binary**:
   ```bash
   # Docker
   docker pull ghcr.io/callchain/callchaind:vX.Y.Z

   # From source
   git fetch origin
   git checkout vX.Y.Z
   cargo build --release
   ```

2. **Replace the binary**:
   ```bash
   sudo cp /usr/local/bin/calld /usr/local/bin/calld.backup
   sudo cp target/release/calld /usr/local/bin/calld
   ```

3. **Review config changes** (if any):
   - Compare your `/etc/callchain/config.toml` against the example in the release notes.
   - Apply new required fields or update deprecated ones.

4. **Restart the node**:
   ```bash
   sudo systemctl restart callchaind
   ```

5. **Verify health**:
   ```bash
   curl -s http://localhost:9090/health | jq .
   curl -s -X POST http://localhost:8545 \
     -H "Content-Type: application/json" \
     -d '{"jsonrpc":"2.0","method":"eth_syncing","id":1}' | jq .
   ```

6. **Monitor for 10 minutes**:
   - Block height advancing
   - Peer count stable
   - No ERROR-level logs

---

## Rollback Procedure

If the upgraded node exhibits consensus errors, state corruption, or critical bugs, roll back immediately.

1. **Stop the node**:
   ```bash
   sudo systemctl stop callchaind
   ```

2. **Restore the previous binary**:
   ```bash
   sudo cp /usr/local/bin/calld.backup /usr/local/bin/calld
   ```

3. **Restore the database** (only if the upgrade performed a schema change or the database was modified):
   ```bash
   sudo rm -rf /var/lib/callchain/mdbx
   sudo tar xzf /backup/callchain-pre-upgrade-<DATE>.tar.gz -C /var/lib/callchain
   ```

4. **Restore the previous config** (if you changed it during upgrade):
   ```bash
   sudo cp /etc/callchain/config.toml.backup /etc/callchain/config.toml
   ```

5. **Restart the node**:
   ```bash
   sudo systemctl start callchaind
   ```

6. **Verify recovery**:
   ```bash
   curl -s http://localhost:9090/health
   sudo journalctl -u callchaind -f | grep "sync\|consensus\|error"
   ```

7. **File a post-mortem** issue with the `rollback` tag describing the failure.

> **Warning:** Do not attempt to roll back across a hard fork height that has already activated. If the chain has already forked, consult the [Chain Halt Runbook](runbooks/chain-halt.md) and coordinate an emergency rollback via validator multi-signature (2/3 quorum) as described in [`upgrade.md`](upgrade.md).

---

## Migration Table

| From | To | Breaking Changes | Action Required |
|------|-----|------------------|-----------------|
| v0.1.0-testnet | v0.2.0 | (TBD) | (TBD) |

---

## Important Notes

### Database Schema Changes

Callchain uses MDBX as its sole persistence backend with 40 tables covering EVM state, consensus blocks, receipts, and more. As noted in [`storage.md`](storage.md), there is currently **no automated database migration framework**. If a release changes the data layout:

- The migration table will flag it explicitly.
- You must perform a **full resync** from genesis or restore from a pre-upgrade snapshot taken with the old binary.
- Archive nodes cannot toggle pruning modes without a resync.

To resync from genesis:
```bash
sudo systemctl stop callchaind
sudo rm -rf /var/lib/callchain/mdbx/*
sudo systemctl start callchaind
```

### Config File Changes

New versions may introduce required configuration fields or deprecate old ones. Always compare your `config.toml` against the release notes. Common changes include:

- New `[storage]` parameters (pruning retention, cache sizes)
- New `[p2p]` bootstrap peers
- New `[governance]` or `[light_client]` settings
- Renamed or removed RPC flags

Keep a versioned backup of your config after every upgrade.

### Genesis File Compatibility

The genesis file defines the chain ID, initial validator set, balances, and consensus parameters. It is loaded once on first boot and persisted into MDBX.

- **Never replace the genesis file** on an existing node with a different genesis. Doing so will cause a state root mismatch and the node will refuse to start.
- If you must restart from a new genesis, wipe the database (`rm -rf /var/lib/callchain/mdbx/*`) and restart.
- Genesis files are version-agnostic: a `v0.1.0` genesis is valid for `v0.2.0` unless the release notes state otherwise.

### Hard Fork Coordination

A **hard fork** is any upgrade that changes consensus rules (e.g., new precompiles, modified block validation, slashing condition changes). These require coordination among validators:

- **Quorum requirement**: At least **2/3 of validators** (by stake weight) must upgrade and signal readiness before the activation height.
- **Activation height**: Hard forks are activated at a specific block height, not by binary version. The `ForkManager` schedules the upgrade and validates block versions after the activation height (see [`upgrade.md`](upgrade.md)).
- **Gossip coordination**: Upgraded nodes automatically broadcast `UpgradeAnnouncement` messages over the `UPGRADE_CHANNEL`. Peers auto-schedule the upgrade if they have not already.
- **Validator readiness**: Validators can explicitly signal readiness for an upcoming upgrade. If `require_validator_readiness` is enabled, activation is gated on a 2/3 quorum of readiness signals.

**Pre-fork validator checklist**:
- [ ] Confirm activation height with other operators.
- [ ] Upgrade binary and restart before the activation height.
- [ ] Verify `call_getBlockHeight` is close to the network tip.
- [ ] Monitor for `UpgradeAnnouncement` gossip from peers.
- [ ] After activation height, verify block version in logs matches the new protocol version.

If fewer than 2/3 validators upgrade before the activation height, the chain may halt. In that case, follow the [Chain Halt Runbook](runbooks/chain-halt.md).

---

## See Also

- [`how-to/for-operator.md`](how-to/for-operator.md) — General node operations, backup, and monitoring
- [`upgrade.md`](upgrade.md) — Protocol upgrade mechanism, `ForkManager`, and emergency rollback
- [`release.md`](release.md) — Release process, canary deployment, and performance baselines
- [`storage.md`](storage.md) — Database architecture, pruning, and snapshot details
- [`runbooks/chain-halt.md`](runbooks/chain-halt.md) — Consensus halt diagnosis and recovery
