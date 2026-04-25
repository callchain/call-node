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

The Docker network uses private IPs (`172.28.0.x`). Without `allow_private_ips = true`, full nodes cannot establish P2P connections on the internal Docker network. (Full nodes bootstrap to validators; validators do not need to bootstrap to full nodes — this is normal design and not a bug.)

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

## 9. Full Nodes Missing from Validator Bootstrap Lists (Not a Bug)

### Observation
Validator configs only list other validators in `bootstrap_peers`. Full nodes (5, 6) are not included.

**Example from `node1.toml`:**
```toml
bootstrap_peers = "...172.28.0.12...,...172.28.0.13...,...172.28.0.14..."
```
IPs `172.28.0.15` (node5) and `172.28.0.16` (node6) are absent.

### Analysis
This is **normal design**, not a bug. In Ethereum, Cosmos, Tendermint, and most BFT networks, validators only bootstrap to other validators. Full nodes bootstrap to validators and discover additional peers via PEX. There is no requirement for validators to include full nodes in their bootstrap lists.

The actual connectivity issue is caused by 12.4 (`allow_private_ips = false`), not by missing bootstrap entries.

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
5. ~~Add full nodes to validator bootstrap lists~~ — **Not a bug.** Validators only bootstrap to other validators; full nodes bootstrap to validators. This is normal design.

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

---

## 12. Detailed Source-Code Analysis & Fix Recommendations

以下是对每个问题的源码级根因分析及具体修复建议。

### 12.1 Epoch 轮换后节点 epoch 不一致（Critical）

**源码定位：**

`crates/node/src/lib.rs:2844-2854`，当节点本地高度到达 `height % epoch_length == 0` 时，无条件发送退出信号：

```rust
if new_height % epoch_length == 0 {
    let _ = exit_tx.send(EpochRotationReason::EpochBoundary);
    break;
}
```

随后 `bft_event_loop` 和 `Engine` 被 abort（`lines 761-763`），进入下一次 loop 时 `epoch_number += 1`，重新创建 `Engine::new(Epoch::new(epoch_number))`（`line 716`）。

commonware-consensus 的 Simplex BFT 引擎只接受与自身 epoch 匹配的消息。节点 1 始终没到达 1000，停留在 epoch 0；节点 2/3/4 已轮换到 epoch 2。节点 1 发出来的消息被其他节点以 `epoch mismatch` 拒绝，导致 3 个节点在 epoch 2 也凑不齐稳定 quorum。

**修复方案：**
- **首选方案：** 不要一到达边界高度就立刻轮换 epoch。先通过心跳或同步握手确认"当前 subset 中多数节点已敲定边界块"，再统一递增 epoch。
- **备选方案：** 如果收到更高 epoch 的 BFT 消息，暂停本地共识，先触发同步追赶到目标高度，然后直接跳到正确 epoch 重新入队，而不是按顺序逐个 epoch 重启。
- **长期方案：** 去掉每次 epoch 都重启引擎的做法。用一个长期运行的 Engine，通过动态更新 `oracle.track()` 来调整 participant set，避免状态丢失。

---

### 12.2 Epoch 轮换后 BFT Block Cache 丢失（Critical）

**源码定位：**

`crates/node/src/lib.rs:2544-2549`，finalize 时从共享 cache 中**移除** block：

```rust
let mut block = {
    let mut cache = block_cache.lock().unwrap();
    cache.remove(&info.digest)
};
```

问题有两层：
1. `execution_results`（`line 2285-2288`）是定义在 `bft_event_loop` 函数内的局部变量。引擎 abort 后该 map 被销毁，新 epoch 的 event loop 从头开始。
2. 如果边界块的延迟 finalize 消息在引擎重启后才到达，block 已被旧 loop 从 cache 中移除，新 loop 的 `execution_results` 也为空。

虽然代码有磁盘 fallback（`line 2557` `load_block`），但 `persist_block`（`line 2901`）是异步写 JSON 文件，可能在引擎 abort 前还未 flush 完成，导致读盘也失败。

**修复方案：**
- **先持久化再移除：** finalize handler 里先调用 `persist_block`，确认写盘成功后再 `cache.remove()`。
- **保留边界块：** `BlockCache` 增加 `retain_recent(n: usize)` 方法，最近 N 个块不轻易移除，给跨 epoch 的延迟消息留出窗口。
- **execution_results 跨 epoch 共享：** 像 `block_cache` 一样，把 `execution_results` 提升到 `CallNode` 层级，通过 `Arc<Mutex<...>>` 共享，避免每次重启都清空。

---

### 12.3 P2P 限流导致共识/同步消息被丢弃（Critical） ✅ 已修改，待验证

**源码定位：**

`crates/network/src/p2p.rs:867-884`，`receive()` 对所有通道的消息统一走 gossip rate limiter：

```rust
if let Err(e) = peer_state.record_message() {
    tracing::warn!(peer_id = %peer_id, "gossip rate limit hit: {e}");
    return Err(e);
}
```

默认限流值 `max_messages_per_second = 100`（`crates/network/src/limits.rs:26`）。同步时一次批量 100 个块通知或 SyncResponse 很容易在 1 秒内突破限制，消息直接被丢弃。

**修复方案：**
- **按通道区分限流：** 只对 gossip 类通道（交易传播、PEX）调用 `record_message()`。`SYNC_CHANNEL`、`BLOCK_CHANNEL` 以及 BFT 的 `vote/cert/resolve` 通道应豁免该限流。
- **提高默认值或配置化：** 把 devnet 的默认限流提升到至少 1000，或从 TOML 配置读取以便 devnet 覆盖。

**已修改（2026-04-25）：**
`crates/network/src/p2p.rs` 的 `receive()` 中，限流器现在仅在 `channel == 1`（交易传播）时生效，其余通道直接放行。编译和单元测试通过（`call-network` 46/46 passed）。

---

### 12.4 Full Node 缺少 `allow_private_ips`（High） ✅ 已修改

**源码定位：**

`crates/network/src/p2p.rs:349`，`CommonwareConfig::default()` 默认 `allow_private_ips: false`。

已确认 `devnet/configs/node5.toml`、`node6.toml` 的 `[p2p]` 段均未设置此值。Docker 网络使用 `172.28.0.x`（RFC-1918 私有地址），commonware-p2p 会拒绝这类连接。

**修复：**
在 `node5.toml` 和 `node6.toml` 中添加：
```toml
[p2p]
allow_private_ips = true
```

**已修改（2026-04-25）：** `devnet/configs/node5.toml`、`node6.toml` 均已添加 `allow_private_ips = true`。

---

### 12.5 Full Node 不在 Validator 的 bootstrap 列表中（Low — 非问题）

**已确认：**

- `node1.toml` bootstrap_peers：`.12, .13, .14`（无 `.15`、`.16`）
- `node2.toml` bootstrap_peers：`.11, .13, .14`（无 `.15`、`.16`）
- node3/4 同理。

**分析：**
这是**正常设计**，不是 bug。Validator 的 bootstrap 列表只包含其他 validators 是行业标准（Ethereum、Cosmos、Tendermint 等均如此）。Full nodes 主动连接 validators 获取 blocks 即可，validator 不需要回 dial full nodes。

Full node 只要能连上至少一个 validator（或另一个 full node），就能通过 PEX 发现更多 peers 并正常同步。在 12.4 修复后（`allow_private_ips = true`），full nodes 的 outbound 连接不再被阻止，该问题已自然消失。

**结论：** 无需修改 validator 的 bootstrap 配置。

---

### 12.6 `eth_getBlockByNumber` 返回 null / 零哈希（Medium） ✅ 已修改，待验证

**源码定位：**

`crates/rpc/src/standard.rs:365-368`，历史块直接返回 null：

```rust
if block_number != current {
    return Ok(serde_json::Value::Null);
}
```

对于当前块，hash 字段全部硬编码为零：

```rust
"hash": format!("0x{}", hex::encode([0u8; 32])),
"parentHash": format!("0x{}", hex::encode([0u8; 32])),
```

虽然 `persist_block`/`load_block` 已在 `crates/node/src/lib.rs:2901-2917` 实现，但 RPC handler 完全没有调用 `load_block`。

**修复：**
- 非当前高度时从磁盘加载：
  ```rust
  if block_number != current {
      if let Some(block) = load_block(&state.data_dir, block_number) {
          return Ok(block_to_json(&block, full_txs));
      }
      return Ok(serde_json::Value::Null);
  }
  ```
- 当前高度时，使用 `state.parent_hash` 和实际 block hash，而不是硬编码零值。`RpcState` 在每次 finalize 后都会更新 `parent_hash`（`lib.rs:2697`）。

**已修改（2026-04-25）：**
- `crates/rpc/src/handlers.rs`：`RpcState` 新增 `data_dir` 和 `load_block` 方法。
- `crates/node/src/lib.rs`：创建 `RpcState` 后注入 `data_dir`。
- `crates/rpc/src/standard.rs`：`eth_getBlockByNumber` 优先调用 `state.load_block()` 从磁盘加载，返回真实 `hash`/`parentHash`/`stateRoot`/`receiptsRoot`/`transactionsRoot`；若磁盘缺失再回退到 stub。
- 编译和单元测试通过（`call-rpc` 16/16 passed，`call-node` passed）。

---

### 12.7 节点 1 Validator Set 分叉（Medium） ✅ 已修改，待验证

**根因推测：**

节点 1 报告 5 个 validator（包含一个与 validatorId 3 地址相同但 ed25519 pubkey 不同的条目），说明节点 1 执行了一条其他节点未执行的 `ValidatorStake` 交易。

最可能的触发点在 `crates/node/src/lib.rs:2398-2414` —— `propose` 阶段直接对本地共享的 `validator_state` 做了**原地修改**。如果节点 1 提议了一个包含 stake 交易的块，但其他节点的 BFT voter 因 verify 失败拒绝了该提案，节点 1 的本地状态已被修改且没有回滚。

目前 `apply_rollback_plan`（`line 2311`）只处理紧急回滚，不处理"提议成功但未 finalize"的场景。

**修复：**
- **propose 阶段不修改共享状态。** 先在临时副本上执行交易，生成 block digest 后返回。只有在 `finalize` handler 里（该块已获得 BFT 多数确认）才真正提交状态变更。`finalize` 里已经有重执行或缓存结果的分支，可以直接复用。
- 或者在 block header 中加入 state root，在 `verify` 阶段校验。若状态分叉，该块在 commit 前就会被拒绝。

**已修改（2026-04-25）：**
- 完整实现了状态隔离 + State Root 校验方案（详见 #14）。
- `propose`/`verify` 改为在克隆状态上执行，`finalize` 独占共享状态写入。
- 新增 `state_root` 到 `BlockHeader`，`verify`/`sync`/`finalize` 均做 root 校验。
- 新增 3 个状态隔离单元测试，全部通过。
- 编译和全量测试通过（`call-consensus` 69/69，`call-node` 60/60 passed）。

---

## 13. 修复优先级汇总

| 优先级 | 问题 | 需修改的文件 |
|--------|------|-------------|
| P0 | Epoch 轮换 — 等待 quorum 再换 epoch | `crates/node/src/lib.rs` |
| P0 | Block cache — 持久化后再移除、保留边界块 | `crates/node/src/lib.rs`, `crates/consensus/src/block_cache.rs` |
| P0 | P2P 限流 — 同步/共识通道豁免 ✅ 已改，待验证 | `crates/network/src/p2p.rs` |
| P1 | Full node 缺少 `allow_private_ips` | `devnet/configs/node5.toml`, `node6.toml` | ✅ 已修改 |
| — | Validator bootstrap 列表补全 full node | — | 非问题，无需修改 |
| P2 | `eth_getBlockByNumber` 从磁盘加载、返回真实 hash ✅ 已改，待验证 | `crates/rpc/src/standard.rs` |
| P2 | propose 阶段隔离状态修改，防止 validator set分叉 | `crates/node/src/lib.rs` 以及多个 crate | ✅ 已改，编译通过，60/60 测试通过 |

## 14. Callchain 状态隔离 + State Root 校验方案

### Context

Callchain 的 BFT propose/verify 阶段直接对共享状态执行 `block.execute()`，导致状态在共识达成前就被修改。这是节点 1 validator set 分叉的根因（节点 1 propose 了一个 stake 交易，P2P 广播被 drop，但本地状态已被修改）。

参考实现的状态隔离原则：
- **arc-node**: propose/verify 通过 Engine API 生成/验证 payload，不修改 CL 共识状态；只在 `Decided` (finalize) 后通过 `forkchoice_updated` 提交。
- **tempo**: propose/verify 在**克隆的 `Inner<TState>`** 和**临时的 `reth_revm::State`** 上运行，丢弃后即销毁；Executor Actor 顺序处理 finalize。
- **Ethereum**: 执行仅在 propose 阶段计算 state root；verify 通过校验 header 中的 state root 避免重执行；只有 finalize 才将 state changes 写入 canonical state。

### Goal

设计一个**短期可实施**（修改范围小）+ **长期正确**（state root 校验）的方案，修复 propose/verify/finalize 的状态隔离问题。

---

### Phase 1: 状态隔离（核心修复，短期可实施）

#### 核心原则

- **propose**: 在**状态克隆**上执行 block，只缓存 block + execution result，**不修改共享状态**
- **verify**: 在**状态克隆**上验证 block（或信任本地缓存），**不修改共享状态**
- **finalize**: **在共享状态上执行 block**（或重新执行），真正提交状态变更
- **sync**: 验证 state root 后，在共享状态上执行并提交

#### 1.1 为缺少 Clone 的状态类型添加 Clone

| 文件 | 类型 | 修改 |
|------|------|------|
| `crates/protocol/src/registry.rs:35` | `AssetRegistry` | `#[derive(Debug, Default, Clone)]` |
| `crates/agent/src/balances.rs:13` | `AgentBalances` | `#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]` |
| `crates/agent/src/registry.rs` | `AgentRegistry` | 手动实现 `Clone`（`Box<dyn DomainVerifier>` 不可 clone，设为 `None`） |
| `crates/consensus/src/fork.rs` | `ForkManager` / `RollbackRecord` | `#[derive(Debug, Clone, ...)]` |
| `crates/oracle/src/lib.rs:117` | `OracleManager` | `#[derive(Debug, Clone, Serialize, Deserialize)]` |

`ValidatorStateManager`、`EvmState`、`BridgeStateManager`、`ShieldedState`、`ComplianceEngine`、`FeeParams` 已实现 `Clone`，无需修改。

#### 1.2 修改 propose handler (`crates/node/src/lib.rs`)

当前代码对所有状态组件使用 `write()` 锁，执行后共享状态已被修改。

**改为只读克隆模式：**

```rust
// 从共享状态读取快照（read lock + clone），不修改原状态
let mut balances = state.balance_state.read().unwrap().clone();
let mut registry = state.asset_registry.read().unwrap().clone();
let mut compliance = state.compliance_engine.read().unwrap().clone();
let mut bridge_state = state.bridge_state.read().unwrap().clone();
let mut shielded_state = state.shielded_state.read().unwrap().clone();
let mut fee_params = state.fee_params.read().unwrap().clone();
let mut evm_state = state.evm_state.read().unwrap().clone();
let mut oracle = state.oracle.read().unwrap().clone();
let mut agent_balances = state.agent_balances.read().unwrap().clone();
let mut agent_registry = state.agent_registry.read().unwrap().clone();
let mut validator_state = state.validator_state.read().unwrap().clone();

// 在克隆上执行——只验证 block 有效性和计算 state roots
let result = block.execute(
    &mut balances, &mut registry, &mut compliance,
    &mut bridge_state, &mut shielded_state, &mut fee_params,
    height, &mut evm_state, Some(&mut oracle), None,
    Some(&mut agent_balances), Some(&mut agent_registry),
    None, None, None, None,
    Some(&mut validator_state),
    None,
)?;

// 将 roots 写入 block header（header 成为 block 的"承诺"）
block.finalize(&result);
let digest = ConsensusDigest::from(block.header.hash());

// 缓存 block 和 result，供 verify/finalize 使用
block_cache.lock().unwrap().insert(digest, block);
execution_results.insert(digest, result);

// 共享状态完全没有被修改
let _ = reply_tx.send(digest);
```

**关键变化：**
- 所有 `write().unwrap()` 改为 `read().unwrap().clone()`
- `block.finalize(&result)` 把 roots 写入 header，使 block 成为自包含的"承诺"
- `oracle.advance_period()` 和 `distribute_rewards()` 等副作用从 propose 阶段**移除**，移到 finalize 阶段

#### 1.3 修改 verify handler (`crates/node/src/lib.rs`)

当前代码同样使用 `write()` 锁直接修改共享状态。

**改为轻量验证（本地 block）或 state root 校验（外部 block）：**

```rust
Some((digest, reply_tx)) = verify_rx.recv() => {
    let block = {
        let cache = block_cache.lock().unwrap();
        cache.get(&digest).cloned()
    };

    let valid = if let Some(block) = block {
        // 本地缓存的 block 是自己 propose 的，已经验证过
        // 但做一次 state root 一致性校验更安全
        let mut balances = state.balance_state.read().unwrap().clone();
        // ... 其余状态同样克隆 ...

        match block.execute(/* ... 克隆状态 ... */) {
            Ok(result) => {
                // State Root 校验：确保 proposer 诚实地计算了 roots
                result.payment_root == block.header.payment_root
                    && result.evm_state_root == block.header.evm_state_root
                    && result.bridge_root == block.header.bridge_root
                    && result.receipt_root == block.header.receipt_root
            }
            Err(_) => false,
        }
    } else {
        // 外部 block 不在本地 cache（当前行为返回 false）
        // TODO: 未来从网络获取 block 后做 state root 校验
        false
    };

    let _ = reply_tx.send(valid);
}
```

**关键变化：**
- 同样使用 `read().unwrap().clone()`，不修改共享状态
- 新增 **State Root 校验**：重新执行 block（在克隆上），比较 computed roots 与 header 中已写入的 roots
- 如果 mismatch，说明 proposer 作弊或状态分叉，返回 false

#### 1.4 修改 finalize handler (`crates/node/src/lib.rs`)

finalize 是**唯一修改共享状态**的阶段。当前代码有两种路径：
1. cache hit (`execution_results.remove`): 直接用 cached result，**不重新执行**
2. cache miss: 重新执行 block，**修改共享状态**

**问题分析：** 路径 1 不执行 block，意味着共享状态**从未被更新**（但以前 propose 阶段已偷偷修改了，所以看起来正常）。路径 2 是正确的行为，但只在 cache miss 时触发。

**修正后：finalize 总是（重新）执行 block 在共享状态上**

```rust
Some(info) = finalize_rx.recv() => {
    let mut block = {
        let mut cache = block_cache.lock().unwrap();
        cache.remove(&info.digest)
    };

    // Cache miss: 从 disk 恢复
    if block.is_none() {
        let height = { consensus.read().unwrap().current_height() };
        if let Some(b) = load_block(&data_dir, height) {
            block = Some(b);
        } else {
            // 触发 sync ...
            continue;
        }
    }

    if let Some(mut block) = block {
        let height = block.header.height;

        // 检查是否已 finalize 过（防重放）
        let current_height = { let c = consensus.read().unwrap(); c.current_height() };
        if height < current_height { continue; }

        // === 核心：在共享状态上执行 block（真正的状态提交）===
        let mut balances = state.balance_state.write().unwrap();
        // ... 其余状态 write lock ...

        let result = match block.execute(/* ... 共享状态 ... */) {
            Ok(r) => r,
            Err(e) => { tracing::warn!(...); continue; }
        };

        // 校验执行结果与 header 中的 state roots 一致
        if result.payment_root != block.header.payment_root
            || result.evm_state_root != block.header.evm_state_root
            || result.bridge_root != block.header.bridge_root
            || result.receipt_root != block.header.receipt_root
        {
            tracing::error!(height, "BFT finalize: state root mismatch — block rejected");
            continue;
        }

        // Oracle 周期边界处理（从 propose 阶段移到这里）
        if height.is_multiple_of(ORACLE_UPDATE_INTERVAL) {
            oracle.advance_period(height);
            // ... 异常值处理 + 奖励分发 ...
        }

        // 提交到共识层
        {
            let mut c = consensus.write().unwrap();
            c.commit_block(&block, &result)?;
        }

        // 审计日志、状态推进、持久化（保持现有逻辑）
    }
}
```

**关键变化：**
- 不再使用 `execution_results.remove()` 的 cached result 路径
- 总是在共享状态上重新执行 block
- 执行后校验 state roots 与 header 匹配（安全性检查）
- Oracle 边界处理从 propose 移到 finalize
- `execution_results` HashMap 在 epoch 结束后会被丢弃（当前行为），这没问题——它只是 propose/verify 的临时缓存

#### 1.5 修改 sync handler (`crates/node/src/lib.rs`)

Sync 接收的是已经 finalized 的 block，应该直接应用。但需要验证 state root 防止恶意节点注入错误数据。

```rust
// State Root 验证：在克隆上执行一次，校验 roots 匹配
let roots_valid = {
    let mut balances = state.balance_state.read().unwrap().clone();
    // ... 其余状态同样克隆 ...
    match block.execute(/* ... 克隆状态 ... */) {
        Ok(result) => {
            result.payment_root == block.header.payment_root
                && result.evm_state_root == block.header.evm_state_root
                && result.bridge_root == block.header.bridge_root
                && result.receipt_root == block.header.receipt_root
        }
        Err(_) => false,
    }
};

if !roots_valid {
    tracing::error!(height = block_height, "sync: state root mismatch");
    break;
}

// 在共享状态上执行（真正的状态提交）
let execute_result = block.execute(/* ... 共享状态 ... */);
```

#### 1.6 添加 commit_block 高度防重放 (`crates/consensus/src/simplex.rs`)

```rust
pub fn commit_block(&mut self, block: &Block, result: &BlockExecutionResult) -> Result<(), ConsensusError> {
    // 防重放：只能提交当前高度的 block
    if block.header.height != self.current_height {
        return Err(ConsensusError::InvalidBlock(format!(
            "height mismatch: expected {}, got {}",
            self.current_height, block.header.height
        )));
    }
    // ...（保持现有逻辑）...
}
```

---

### Phase 2: State Root 校验（长期优化）

Phase 1 已经在 verify 和 sync 阶段引入了 state root 校验。Phase 2 进一步优化，使 verify 阶段**不需要完整重新执行 block**。

#### 2.1 统一 State Root

当前 `BlockHeader` 有四个独立的 root 字段。已添加统一的 `state_root`：

```rust
pub struct BlockHeader {
    // ... existing fields ...
    pub state_root: Hash,  // keccak256(payment_root || evm_state_root || bridge_root || receipt_root)
    // ...
}
```

在 `Block::finalize()` 中计算：
```rust
pub fn finalize(&mut self, result: &BlockExecutionResult) {
    self.header.payment_root = result.payment_root;
    self.header.evm_state_root = result.evm_state_root;
    self.header.bridge_root = result.bridge_root;
    self.header.receipt_root = result.receipt_root;
    self.header.state_root = keccak256(&[
        self.header.payment_root.as_slice(),
        self.header.evm_state_root.as_slice(),
        self.header.bridge_root.as_slice(),
        self.header.receipt_root.as_slice(),
    ].concat());
}
```

#### 2.2 轻量 Verify（未来优化）

当 block 从网络接收时，如果只需验证 `state_root` 的格式正确性（非零、符合规范），可以跳过完整重执行。但这降低了安全性——恶意 proposer 可以伪造 state_root。

更安全的方案是 **Merkle Proof 验证**（类似 Ethereum 的 state trie）：
- 每个状态组件（balances, validators 等）维护 Merkle trie
- `payment_root` 是 balance trie 的根
- `evm_state_root` 是 EVM state trie 的根
- verify 阶段只需校验 Merkle proof，不需要完整重执行

这需要将 `HashMap`-based 状态改为 `MerkleTrie`-based 状态，改动较大，适合作为长期重构。

---

### 方案对比总结

| 特性 | arc-node | tempo | Ethereum | **Callchain（本方案）** |
|------|----------|-------|----------|------------------------|
| propose 状态隔离 | Engine API，不修改 CL 状态 | 克隆 `Inner` + 临时 `reth_revm::State` | 临时状态计算 state root | **克隆所有状态，不修改共享状态** |
| verify 验证方式 | Engine API 验证 payload | 克隆状态上验证 | 校验 state root | **克隆状态执行 + state root 校验** |
| finalize 状态提交 | `forkchoice_updated` → canonical | Executor actor 顺序处理 | canonical state 更新 | **共享状态上重新执行 + state root 校验** |
| sync 验证 | 通过 Engine API 导入 | 信任 sync 层 + 执行验证 | state root + Merkle proof | **克隆验证 roots + 共享状态执行** |

---

### 修改文件清单

| 文件 | 修改内容 | 优先级 | 状态 |
|------|----------|--------|------|
| `crates/protocol/src/registry.rs` | `AssetRegistry` 加 `Clone` | P0 | ✅ 已完成 |
| `crates/agent/src/balances.rs` | `AgentBalances` 加 `Clone` | P0 | ✅ 已完成 |
| `crates/agent/src/registry.rs` | `AgentRegistry` 手动实现 `Clone` | P0 | ✅ 已完成 |
| `crates/consensus/src/fork.rs` | `ForkManager` / `RollbackRecord` 加 `Clone` | P0 | ✅ 已完成 |
| `crates/oracle/src/lib.rs` | `OracleManager` 加 `Clone` | P0 | ✅ 已完成 |
| `crates/node/src/lib.rs` | propose handler 改为克隆执行 | P0 | ✅ 已完成 |
| `crates/node/src/lib.rs` | verify handler 改为克隆 + state root 校验 | P0 | ✅ 已完成 |
| `crates/node/src/lib.rs` | finalize handler 改为共享状态执行 + root 校验 | P0 | ✅ 已完成 |
| `crates/node/src/lib.rs` | sync handler 添加 state root 验证 | P0 | ✅ 已完成 |
| `crates/consensus/src/simplex.rs` | `commit_block` 添加高度防重放 | P1 | ✅ 已完成 |
| `crates/consensus/src/block.rs` | 添加统一 `state_root` 字段到 `BlockHeader` | P2 | ✅ 已完成 |

---

### 验证记录

1. **编译检查**: `cargo check -p call-node -p call-consensus` ✅ 通过
2. **单元测试**: `cargo test -p call-node -p call-consensus` ✅ 全部通过（`call-consensus` 69/69，`call-node` 60/60）
3. **状态隔离测试**（新增）：
   - `test_state_isolation_propose_does_not_modify_shared_state` — propose 后共享状态未被修改，finalize 后已被修改 ✅
   - `test_state_root_mismatch_rejects_block` — 篡改 header root 后重新执行检测 mismatch ✅
   - `test_commit_block_height_replay_protection` — 同一 block 重复 commit 被高度检查拒绝 ✅
4. **State Root 校验测试**: 修改一个 state root 后验证 block 被拒绝 ✅（通过 `test_state_root_mismatch_rejects_block`）
5. **Devnet 验证**: 待运行 6 节点 devnet 确认
