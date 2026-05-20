# Runbook: Chain Halt Recovery

**Severity**: Critical
**Scope**: All validator and full nodes
**Owner**: On-call protocol engineer
**Last updated**: 2026-04-20

---

## Symptoms

| Symptom | Likely Cause |
|---|---|
| No new blocks for > 2 × `block_time_millis` (500ms) | Consensus stall |
| `call_getBlockHeight` RPC returns stale value | BFT engine stopped |
| Prometheus alert `callchain_blocks_produced_total` flat | Proposer not producing |
| P2P peers > 0 but no block announcements | Internal consensus failure |
| All validators report "not our turn" | Proposer subset empty or corrupt |

---

## Diagnosis Flow

```
1. Check RPC health
   $ curl -s http://localhost:8545 -X POST \
     -d '{"jsonrpc":"2.0","method":"call_getBlockHeight","params":[],"id":1}'

2. Check if node is running
   $ pgrep -f calld

3. Check logs for consensus errors
   $ tail -n 500 /var/log/callchain/calld.log | grep -E "consensus|BFT|epoch"

4. Check peer connectivity
   $ curl -s http://localhost:8545 -X POST \
     -d '{"jsonrpc":"2.0","method":"net_peerCount","params":[],"id":1}'

5. Check if checkpoint marker exists (unclean shutdown)
   $ ls $DATA_DIR/mdbx/call_checkpoint/
```

---

## Common Causes and Fixes

### Cause A: > 1/3 Validators Offline

**Diagnosis**: `net_peerCount` low; logs show "not enough peers for quorum"

**Fix**:
1. Identify offline validators via validator set RPC
2. Contact offline validators via pagerduty/Slack
3. If validators cannot return within 10 minutes, coordinate **emergency pause**:
   ```bash
   # Any online validator initiates emergency pause
   cast send $GOVERNANCE_ADDR "emergencyPause(string)" "critical: >1/3 validators offline"
   ```
4. Once validators recover, resume via `GovernanceEmergencyResume`

### Cause B: Consensus Bug / Invalid Block

**Diagnosis**: Logs show "block execution failed" with repeated `ConsensusError`; block cache contains invalid block

**Fix**:
1. Stop the node: `kill -TERM $(pgrep -f calld)`
2. Block cache is in-memory; it is cleared automatically on process restart. No manual cleanup needed.
3. Check for pending checkpoint marker:
   ```bash
   # If call_checkpoint table has entries, previous shutdown was unclean
   # Node will auto-recover from genesis on next start
   ```
4. Restart node: `calld --config /etc/callchain/config.toml`
5. If auto-recovery fails, see [state-corruption.md](state-corruption.md)

### Cause C: BFT Engine Epoch Rotation Failure

**Diagnosis**: Logs show "epoch rotation failed" or "validator set mismatch"

**Fix**:
1. Restart the node process: `kill -TERM $(pgrep -f calld)` then `calld --config /etc/callchain/config.toml`
2. On restart, the node loads persisted consensus state from MDBX and continues from last committed height
3. If the issue persists, check for corrupt validator set state in MDBX and consider state sync from a trusted peer

### Cause D: Network Partition

**Diagnosis**: Node has peers but they are all on the "wrong" side of a partition; proposer not in connected set

**Fix**:
1. Check bootstrap peer connectivity
2. Verify `bootstrap_peers` in config are healthy
3. Restart P2P layer if needed (node restart)

---

## Verification

After recovery:

```bash
# 1. Block height advancing
watch -n 1 'curl -s http://localhost:8545 -X POST \
  -d "{\"jsonrpc\":\"2.0\",\"method\":\"call_getBlockHeight\",\"params\":[],\"id\":1}" | jq .result'

# 2. Peers connected
curl -s http://localhost:8545 -X POST \
  -d '{"jsonrpc":"2.0","method":"net_peerCount","params":[],"id":1}' | jq .result

# 3. No error logs
tail -n 100 /var/log/callchain/calld.log | grep -i error || echo "No errors"

# 4. Mempool accepting txs
curl -s http://localhost:8545 -X POST \
  -d '{"jsonrpc":"2.0","method":"txpool_status","params":[],"id":1}' | jq .result
```

---

## Post-Incident

1. Document halt duration, root cause, and fix in incident tracker
2. If caused by software bug, file issue at https://github.com/anthropics/callchain/issues
3. If caused by validator misconfiguration, update validator onboarding docs
4. Review whether governance parameters (timelock, emergency pause threshold) were appropriate

---

## Emergency Contacts

| Role | Contact | Escalation |
|---|---|---|
| Protocol Lead | `#protocol-oncall` Slack | +1 hour no response |
| Infrastructure | `#infra-oncall` Slack | +30 min no response |
| Security | security@callchain.org | Immediate for fund-at-risk |
