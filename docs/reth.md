# call-node 深度复用 reth 设计与实现路径

**Scope**: 从自研 EVM 堆栈一次性迁移到深度复用 reth 的技术方案
**Last Updated**: 2026-05-05

---

## 一、现状对比

| 维度 | tempo | arc-node | call-node 当前 | call-node 目标 |
|---|---|---|---|---|
| **状态存储** | reth MDBX + reth-provider | reth MDBX + reth-provider | ✅ MDBX 主存储 (`InMemoryStateProvider` + `CacheDB`) | reth MDBX + reth-provider |
| **执行层** | revm + StateProviderDatabase | revm + StateProviderDatabase | ✅ `CacheDB<StateProviderDatabase>` 直接操作 MDBX | revm + StateProviderDatabase |
| **Trie 实现** | reth-trie (sparse trie 优化) | reth-trie | ✅ reth-trie 增量更新 (`TrieUpdates` 持久化到 MDBX) | reth-trie (incremental) |
| **State Root** | reth-trie 增量更新 | reth-trie 增量更新 | ✅ `compute_state_root_with_updates()` 增量计算 | reth-trie 增量更新 |
| **历史状态** | 归档节点（全量 MDBX） | 可配置 pruning/archive | ✅ `AccountHistory`/`StorageHistory` diff 表 + 128-block 快照 | 128-block 剪枝归档或全归档 |
| **Proof 支持** | `eth_getProof` 真 Merkle Proof | `eth_getProof` 真 Merkle Proof | ✅ `eth_getProof` 从持久化 trie 节点读取 | `eth_getProof` 真 Merkle Proof |
| **共识耦合** | Commonware (外部) | Malachite (外部) | ✅ `Block::execute` 包含所有状态变更，`commit_block` 纯 BFT | 自研共识通过 BlockExecutor |
| **Precompile 状态访问** | revm JournalTr | revm JournalTr | ⚠️ `StorageCtx` TLS (`scoped_thread_local!`) 仍保留 | revm JournalTr |
| **Gas 管理** | revm 自动 | revm 自动 | ⚠️ 手动 `add_gas()` 仍保留 | revm 自动 |

**关键结论**：P0（执行层+状态存储）、P1（历史状态+Proof）和 P2（BlockExecutor 与共识解耦）均已完成。

---

## 二、已完成阶段

### Phase 1：执行层替换 ✅（2026-05-05 完成）

- `execute_tx_db` / `execute_tx_provider` 使用 `CacheDB<StateProviderDatabase>` 直接操作 MDBX
- `EvmExecutor` 保留但所有生产路径走 provider/CacheDB
- `sync_to_revm_db` / `apply_from_revm_state` 从生产路径移除

### Phase 2：状态存储替换 ✅（2026-05-05 完成）

- `EvmState` 从生产代码删除（仅存于测试模块）
- MDBX `CallEvmAccounts` / `CallEvmStorage` 表为主存储
- `InMemoryStateProvider` 从 MDBX 加载，实现 reth `StateProvider` trait
- reth-trie `StateRoot::root_with_updates()` 替代 `HashBuilder` 全量聚合
- `TrieUpdates` 持久化到 `CallTrieUpdates` 表

### Phase 3：历史状态与 Proof ✅（2026-05-05 完成）

- `CallAccountHistory` / `CallStorageHistory` 表记录每块 diff
- `get_historical_account` / `get_historical_storage` 支持 blockTag 查询
- `eth_getProof` 从 `CallAccountTrie` / `CallStorageTrie` 读取节点生成 Merkle proof
- `MdbxTrieCursorFactory` + `BTreeTrieCursor` 实现 reth `TrieCursorFactory`
- Archive / Pruned 模式配置（CLI `--archive` + `retention_blocks`）
- Block 快照 `CallBlockStateSnapshots` 支持快速历史回退

---

## 三、P2 已完成：BlockExecutor 与共识解耦

### 目标

共识引擎（`SimplexConsensus`）只负责 BFT 状态机（height, round, votes, proposer subset），所有状态变更（EVM tx 执行、system settlement、validator reward）通过统一的 `BlockExecutor` 执行。

### 已完成架构

```rust
// bft_loop.rs finalize
let result = state.write_all().execute_block(&block, height)?;  // ← 包含 validator reward
// ...
let mut c = consensus.write().unwrap();
c.commit_block(&block, &result)?;  // ← 纯 BFT，不接触状态
let provider = InMemoryStateProvider::from_db(&state.db_env)?;
c.advance_round(&provider);  // ← 只读：刷新 proposer subset
```

`commit_block` 只负责 BFT 状态推进（height++, last_hash）。所有状态变更（validator reward、system settlement、EVM tx 执行）统一在 `Block::execute` 中完成，确保 state root 覆盖全部变更。

### 实施步骤

| # | 任务 | 文件 | 说明 |
|---|---|---|---|
| P2-1 | 将 validator reward 移入 `Block::execute` | `crates/consensus/src/block.rs` | reward 在 state root 计算前应用，确保 state root 包含所有变更 |
| P2-2 | `commit_block` 变为纯 BFT | `crates/consensus/src/simplex.rs` | 删除 `&mut impl ProtocolStorage` 参数和状态修改逻辑 |
| P2-3 | 分离 `advance_round` | `crates/consensus/src/simplex.rs` | `advance_round` 独立为公开方法，只读状态刷新 proposer subset |
| P2-4 | 更新 node 层调用顺序 | `crates/node/src/bft_loop.rs` | `execute_block` → `commit_block` → `advance_round` |
| P2-5 | （可选）System Contract 化 | — | 将 `state_accessors` 中的协议逻辑部署为 system contract，通过 system tx 执行 |

### 与 reth `BlockExecutor` trait 的关系

call-node 当前使用自定义 `CallchainBlockExecutor` trait（`crates/evm/src/executor.rs`），未直接对接 reth 的 `Executor` trait。原因：
- reth `Executor` 强依赖 `RecoveredBlock<NodePrimitives>` 和 `revm::State<DB>`，与 call-node 的 `Block` 类型和 MDBX 加载模式不完全兼容
- call-node 的 system settlement（base fee、oracle reward、validator reward）和 EVM tx 执行是同一流程，不需要 reth 的 `PostExecutionInput` 回调

**决策**：不直接实现 reth `Executor` trait，而是保持自定义 `Block::execute` + `CallchainBlockExecutor` 的轻量封装。如果未来需要对接 reth 的 pipeline/stages，再考虑适配层。

---

## 四、剩余根本障碍（P2 之后）

### 障碍 1：StorageCtx TLS vs revm JournalTr

**状态**：⚠️ 仍保留。precompile 通过 `scoped_thread_local!` 全局访问状态。

**影响**：revm 无法追踪 precompile 的存储访问用于 gas 计费；revert 时无法回滚 precompile 的存储变更。

**迁移策略**：
1. precompile 直接通过 `revm::JournalTr::sload`/`sstore` 访问状态
2. 协议状态访问变成普通 EVM storage slot 读写
3. 删除 `scoped_thread_local!` 和 `with_storage()`

### 障碍 2：手动 Gas 追踪 vs revm 自动 Gas

**状态**：⚠️ 仍保留。precompile 手动调用 `add_gas()`。

**影响**：与 revm 的 warm/cold gas 计算是两套独立系统。

**迁移策略**：
1. 协议存储映射为 EVM storage slot 读写
2. 由 revm 自动计费，删除所有 `add_gas()`

### 障碍 3：System Contracts（远期）

**状态**：❌ 未开始。validator staking、asset registry、bridge logic 仍通过 `state_accessors` 直接读写 EVM 存储。

**决策**：短期内保留 `state_accessors`（已整合进 `Block::execute`），长期逐步迁移为 system contract。这不是 P2 的阻塞项。

---

## 五、未完成任务清单

### P0 — 执行层与状态存储 ✅（全部完成）

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 1 | `EvmExecutor` 改为 `StateProviderDatabase` 执行路径 | ✅ | `execute_tx_db` / `execute_tx_provider` 使用 `CacheDB<StateProviderDatabase>` |
| 2 | 删除 `EvmState` 结构 | ✅ | `EvmState` 从生产代码删除，仅存于测试模块 |
| 3 | 状态读写全部改为 `StateProvider` trait | ✅ | `InMemoryStateProvider` 是唯一生产路径 |
| 4 | 共识层改为 `Arc<Database>` | ✅ | `SimplexConsensus` 通过 `ProtocolStorage` 读取，不持有 `EvmState` |
| 5 | MDBX 成为主存储 | ✅ | `CacheDB` 仅作执行期热点缓存 |

### P1 — 历史状态与 Proof ✅（全部完成）

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 6 | `AccountHistory`/`StorageHistory` diff 表 | ✅ | `record_revm_delta_history` + `get_historical_account`/`get_historical_storage` |
| 7 | Archive / Pruned 模式配置 | ✅ | `--archive` CLI + `retention_blocks` 配置 |
| 8 | `eth_getProof` 从持久化 trie 节点读取 | ✅ | `MdbxTrieCursorFactory` + `compute_account_proof_persistent` |
| 9 | `eth_call` / `eth_estimateGas` 改用 `StateProvider` | ✅ | `execute_tx_provider` 通过 `InMemoryStateProvider::from_db` 加载 |

### P2 — BlockExecutor 与共识解耦 ✅（已完成）

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 10 | `Block::execute` 包含 validator reward | ✅ | reward 在 state root 计算前应用，确保 state root 包含所有变更 |
| 11 | `commit_block` 纯 BFT 化 | ✅ | 删除 `&mut impl ProtocolStorage`，只保留 height/round/hash 推进 |
| 12 | `advance_round` 与 `commit_block` 分离 | ✅ | `advance_round` 独立为只读方法，调用方在 `commit_block` 后执行 |
| 13 | 共识引擎完全删除状态修改，仅负责 BFT | ✅ | `stake_validator`/`slash_*` 仍通过 executor trait 修改状态，但不在 `commit_block` 中 |
| 14 | 删除 `EvmState::clone()` 依赖 | ✅ | `EvmState` 已不存在于生产代码 |

### P3 — Precompile 重构（远期，非阻塞）

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 15 | StorageCtx TLS → revm JournalTr | ❌ | `scoped_thread_local!` 仍保留 |
| 16 | 手动 `add_gas()` → revm 自动计费 | ❌ | precompile 仍手动追踪 gas |
| 17 | System Contract 部署 | ❌ | validator staking、asset registry 等仍走 `state_accessors` |

---

## 六、参考资源

- **reth 架构**：`tempo/crates/execution/` — reth MDBX + revm 集成示例
- **共识解耦**：`arc-node/crates/executor/` — `BlockExecutor` trait 自定义实现
- **call-node 当前执行层**：`crates/evm/src/executor.rs` — `EvmExecutor::execute_tx_provider()`
- **call-node 状态层**：`crates/evm/src/provider.rs` — `InMemoryStateProvider`
- **call-node trie 层**：`crates/evm/src/trie.rs` — `MdbxTrieCursorFactory`
- **call-node precompile 状态访问**：`crates/precompile/src/storage.rs` — `StorageCtx` TLS
