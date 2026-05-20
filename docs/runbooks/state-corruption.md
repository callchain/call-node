# Runbook: State Corruption Recovery

**Severity**: Critical
**Scope**: Affected node(s)
**Owner**: On-call protocol engineer + storage engineer
**Last updated**: 2026-04-20

---

## Symptoms

| Symptom | Likely Cause |
|---|---|
| Node crashes on startup with `StorageError::Database` | MDBX corruption |
| `calld` panics in `load_state_from_db` | Inconsistent serialized state |
| Block execution produces different state root than peers | State divergence |
| `mdbx_chk` reports errors | Low-level database corruption |
| Receipts/balances clearly wrong (e.g. negative balance) | Partial write / rollback failure |

---

## Diagnosis Flow

```
1. Run MDBX integrity check
   $ mdbx_chk $DATA_DIR/mdbx/

2. Check for pending checkpoint (incomplete write)
   $ ls $DATA_DIR/mdbx/call_checkpoint/  # non-empty indicates unclean shutdown

3. Check last persisted block height vs consensus state
   $ curl -s http://localhost:8545 -X POST \
     -d '{"jsonrpc":"2.0","method":"call_getBlockHeight","params":[],"id":1}'
   # Compare against trusted peer; automated consistency scripts are not yet available

4. Compare state root with trusted peer
   $ curl -s http://TRUSTED_PEER:8545 -X POST \
     -d '{"jsonrpc":"2.0","method":"call_getBlockHeight","params":[],"id":1}'
```

---

## Recovery Options

### Option 1: Restart from Snapshot (Fastest — Full/Archive Nodes)

**When**: Corruption detected early; snapshot from < 1 hour ago available

**Steps**:
1. Stop node: `systemctl stop calld`
2. Backup corrupted DB (for forensics):
   ```bash
   tar czf $DATA_DIR/mdbx-corrupted-$(date +%s).tar.gz $DATA_DIR/mdbx/
   ```
3. Remove corrupted DB:
   ```bash
   rm -rf $DATA_DIR/mdbx/*
   ```
4. Restore from latest snapshot:
   ```bash
   # Snapshots are written to $DATA_DIR/snapshots/ every 1000 blocks
   latest=$(ls -t $DATA_DIR/snapshots/*.bin | head -1)
   cp "$latest" $DATA_DIR/snapshot-restore.bin
   ```
5. Start node with snapshot restore:
   ```bash
   calld --config /etc/callchain/config.toml --restore-snapshot $DATA_DIR/snapshot-restore.bin
   ```
6. Node will replay blocks from snapshot height to current network height via P2P sync

### Option 2: State Sync from Trusted Peer (No Snapshot)

**When**: No valid snapshot available; peer network healthy

**Steps**:
1. Stop node: `systemctl stop calld`
2. Backup and remove DB (as above)
3. Start node in **fast-sync mode**:
   ```bash
   calld --config /etc/callchain/config.toml --sync-mode fast
   ```
4. Node downloads block headers from P2P peers with light client verification
5. Once caught up, switch to full mode:
   ```bash
   # Edit config.toml: mode = "full"
   systemctl restart calld
   ```

### Option 3: Genesis Reset (Last Resort — Testnet Only)

**When**: Chain-wide corruption; no trusted peers with valid state

**⚠️ WARNING**: This resets ALL state. Only use on testnet or with explicit governance approval.

**Steps**:
1. Halt all validators (see [chain-halt.md](chain-halt.md))
2. All nodes remove DB:
   ```bash
   systemctl stop calld
   rm -rf $DATA_DIR/mdbx/*
   rm -rf $DATA_DIR/blocks/*
   ```
3. Ensure all nodes use identical genesis file:
   ```bash
   sha256sum /etc/callchain/genesis.json  # Verify across all nodes
   ```
4. Restart all validators simultaneously:
   ```bash
   systemctl start calld
   ```
5. Monitor for block production resumption

---

## State Validation Checklist

After recovery, verify these state components:

```bash
# 1. Balances table exists and has data
curl -s http://localhost:8545 -X POST \
  -d '{"jsonrpc":"2.0","method":"call_protocolBalance","params":[1,"0x'$(cat /dev/urandom | xxd -p | head -c 40)'"],"id":1}' | jq .result

# 2. Validator set matches expected
# Check against governance or genesis

# 3. No duplicate nullifiers (shielded state)
# Check via `call_shieldedTreeState` — nullifier_count should be monotonic

# 4. Bridge pending deposits are consistent
# Compare `call_bridgePendingCount` across peers

# 5. Receipts for last 100 blocks exist
curl -s http://localhost:8545 -X POST \
  -d '{"jsonrpc":"2.0","method":"call_getBlockReceipts","params":[$(curl -s http://localhost:8545 -X POST -d '"'"'{"jsonrpc":"2.0","method":"call_getBlockHeight","params":[],"id":1}'"'"' | jq -r .result)],"id":1}' | jq '.result | length'
```

---

## Prevention

| Measure | Implementation |
|---|---|
| Automated snapshots | Cron every 1000 blocks to `$DATA_DIR/snapshots/` |
| Checkpoint markers | `persist_state_to_db()` writes pending marker before flush, clears after |
| DB integrity checks | Run `mdbx_chk` weekly via cron |
| Monitor state root divergence | Alert if local state root != peer state root for > 5 blocks |
| Multi-node state comparison | Cross-compare balances of random addresses across 3+ nodes hourly |

---

## Tools

| Tool | Path | Purpose |
|---|---|---|
| `mdbx_chk` | System package `libmdbx-utils` | Low-level DB integrity check |
| `ls $DATA_DIR/mdbx/call_checkpoint/` | Manual | Check for incomplete writes (unclean shutdown marker) |
| Peer RPC comparison | Manual | Cross-compare `call_getBlockHeight` and state roots across nodes |
| `--restore-snapshot` flag | `calld` CLI | Automated snapshot restore on node startup |
