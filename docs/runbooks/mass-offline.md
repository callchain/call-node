# Runbook: Mass Validator Offline Event

**Severity**: Critical
**Scope**: Entire validator set (or > 1/3)
**Owner**: Protocol lead + validator ops
**Last updated**: 2026-04-20

---

## Thresholds

| Offline Fraction | Impact | Response |
|---|---|---|
| < 1/3 | No finality impact | Monitor, contact validators |
| = 1/3 | Stall risk if one more fails | Escalate to validator ops |
| > 1/3 | Consensus halts (no new blocks) | Emergency procedures |
| > 2/3 | Complete halt, safety at risk | Full incident response |

---

## Immediate Response (> 1/3 Offline)

### Step 1: Confirm the Scale

```bash
# Get validator set and online status
curl -s http://LOCAL_VALIDATOR:8545 -X POST \
  -d '{"jsonrpc":"2.0","method":"call_validatorSet","params":[],"id":1}' | jq '.result[] | {id, address, online}'

# Count online validators
online_count=$(curl -s http://LOCAL_VALIDATOR:8545 -X POST \
  -d '{"jsonrpc":"2.0","method":"call_validatorSet","params":[],"id":1}' | jq '[.result[] | select(.online==true)] | length')
total_count=$(curl -s http://LOCAL_VALIDATOR:8545 -X POST \
  -d '{"jsonrpc":"2.0","method":"call_validatorSet","params":[],"id":1}' | jq '.result | length')
echo "$online_count / $total_count validators online"
```

### Step 2: Initiate Emergency Pause (if > 1/3 offline and expected recovery > 15 min)

Any online validator can initiate emergency pause:

```bash
# Via governance contract
cast send $GOVERNANCE_ADDR \
  "emergencyPause(string)" \
  "mass validator offline: $online_count/$total_count online"
```

This pauses:
- Bridge deposits/withdrawals (prevents double-spend if validators recover inconsistently)
- Asset registrations
- Governance parameter changes

Normal transfers and EVM execution continue.

### Step 3: Coordinate Validator Recovery

**Communication channels** (in priority order):
1. `#validators-emergency` Discord/Slack
2. Validator ops phone bridge
3. PagerDuty escalation policy

**For each offline validator**:
- Check if hardware/cloud provider issue (AWS/GCP status pages)
- Check if network connectivity issue (DDoS, routing)
- Check if software crash (OOM, panic, bug)
- Provide restart instructions if needed

### Step 4: Monitor Recovery

```bash
# Watch validator count
watch -n 5 'curl -s http://LOCAL_VALIDATOR:8545 -X POST \
  -d '"'"'{"jsonrpc":"2.0","method":"call_validatorSet","params":[],"id":1}'"'"' | \
  jq "[.result[] | select(.online==true)] | length"'

# Watch for block production resumption
watch -n 1 'curl -s http://LOCAL_VALIDATOR:8545 -X POST \
  -d '"'"'{"jsonrpc":"2.0","method":"call_getBlockHeight","params":[],"id":1}'"'"' | jq .result'
```

### Step 5: Resume Normal Operations

Once > 2/3 validators are online and blocks are producing:

1. Submit `GovernanceEmergencyResume` from any validator:
   ```bash
   cast send $GOVERNANCE_ADDR "emergencyResume()"
   ```

2. Verify bridge operations resume:
   ```bash
   curl -s http://LOCAL_VALIDATOR:8545 -X POST \
     -d '{"jsonrpc":"2.0","method":"call_bridgeStatus","params":[],"id":1}' | jq .result
   ```

3. Check for any orphaned bridge deposits (deposits queued during pause):
   ```bash
   curl -s http://LOCAL_VALIDATOR:8545 -X POST \
     -d '{"jsonrpc":"2.0","method":"call_bridgePendingCount","params":[],"id":1}' | jq .result
   ```

---

## Scenario: Cloud Provider Outage

**Example**: AWS us-east-1 region down, 40% of validators hosted there.

1. **Immediate**: Online validators in other regions initiate emergency pause
2. **Short-term**: Coordinate validator migration to unaffected regions
3. **Recovery**: Once migrated validators are synced and staking, resume operations
4. **Long-term**: Update validator diversity requirements (max 20% per cloud provider)

---

## Scenario: Coordinated DDoS

**Example**: All validator RPC and P2P endpoints under DDoS.

1. **Immediate**: Activate Cloudflare/infra DDoS protection
2. **Short-term**: Switch to backup endpoints (separate IP ranges)
3. **Recovery**: If DDoS subsides, validators reconnect and consensus resumes
4. **Long-term**: Implement validator endpoint diversity (multiple ISPs, anycast)

---

## Scenario: Software Bug Causing Panic

**Example**: New release has consensus bug causing validator crashes.

1. **Immediate**: Emergency pause by online validators
2. **Short-term**: Identify bug, prepare hotfix
3. **Recovery**: All validators downgrade to last stable release or apply hotfix
4. **Long-term**: Strengthen release testing (canary deployments, extended testnet)

---

## Prevention

| Measure | Target |
|---|---|
| Validator geographic diversity | < 30% in any single region |
| Cloud provider diversity | < 30% on any single provider |
| Hardware diversity | Mix of cloud, bare metal, colo |
| Network diversity | Multiple ISPs, anycast endpoints |
| Automated health checks | Alert if validator offline > 2 minutes |
| Standby validators | Maintain 110% capacity (N+1) |
| Regular DR drills | Quarterly mass-offline simulation |

---

## Post-Incident Review

Document:
1. Root cause (hardware, network, software, human error)
2. Time to detection (TTD)
3. Time to recovery (TTR)
4. Effectiveness of emergency pause
5. Communication gaps
6. Action items for prevention
