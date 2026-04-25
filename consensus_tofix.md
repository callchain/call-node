# Callchain Consensus & Devnet Issues — Summary

## 1. Overview

The 6-node Docker devnet (`devnet/docker-compose.yml`) is **completely stalled**. No blocks have been produced since the epoch rotation at height 1000. Nodes are split across multiple epochs and cannot reach consensus.

---

## 2. Current Node Status

| Container | Role | Config | RPC Port | Block Height | Status |
|-----------|------|--------|----------|-------------|--------|
| `callchain-node1` | Validator | `node1.toml` | 5005 | **0x305 (773)** | Stuck on old epoch |
| `callchain-node2` | Validator | `node2.toml` | 5007 | **0x3e8 (1000)** | Stuck post-epoch rotation |
| `callchain-node3` | Validator | `node3.toml` | 5009 | **0x3e8 (1000)** | Stuck post-epoch rotation |
| `callchain-node4` | Validator | `node4.toml` | 5011 | **0x3e8 (1000)** | Stuck post-epoch rotation |
| `callchain-node5` | Full node | `node5.toml` | 5013 | **0x0** | Stuck at genesis |
| `callchain-node6` | Full node | `node6.toml` | 5015 | **0x0** | Stuck at genesis |

**Observation:** Block heights have not changed for 15+ minutes. All nodes are frozen.

---

## 3. Core Issue: Epoch Desync After Height 1000

### What happened
- Block 1000 committed successfully on nodes 2, 3, 4
- The epoch boundary triggered a **participant subset rotation** (`epoch=1` -> `epoch=2`)
- Node 2 restarted its BFT engine for the new epoch
- Node 1 never reached height 1000 (stuck at 773) and remains on **epoch 0**

### Symptom logs (node 2)
```
INFO call_consensus::simplex: committed block height=1000 round=1000
INFO call_node: BFT: epoch boundary reached, rotating participant subset epoch=2 height=1000
INFO call_node: BFT: engine exited for epoch rotation r=EpochBoundary epoch=1
INFO call_node: BFT: selected for epoch, starting engine epoch=2 subset_size=4
```

### Symptom logs (nodes 3, 4)
```
WARN commonware_consensus::simplex::actors::batcher::actor: epoch mismatch peer=4cb5abf6...
WARN commonware_consensus::simplex::actors::voter::actor: proposal failed verification
    round=Round { epoch: Epoch(1), view: View(1085) }
```

### Root cause
Node 1 is on a **different epoch** than nodes 2/3/4. The Simplex BFT protocol rejects messages from peers with mismatched epochs. With 4 validators total and 1 on epoch 0, the remaining 3 in epoch 2 cannot form a stable quorum because they see epoch-mismatch messages from node 1.

---

## 4. Issue: Full Nodes (5, 6) Completely Isolated

### Symptom
- Both full nodes report `blockNumber: 0x0` forever
- No P2P activity in their logs after startup
- No warnings or errors (they are simply silent)

### Root cause
The full node configs (`node5.toml`, `node6.toml`) are **missing `allow_private_ips = true`**:

```toml
# Validator configs HAVE this:
[p2p]
allow_private_ips = true

# Full node configs are MISSING it:
[p2p]
# (no allow_private_ips setting)
```

The Docker network uses private IPs (`172.28.0.x`). Without `allow_private_ips = true`, full nodes cannot establish P2P connections on the internal Docker network. Additionally, no validator bootstrap config includes nodes 5/6 as peers, so validators never dial them.

**Fix:** Add `allow_private_ips = true` to `devnet/configs/node5.toml` and `node6.toml`.

---

## 5. Issue: BFT Block Cache Misses After Epoch Rotation

### Symptom (nodes 2, 3, 4 after height 1000)
```
WARN call_node: BFT finalize: block not in cache or disk, triggering sync digest=... height=1000
```

This floods the logs immediately after the epoch engine restarts. The BFT voter is receiving finalization certificates for blocks at height 1000, but the block cache was likely cleared during the epoch rotation engine teardown.

**Impact:** Even without the epoch desync, the restarted engine cannot finalize blocks because it lost the in-memory block cache during the `engine exited for epoch rotation` transition.

---

## 6. Issue: P2P Rate Limiting Under Load

### Symptom (all active nodes)
```
WARN call_node: p2p: message rejected by defense peer_id="..." error=peer rate limited size=835
WARN call_network::p2p: gossip rate limit hit: rate limit exceeded for peer
```

This appears during sync attempts and consensus message bursts. The P2P defense layer (`call-network`) is aggressively rate-limiting consensus and sync traffic, which:
- Prevents lagging nodes from catching up
- Drops BFT proposal/finalization messages during critical periods

**Impact:** Rate limiting is a contributing factor to why node 1 could never catch up from 773 to 1000.

---

## 7. Issue: Block Content Not Persisted/Returned

### Symptom
`eth_getBlockByNumber` returns blocks with zeroed hashes:
```json
{"number":"0x3e8","hash":"0x0000...0000","parentHash":"0x0000...0000"}
```

And `eth_getBlockByNumber("0x64")` returns **null on all nodes**, even nodes at height 1000.

**Impact:** Makes it impossible to verify block contents are consistent across nodes. Block hashes are not being computed or stored.

---

## 8. Issue: Node 1 Diverged Validator Set

### Symptom
Node 1 reports **5 validators** (including `validatorId: 4`), while nodes 2-4 report only **4 validators**.

Node 1's validator list includes an extra entry with the same address as validatorId 3 but a different ed25519 pubkey. This confirms node 1 executed a different state transition (likely a validator stake transaction that other nodes rejected or that was in a fork).

---

## 9. Issue: Full Nodes Missing from Bootstrap Lists

### Symptom
Validator configs only list other validators in `bootstrap_peers`. Full nodes (5, 6) are never dialed by anyone.

**Example from `node1.toml`:**
```toml
bootstrap_peers = "...172.28.0.12...,...172.28.0.13...,...172.28.0.14..."
```
IPs `172.28.0.15` (node5) and `172.28.0.16` (node6) are absent.

---

## 10. Recommended Fixes

### Critical (blocking consensus)
1. **Fix epoch rotation cache preservation** — The BFT engine restart at epoch boundaries clears the block cache. Blocks finalized just before rotation must be persisted to disk and reloaded into the new engine's cache.
2. **Handle lagging validators during epoch rotation** — If a validator has not caught up to the epoch boundary block, the remaining validators should either:
   - Wait for the lagging validator before rotating, OR
   - Exclude the lagging validator from the new epoch subset and proceed with the remaining quorum
3. **Fix P2P rate limiting for consensus traffic** — BFT proposal/finalization messages should bypass the aggressive rate limiter or use a higher threshold.

### High (blocking full node operation)
4. **Add `allow_private_ips = true` to full node configs** (`node5.toml`, `node6.toml`)
5. **Add full nodes to validator bootstrap lists** so validators can dial them back

### Medium (data quality)
6. **Fix `eth_getBlockByNumber` to return real block hashes** — The zeroed hash suggests block hash computation or storage is not wired up correctly
7. **Fix `eth_getBlockByNumber` for historical blocks** — Block 100 should exist on nodes at height 1000 but returns null

---

## 11. Files to Inspect

| File | Relevance |
|------|-----------|
| `crates/consensus/src/simplex/` | Epoch rotation logic, engine restart, cache management |
| `crates/network/src/p2p.rs` | P2P rate limiting and defense configuration |
| `crates/node/src/boot.rs` | BFT engine lifecycle, epoch boundary handling |
| `devnet/configs/node5.toml` | Missing `allow_private_ips` |
| `devnet/configs/node6.toml` | Missing `allow_private_ips` |
| `crates/rpc/src/eth.rs` | `eth_getBlockByNumber` implementation |
| `crates/storage/src/` | Block persistence and retrieval |
