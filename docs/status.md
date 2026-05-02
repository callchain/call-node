# EVM-Only State Migration — Protocol State Status

> 本文档记录 `RpcState` 中各 protocol state 字段的迁移状态：是否已迁移到 EVM storage、仍在被谁使用、何时可以删除。

---

## 结论

这些 protocol state 最终目标上都不需要，但目前仍然被 actively 使用，不能直接删除。

---

## 逐个状态评估

| 字段 | 是否已迁移到 EVM storage | 仍在被谁使用 | 能否删除 |
|------|------------------------|-------------|---------|
| `balance_state` | 部分迁移（Asset precompile 已读写 EVM 余额），但 `balance_state` 仍作为内存缓存 | `block_producer.rs` bridge deposit 结算、`state_persist.rs` 快照 | **否** |
| `asset_registry` | 部分迁移（Asset 元数据已存 EVM） | `state_persist.rs` 加载/保存 | **否** |
| `compliance_engine` | 部分迁移（Compliance precompile 存在） | `state_bundle.rs` 仅获取锁，node 中无 active 使用 | **可能可以，需确认** |
| `bridge_state` | 未完全迁移（Bridge precompile 存在，但 deposit 结算仍走内存） | `block_producer.rs` `finalize_pending_external_deposits` | **否** |
| `validator_state` | ✅ 已完全迁移到 EVM storage，`RpcState` 字段已删除 | 无（所有验证人读写走 EVM） | **是**（已移除） |
| `agent_registry` | 未完全迁移 | `block_producer.rs` 快照 | **否** |
| `shielded_state` | 未完全迁移 | `block_producer.rs` 快照 | **否** |
| `governance` | 未完全迁移 | `block_producer.rs` 提案推进、事件广播 | **否** |
| `oracle` | 未完全迁移 | `block_producer.rs`, `bft_loop.rs` 价格跟踪 | **否** |
| `fee_currency_registry` | 未迁移 | `lib.rs`, `state_persist.rs` | **否** |

---

## 关键阻塞点

1. **`block_producer.rs:106-125`** — bridge deposit 仍通过 `balance_state.mint()` 结算，未走 EVM storage
2. **`block_producer.rs:279-309`** — governance 提案推进仍走内存 `GovernanceManager`
3. **`block_producer.rs:414-454`** — 状态快照仍从各内存结构读 root

---

## 修正计划

按以下顺序逐步将 node/rpc 层的读写路径切到 EVM storage，每完成一个字段就删除对应的 `RpcState` 字段和 persistence 代码。

### Step 1 — `balance_state` + `asset_registry`（Asset precompile 打通）

- **目标**：所有余额和资产元数据只从 EVM storage 读写，`AccountState` 和 `AssetRegistry` 从 `RpcState` 移除
- **工作量**：中等
- **步骤**：
  1. 修改 `block_producer.rs:106-125`：bridge deposit 结算改为调用 `evm_instructions::seed_balance()` 写 EVM storage，不再写 `balance_state`
  2. 删除快照中的 `protocol_root` 计算（改用 `evm_root` 作为唯一 root）
  3. 删除 `state_persist.rs` 中的 `save_balances` / `load_balances`（数据已在 EVM accounts 中）
  4. 删除 `RpcState.balance_state` 和 `RpcState.asset_registry` 字段
  5. 清理 `state_bundle.rs` 中对应的锁获取

### Step 2 — `validator_state`（Phase 5 收尾）✅ 已完成

- **目标**：`ValidatorStateManager` 完全从 `RpcState` 移除，所有验证人读写走 EVM
- **状态**：已完成（2026-05-02）
- **完成内容**：
  1. ✅ `crates/rpc/src/handlers/state.rs` — 从 `RpcState` 删除 `validator_state` 字段，RPC handler 不再依赖内存验证人状态
  2. ✅ `crates/rpc/src/state_bundle.rs` — 从 `StateWriteBundle` / `StateReadBundle` 移除 `validator_state`
  3. ✅ `crates/node/src/block_producer.rs` — governance 同步改为从 EVM storage 读验证人列表（`read_validators`/`read_validator_addresses`）
  4. ✅ `crates/node/src/bft_loop.rs` — 删除 rollback 时的 validator reset；pubkey_to_id 改为从 EVM 读取
  5. ✅ `crates/node/src/lib.rs` — BFT subset 从 EVM 读取；light client trusted validators 从 EVM 读取
  6. ✅ `crates/node/src/boot.rs` — BLS pubkey 写入 EVM；删除 genesis `validator_state` 注入
  7. ✅ `crates/node/src/network_handler.rs` — `is_validator` 改为从 EVM count 判断
  8. ✅ `crates/rpc/src/handlers/executor.rs` — `ValidatorSlash` 使用 `remove_validator_evm()`；rotation 使用 `rotate_validator_key_evm()`
  9. ✅ `crates/node/src/state_persist.rs` — 删除 validator state persistence 代码
  10. ✅ `crates/chainspec/src/genesis.rs` — 从 `GenesisState` / `LoadedState` 删除 `validators` 字段
  11. ✅ `crates/consensus/src/exec/evm_instructions.rs` — 新增 BLS pubkey storage、validator 删除/轮换 helper
  12. ✅ 修复全部集成测试中的预存在 bug（`evm_state` 重复写锁死锁、gas limit 不匹配、quorum 计算错误）

### Step 3 — `bridge_state`（Bridge precompile 打通）

- **目标**：外部 deposit/withdrawal 状态只存 EVM storage
- **工作量**：中等
- **步骤**：
  1. 将 `BridgeStateManager` 的 pending deposits、withdrawal tracking 迁移到 Bridge precompile storage slots
  2. 修改 `block_producer.rs` bridge 结算逻辑为调用 Bridge precompile
  3. 删除 `RpcState.bridge_state` 和 persistence

### Step 4 — `governance`（Governance precompile 打通）

- **目标**：提案、投票、执行状态只存 EVM storage
- **工作量**：中等偏大
- **步骤**：
  1. 将 `GovernanceManager` 的提案列表、投票记录、状态机迁移到 Governance precompile storage
  2. 修改 `block_producer.rs:277-299` 提案推进逻辑为调用 Governance precompile 或读取 EVM storage
  3. 删除 `RpcState.governance` 和 persistence

### Step 5 — `oracle`（Oracle precompile 打通）

- **目标**：价格数据、TWAP、更新时间只存 EVM storage
- **工作量**：中等
- **步骤**：
  1. 将 `OracleManager` 的 tracked prices、period tracking、outlier detection 数据迁移到 Oracle precompile storage
  2. 修改 `bft_loop.rs` 和 `block_producer.rs` 的 oracle 边界逻辑为读写 EVM
  3. 删除 `RpcState.oracle` 和 persistence

### Step 6 — `agent_registry` + `shielded_state`（Agent / Shielded precompile 打通）

- **目标**：Agent 注册信息和 Shielded 的 nullifier/commitment 树只存 EVM storage
- **工作量**：中等
- **步骤**：
  1. Agent：将 `AgentRegistry` 迁移到 Agent precompile storage；修改相关 RPC
  2. Shielded：将 `ShieldedState` 的 Merkle tree root + nullifier set 迁移到 Shielded precompile storage（完整树可做 sidecar）
  3. 删除 `RpcState.agent_registry`、`agent_balances`、`shielded_state` 和 persistence

### Step 7 — `fee_currency_registry` + `compliance_engine`

- **目标**：费用币种和合规策略只存 EVM storage
- **工作量**：小
- **步骤**：
  1. `fee_currency_registry`：费用币种列表写入 EVM storage（或用 governance 提案管理）
  2. `compliance_engine`：合规策略写入 Compliance precompile storage
  3. 删除对应 `RpcState` 字段和 persistence

### Step 8 — 状态快照和 persistence 简化

- **目标**：persistence 只保存 `EvmState`（+ receipts + fork state）
- **步骤**：
  1. 删除 `state_persist.rs` 中所有 protocol-state 表的 save/load（只保留 `CallEvmAccounts`、`CallReceipts`、`CallForkState`）
  2. 更新 `LoadedState` 为只含 `evm_state`
  3. 删除 `RpcState` 中所有已移除的字段
  4. 更新 `state_bundle.rs` 的 `StateWriteBundle` / `StateReadBundle` 只含 `evm`、`fee_params`、`fork_manager`

---

## 优先级建议

按**依赖链**和**改动量**排序：

1. **Step 2**（validator_state 收尾）— Phase 5 刚完成，残留字段清理工作量最小
2. **Step 1**（balance_state + asset_registry）— Asset precompile 已经可用，只需改 bridge deposit 结算和 snapshot
3. **Step 3**（bridge_state）— 依赖 Step 1 的 balance 迁移完成后更安全
4. **Step 5**（oracle）— 数据相对独立
5. **Step 4**（governance）— 改动量最大，放后面
6. **Step 6**（agent + shielded）— 影响面较小
7. **Step 7**（fee_currency + compliance）— 最后收尾
8. **Step 8**（persistence 简化）— 每步完成后逐步清理
