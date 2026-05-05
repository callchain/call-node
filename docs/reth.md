# call-node 深度复用 reth 设计与实现路径

**Scope**: 从自研 EVM 堆栈一次性迁移到深度复用 reth 的技术方案
**Last Updated**: 2026-05-04

---

## 一、现状对比

| 维度 | tempo | arc-node | call-node 当前 | call-node 目标 |
|---|---|---|---|---|
| **状态存储** | reth MDBX + reth-provider | reth MDBX + reth-provider | 内存 HashMap (`EvmState`) | reth MDBX + reth-provider |
| **执行层** | revm + StateProviderDatabase | revm + StateProviderDatabase | 自研 `EvmExecutor` (sync_to_revm_db/apply_from_revm_state) | revm + StateProviderDatabase |
| **Trie 实现** | reth-trie (sparse trie 优化) | reth-trie | 无 trie，`HashBuilder` 全量聚合 O(n) | reth-trie (incremental) |
| **State Root** | reth-trie 增量更新 | reth-trie 增量更新 | `compute_state_root()` 全量 | reth-trie 增量更新 |
| **历史状态** | 归档节点（全量 MDBX） | 可配置 pruning/archive | 仅当前状态（`EvmState` clone） | 128-block 剪枝归档或全归档 |
| **Proof 支持** | `eth_getProof` 真 Merkle Proof | `eth_getProof` 真 Merkle Proof | stub (`[state_root]`) | `eth_getProof` 真 Merkle Proof |
| **共识耦合** | Commonware (外部) | Malachite (外部) | 自研共识直接操作 `EvmState` | 自研共识通过 reth BlockExecutor |
| **Precompile 状态访问** | revm JournalTr | revm JournalTr | `StorageCtx` TLS (`scoped_thread_local!`) | revm JournalTr |
| **Gas 管理** | revm 自动 | revm 自动 | 手动 `add_gas()` | revm 自动 |

**关键结论**：tempo 和 arc-node 都选择了"薄封装 reth + 外部共识"的架构。call-node 当前是自研执行层 + 直接状态操作，这是未深度复用 reth 的根本原因。

---

## 二、未深度复用 reth 的 5 个根本障碍

### 障碍 1：StorageCtx TLS vs revm JournalTr

**现状**：call-node precompile 通过 `scoped_thread_local!` 的 `StorageCtx` 全局访问状态。

```rust
// crates/precompile/src/storage.rs
scoped_thread_local!(static STORAGE_CTX: RefCell<StorageCtx>);

pub fn with_storage<T>(f: impl FnOnce(&mut dyn StorageProvider) -> T) -> T {
    STORAGE_CTX.with(|ctx| {
        let mut ctx = ctx.borrow_mut();
        f(ctx.provider.as_mut())
    })
}
```

**问题**：revm 的 `JournalTr` 是执行期状态变更的权威来源。`StorageCtx` TLS 绕过了 revm 的 journal，导致：
- revm 无法追踪 precompile 的状态访问用于 gas 计费
- revm 的 revert 机制无法回滚 precompile 的存储变更
- `StateProviderDatabase` 无法为 precompile 提供历史状态视图

**迁移策略**：
1. 删除 TLS 模式，precompile 直接操作 revm 的 `JournalTr`
2. 协议状态访问变成普通的 EVM `SLOAD`/`SSTORE`，由 revm 自动处理 gas 和 revert
3. 执行期通过 `CacheDB` 的 `storage` 方法代理，不再使用全局变量

---

### 障碍 2：手动 Gas 追踪 vs revm 自动 Gas

**现状**：precompile 手动调用 `add_gas()` 向 EVM 报告存储访问开销。

```rust
// crates/precompile/src/storage.rs (简化)
fn read_storage(&mut self, key: &StorageKey) -> Option<Vec<u8>> {
    self.add_gas(COLD_STORAGE_READ_COST); // 手动
    // ... read ...
}
```

**问题**：revm 在 `SLOAD`/`SSTORE` 时自动计算 warm/cold gas。call-node 的手动追踪与 revm 的 `JournalTr::warm_preloaded_addresses` 和 `JournalTr::sload` gas 计算是两套独立系统，容易不一致。

**迁移策略**：
1. 协议 precompile 的存储访问映射为 EVM 存储槽读写（address=precompile_addr, slot=hash(key)）
2. 由 revm 自动计费，删除所有 `add_gas()` 调用
3. 如果协议存储需要自定义 gas 模型，实现自定义 `JournalTr` 覆盖 `sload_gas`/`sstore_gas`

---

### 障碍 3：协议直接操作 EVM State vs revm Transaction-Only 执行

**现状**：共识层直接读写 `EvmState`。

```rust
// crates/consensus/src/block.rs
let evm_snapshot = state.evm_state.clone();
// ... 直接修改 evm_state.accounts ...
// 出错时: *state.evm_state = evm_snapshot;
```

**问题**：reth 的 `BlockExecutor` 假设所有状态变更都通过交易执行产生。call-node 的共识逻辑（validator staking、proposer 轮换、protocol balance 调整）直接修改 `EvmState`，这在 reth 架构中没有对应位置。

**迁移策略**：
1. **所有协议逻辑转为 EVM 交易**：validator staking、asset registration、bridge deposit/withdraw 都变成调用预部署的 system contract（或 precompile 的 `CALL` 入口）
2. **System contracts**：部署不可变更的合约地址（如 `0x0000...0001` 为 StakingContract），共识区块中自动插入 system transactions
3. **BlockExecutor trait 实现**：`CallchainBlockExecutor` 在 `execute_transactions()` 前/后插入 system tx

---

### 障碍 4：EvmState Cloneable Snapshots vs StateProvider Trait Object

**现状**：`EvmState` 实现 `Clone`，RPC `eth_call` 通过 `clone()` 实现只读模拟。

```rust
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EvmState {
    pub accounts: HashMap<Address, EvmAccount>,
}
```

**问题**：reth 的 `StateProvider` 是 trait object，不提供 `Clone`。`eth_call` 在 reth 中通过 `StateProviderBox::new(state)` 创建只读视图，不需要 clone 整个 state。

**迁移策略**：
1. `eth_call` / `eth_estimateGas` 使用 reth 的 `StateProviderFactory::history_by_block_number()` 获取历史状态的 `StateProvider`
2. 模拟执行时，revm 的 `InMemoryDB` 缓存变更，不修改底层 MDBX，无需 clone
3. 删除 `EvmState::clone()` 依赖，完全改用 `StateProvider` trait

---

### 障碍 5：解耦共识/EVM vs reth BlockExecutor Trait

**现状**：call-node 共识和 EVM 是两层独立系统，通过 `ExecutionState { evm_state: &mut EvmState }` 连接。

**问题**：reth 的 `BlockExecutor` trait 要求执行器管理整个区块的状态转换。call-node 的共识逻辑（BFT 投票、validator set 变更）不在交易执行路径中。

**迁移策略**：
1. 实现 `CallchainBlockExecutor: BlockExecutor`，其中：
   - `execute_transactions()`：执行普通 EVM tx + system tx
   - `apply_post_execution_changes()`：应用 validator reward、proposer 分配
2. 共识引擎只负责：收集 tx → 提议区块 → BFT 投票 → 提交后调用 `BlockExecutor::execute_and_verify_block()`
3. 参考 arc-node 的 `CustomExecutor` 实现（在 arc-node/crates/executor/ 中）

---

## 三、一次性切换实现路径

### Phase 1：执行层替换（引入 revm StateProviderDatabase）

**目标**：用 revm 的标准执行路径替换自研 `EvmExecutor`，同时解决 StorageCtx TLS 和手动 Gas。

1. **删除 `sync_to_revm_db` / `apply_from_revm_state`**：
   - 改为 revm `CacheDB<StateProviderDatabase>` 直接操作底层状态
2. **StorageCtx TLS → JournalTr**：
   - precompile 直接通过 `JournalTr::sload`/`JournalTr::sstore` 访问状态
   - 删除 `scoped_thread_local!` 和 `with_storage()` 全局模式
3. **Gas 自动追踪**：
   - 删除所有手动 `add_gas()` 调用
   - 协议存储映射为 EVM storage slot，由 revm 自动计费

**验证标准**：
- 所有现有 EVM tx 测试通过
- precompile 测试通过（balance/staking/asset/bridge）
- Gas 使用量与迁移前一致（对比关键 tx 的 gas_used）

---

### Phase 2：状态存储替换（引入 reth MDBX + reth-trie）

**目标**：`EvmState` 从内存 HashMap 一次性替换为 MDBX，同时获得增量 state root 和 Merkle proof。

1. **Schema 设计**（参考 reth `tables.rs`）：
   ```
   CanonicalHeaders: (BlockNumber) -> Header
   HeaderTD: (BlockNumber) -> TotalDifficulty
   HeaderNumbers: (BlockHash) -> BlockNumber
   BlockBodyIndices: (BlockNumber) -> StoredBlockBodyIndices
   Transactions: (TxNumber) -> TransactionSignedNoHash
   Receipts: (BlockNumber, TxIndex) -> Receipt
   PlainAccountState: (Address) -> Account
   PlainStorageState: (Address, Slot) -> Value
   AccountHistory: (Address, BlockNumber) -> AccountBeforeTx
   StorageHistory: (Address, Slot, BlockNumber) -> ValueBeforeTx
   AccountTrie: (Nibbles) -> BranchNode/LeafNode
   StorageTrie: (Address, Nibbles) -> BranchNode/LeafNode
   ```
2. **状态访问层替换**：
   - 所有 `EvmState` 读写改为 `StateProvider` / `StateProviderFactory` trait 调用
   - 共识层不再持有 `EvmState`，改为持有 `Arc<Database>`
3. **State Root 替换**：
   - 删除 `compute_state_root()` 的 `HashBuilder` 全量聚合
   - 使用 `reth-trie` 增量更新，区块提交时更新 `AccountTrie`/`StorageTrie`
4. **删除 `EvmState` 结构**：
   - 不再保留内存中的 `HashMap<Address, EvmAccount>`
   - 所有状态从 MDBX 读取，`CacheDB` 缓存热点

**验证标准**：
- `eth_getProof` 返回真 Merkle proof（非 stub）
- `eth_getBalance(blockTag)` 支持 `"0xN"` 历史查询
- `state_root` 与迁移前一致（创世区块至切换区块）

---

### Phase 3：历史状态与 Proof（reth-provider + reth-trie）

**目标**：完整的 blockTag 历史查询 + `eth_getProof`。

1. **StateProviderFactory**：
   - `latest()` → 当前 MDBX 状态
   - `history_by_block_number(n)` → 从 `AccountHistory`/`StorageHistory` 重建
2. **Archive vs Pruned**：
   - 默认 128-block 剪枝（参考 reth `PruneModes`）
   - 全归档节点可选（配置 `--archive`）
3. **eth_getProof**：
   - 通过 `AccountTrie`/`StorageTrie` 读取节点，生成 Merkle proof

---

### Phase 4：BlockExecutor 与共识解耦

**目标**：共识只负责 BFT，状态转换完全委托给 reth `BlockExecutor`。

1. **System Contracts**：
   - 将 validator staking、asset registry、bridge logic 部署为 system contracts
   - 或保留 precompile 但统一通过 `CALL` 入口执行
2. **CallchainBlockExecutor**：
   - 实现 `BlockExecutor` trait
   - 区块执行前插入 system tx（如 proposer reward、validator set 更新）
3. **共识引擎改造**：
   - 删除直接 `EvmState` 操作
   - 提交区块后调用 `executor.execute_and_verify_block()`

---

## 四、风险评估

| 风险 | 可能性 | 影响 | 缓解措施 |
|---|---|---|---|
| Phase 1 Gas 计费不一致 | 中 | 高 | 迁移前后对比 100+ tx 的 gas_used，建立回归测试 |
| Phase 2 MDBX 性能低于内存 HashMap | 低 | 中 | 先跑 benchmark，确认 TPS 满足需求再合并 |
| Phase 3 历史状态重建失败 | 低 | 高 | 128-block 内 `state_root` 逐块校验 |
| Phase 4 System Contract 安全漏洞 | 中 | 高 | 形式化验证 system contract 逻辑，immutable 部署 |

**无回滚设计**：一次性切换，不保留 dual-write 或 feature flag。每个 Phase 独立分支开发，合并前必须通过完整测试套件 + 主网等价性验证（state_root 逐块比对）。

---

## 五、决策点

### System Contract vs Precompile 保留？

| 方案 | 兼容性 | 复杂度 |
|---|---|---|
| 全部 System Contract | 最高（纯 EVM）| 高（需合约审计）|
| 保留 Precompile + JournalTr 代理 | 中 | 中 |
| 混合：核心逻辑 System Contract，Gas/辅助 Precompile | 推荐 | 中 |

**推荐**：Bridge、Asset Registry 等状态重的走 System Contract；Gas 优化相关的保留 Precompile。

---

## 六、未完成任务清单（按优先级排序）

### P0 — 执行层与状态存储（阻塞后续所有阶段）

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 1 | `EvmExecutor` 改为 `StateProviderDatabase` 执行路径 | **未完成** | 当前 `execute_tx` 仍走 `apply_from_revm_state`，未使用 `CacheDB<StateProviderDatabase>` |
| 2 | 删除 `EvmState` 结构（`HashMap<Address, EvmAccount>`） | **未完成** | `EvmState` 仍是唯一状态源，MDBX 只是持久化备份 |
| 3 | 状态读写全部改为 `StateProvider` / `StateProviderFactory` trait | **未完成** | 共识、执行、RPC 仍直接操作 `EvmState.accounts` |
| 4 | 共识层不再持有 `EvmState`，改为 `Arc<Database>` | **未完成** | `SimplexConsensus` 方法签名仍接收 `&mut EvmState` |
| 5 | MDBX 成为主存储，`CacheDB` 仅作热点缓存 | **未完成** | 当前 MDBX 是备份，`EvmState` 是主存储 |

### P1 — 历史状态与 Proof（功能增强）

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 6 | `history_by_block_number` 改为 `AccountHistory`/`StorageHistory` diff 表重建 | **未完成** | 当前是全量快照（`serde_json`），128-block 后丢弃 |
| 7 | Archive / Pruned 模式配置（`--archive` 参数） | **未完成** | 无配置选项 |
| 8 | `eth_getProof` 从持久化 `AccountTrie`/`StorageTrie` 节点读取 | **未完成** | 当前从头构建 trie，`TrieUpdates` 未持久化到 MDBX |
| 9 | `eth_call` / `eth_estimateGas` 改用 `StateProviderBox` 替代 `clone()` | **未完成** | 仍通过 `EvmState::clone()` 实现只读模拟 |

### P2 — BlockExecutor 与共识解耦（架构顶层）

| # | 任务 | 状态 | 说明 |
|---|---|---|---|
| 10 | 实现 reth `BlockExecutor` trait | **未完成** | 当前是自定义 `CallchainBlockExecutor`，未对接 reth |
| 11 | 部署 System Contracts（StakingContract、AssetRegistry 等） | **未完成** | 共识仍直接调用 `state_accessors` 读写 |
| 12 | 协议逻辑转为 EVM system tx（validator staking、reward 等） | **未完成** | 共识直接修改 EVM 存储，未走交易执行路径 |
| 13 | 共识引擎完全删除 `EvmState` 操作，仅负责 BFT | **未完成** | `commit_block` 仍接收 `&mut EvmState` 直接修改 |
| 14 | 删除 `EvmState::clone()` 依赖 | **未完成** | 多处仍依赖 clone 进行只读模拟 |

---

## 七、参考资源

- **reth 架构**：`tempo/crates/execution/` — reth MDBX + revm 集成示例
- **共识解耦**：`arc-node/crates/executor/` — `BlockExecutor` trait 自定义实现
- **call-node 当前执行层**：`crates/evm/src/executor.rs` — `EvmExecutor::execute_tx()`
- **call-node 状态层**：`crates/evm/src/state.rs` — `EvmState`, `compute_state_root()`
- **call-node precompile 状态访问**：`crates/precompile/src/storage.rs` — `StorageCtx` TLS
