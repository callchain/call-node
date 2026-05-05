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
| **Precompile 状态访问** | revm JournalTr | revm JournalTr | ✅ `EvmStorageProvider` 直接访问 `JournalTr` | revm JournalTr |
| **Gas 管理** | revm 自动 | revm 自动 | ⚠️ precompile 手动追踪 gas（正确但非自动） | revm 自动 |

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

## 四、Future Features（远期，非阻塞）

### Feature 1：Precompile Gas 自动计费

**状态**：⚠️ 当前 precompile 通过 `StorageProvider::deduct_gas()` 手动追踪 gas。

**背景**：`EvmStorageProvider` 已直接调用 `journal.sload()`/`journal.sstore()`，revm 正确记录存储访问并可 revert。但 gas 计费是手动的（warm/cold SLOAD/SSTORE 定价），而非由 revm 解释器自动扣除。

**原因**：revm 的 precompile 接口是黑盒（input + gas_limit → output + gas_used），解释器无法在 precompile 执行期间自动计费内部存储访问。这与 tempo、arc-node 的做法一致。

**何时需要**：当 precompile gas 计费需要与 EVM 解释器完全统一时（例如支持 EIP-2929 动态 warm/cold 定价的自动传播）。

### Feature 2：System Contracts

**状态**：❌ 未开始。validator staking、asset registry、bridge logic 仍通过 Rust `state_accessors` 直接读写 EVM 存储。

**背景**：当前协议逻辑（staking、slashing、asset registry、bridge settlement）以 Rust 预编译代码形式嵌入，通过 `StorageProvider` 访问 EVM storage slot。这工作正常，但与纯 EVM 生态的 system contract 模式不同。

**迁移策略**：
1. 将协议逻辑编写为 Solidity system contracts
2. 部署到固定地址（如 `0x0000...0001`）
3. 在 `Block::execute` 中作为 system transaction 调用
4. 删除对应的 Rust precompile，保留只读查询接口

**何时需要**：当 call-node 需要完全兼容 EVM 工具链（explorer 可直接解码 system contract ABI）或社区治理需要升级协议逻辑时。

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

### P3 — Future Features（远期，非阻塞）

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 15 | StorageCtx TLS → revm JournalTr | ✅ | 所有 precompile 通过 `EvmStorageProvider` 直接访问 revm `JournalTr` |
| 16 | precompile gas 自动计费 | 🔮 Future | 当前手动 `deduct_gas()` 正确工作；需 revm 架构支持才能完全自动 |
| 17 | System Contract 部署 | 🔮 Future | 长期将协议逻辑从 Rust precompile 迁移为 Solidity system contract |

---

## 六、参考资源

- **reth 架构**：`tempo/crates/execution/` — reth MDBX + revm 集成示例
- **共识解耦**：`arc-node/crates/executor/` — `BlockExecutor` trait 自定义实现
- **call-node 当前执行层**：`crates/evm/src/executor.rs` — `EvmExecutor::execute_tx_provider()`
- **call-node 状态层**：`crates/evm/src/provider.rs` — `InMemoryStateProvider`
- **call-node trie 层**：`crates/evm/src/trie.rs` — `MdbxTrieCursorFactory`
- **call-node precompile 状态访问**：`crates/precompile/src/storage.rs` — `StorageCtx` TLS
