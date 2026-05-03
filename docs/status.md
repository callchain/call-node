# EVM-Only State Migration — Protocol State Status

> 本文档记录 `RpcState` 中各 protocol state 字段的迁移状态：是否已迁移到 EVM storage、仍在被谁使用、何时可以删除。

---

## 结论

这些 protocol state 最终目标上都不需要，但目前仍然被 actively 使用，不能直接删除。

---

## 逐个状态评估

| 字段 | 是否已迁移到 EVM storage | 仍在被谁使用 | 能否删除 |
|------|------------------------|-------------|---------|
| `balance_state` | ✅ 已完全迁移到 EVM storage，`RpcState` 字段已删除 | 无（所有余额读写走 EVM） | **是**（已移除） |
| `asset_registry` | ✅ 已完全迁移到 EVM storage，`RpcState` 字段已删除 | 无（所有资产元数据读写走 EVM） | **是**（已移除） |
| `compliance_engine` | 部分迁移（Compliance precompile 存在） | `state_bundle.rs` 仅获取锁，node 中无 active 使用 | **可能可以，需确认** |
| `bridge_state` | ✅ 已完全迁移到 EVM storage，`RpcState` 字段已删除 | 无（pending deposits、processed txs、daily limits 全部走 EVM） | **是**（已移除） |
| `validator_state` | ✅ 已完全迁移到 EVM storage，`RpcState` 字段已删除 | 无（所有验证人读写走 EVM） | **是**（已移除） |
| `agent_registry` | ✅ 已完全迁移到 EVM storage，`RpcState` 字段已删除 | 无（agent 快照从 EVM 读取，RPC 读写走 EVM） | **是**（已移除） |
| `shielded_state` | ✅ 已完全迁移到 EVM storage，`RpcState` 字段已删除 | 无（shielded 快照从 EVM 读取，RPC 读写走 EVM） | **是**（已移除） |
| `governance` | ✅ 已移出 `RpcState`，作为 `CallNode` sidecar；RPC 读端走 EVM precompile | `block_producer.rs` / `bft_loop.rs` 提案推进仍用内存 `GovernanceManager`（事件广播） | **部分**（sidecar 化完成） |
| `oracle` | ✅ 已完全迁移到 EVM storage，`RpcState` 字段已删除 | 无（价格/TWAP 全部走 EVM，OracleManager 为独立 transient） | **是**（已移除） |
| `fee_currency_registry` | ✅ 已完全移除，`RpcState` 字段已删除 | 无 | **是**（已移除） |
| `compliance_engine` | ✅ 已完全移除，`RpcState` 字段已删除 | 无 | **是**（已移除） |

---

## 关键阻塞点

1. ✅ ~~`block_producer.rs:106-125` — bridge deposit 仍通过 `balance_state.mint()` 结算，未走 EVM storage~~（已解决：balance 和 bridge 均走 EVM）
2. **`block_producer.rs:279-309`** — governance 提案推进仍走内存 `GovernanceManager`
3. ✅ ~~`block_producer.rs:414-454` — 状态快照仍从各内存结构读 root~~（已解决：validator / agent / shielded root 均从 EVM 读取）

---

## 修正计划

按以下顺序逐步将 node/rpc 层的读写路径切到 EVM storage，每完成一个字段就删除对应的 `RpcState` 字段和 persistence 代码。

### Step 1 — `balance_state` + `asset_registry`（Asset precompile 打通）✅ 已完成

- **目标**：所有余额和资产元数据只从 EVM storage 读写，`AccountState` 和 `AssetRegistry` 从 `RpcState` 移除
- **状态**：已完成（2026-05-02）
- **完成内容**：
  1. ✅ `crates/consensus/src/exec/evm_instructions.rs` — 新增 `add_balance_evm`、`deduct_balance_evm`、`seed_allowance`、`read_allowance`、`add_asset_supply_evm`、`seed_asset_compliance`、`read_asset_contract_address`、`seed_asset_contract_address`
  2. ✅ `crates/node/src/block_producer.rs` — bridge deposit 结算改为调用 `evm_instructions::add_balance_evm()` 写 EVM storage，不再写 `balance_state`
  3. ✅ `crates/node/src/state_persist.rs` — 删除 `balance_state` 和 `asset_registry` 的 persistence 代码
  4. ✅ `crates/rpc/src/state_bundle.rs` — 从 `StateWriteBundle` / `StateReadBundle` 移除 `balance_state` 和 `asset_registry`
  5. ✅ `crates/rpc/src/handlers/state.rs` / `callchain.rs` — 从 `RpcState` 删除 `balance_state` 和 `asset_registry` 字段；`grant_agent_balance` 改为从 EVM 读余额
  6. ✅ `crates/node/src/lib.rs` — 删除 `AccountState` 和 `AssetRegistry` 初始化
  7. ✅ `crates/node/src/boot.rs` — genesis 注入改为直接写 EVM storage
  8. ✅ `crates/chainspec/src/genesis.rs` — 从 `GenesisState` 删除 `balances` 和 `registry`；资产注册直接写 EVM
  9. ✅ `crates/bridge/src/external/deposit.rs` — 删除 `process_external_deposit` / `process_light_client_deposit` 的 `AccountState` 参数
  10. ✅ 修复全部 lib tests 和 integration tests（12 个测试文件）

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

### Step 3 — `bridge_state`（Bridge precompile 打通）✅ 已完成

- **目标**：外部 deposit/withdrawal 状态只存 EVM storage
- **状态**：已完成（2026-05-02）
- **完成内容**：
  1. ✅ `crates/consensus/src/exec/evm_instructions.rs` — 新增 `read_bridge_pending_*`、`seed_bridge_pending`、`set_bridge_pending_status`、`read_bridge_processed`、`seed_bridge_processed`、`finalize_pending_external_deposits_evm`
  2. ✅ `crates/node/src/block_producer.rs` — bridge deposit finalization 改为调用 `finalize_pending_external_deposits_evm()`，直接读写 EVM storage，不再依赖 `BridgeStateManager`
  3. ✅ `crates/bridge/src/external/deposit.rs` — 新增 `process_external_deposit_evm`、`process_light_client_deposit_evm`，inline EVM slot helpers；保留原有函数供 bridge crate 内部测试
  4. ✅ `crates/rpc/src/handlers/callchain.rs` — `call_bridgeGetDepositStatus` 改为从 EVM storage 读 pending/processed 状态；`call_lightClientBridgeDeposit` 改为调用 `process_light_client_deposit_evm`
  5. ✅ `crates/rpc/src/handlers/state.rs` — 从 `RpcState` 删除 `bridge_state` 字段和 `new()` 参数
  6. ✅ `crates/rpc/src/state_bundle.rs` — 从 `StateWriteBundle` / `StateReadBundle` 移除 `bridge`
  7. ✅ `crates/node/src/state_persist.rs` — 删除 `bridge_state` persistence（`load_bridge_state_inner` / `save_bridge_state_inner` / `CallBridgeOps`）
  8. ✅ `crates/node/src/lib.rs` — 删除 `BridgeStateManager` 初始化；`LoadedState` 移除 `bridge_state`
  9. ✅ `crates/payload-builder/src/builder.rs` — 移除 `BridgeStateManager` 参数
  10. ✅ 修复全部 lib tests 和 integration tests

### Step 4 — `governance`（Governance sidecar 化 + RPC 读端走 EVM）✅ 已完成

- **目标**：`GovernanceManager` 从 `RpcState` 移除，RPC 读端走 EVM precompile
- **状态**：已完成（2026-05-03）
- **完成内容**：
  1. ✅ `crates/rpc/src/handlers/state.rs` — 从 `RpcState` 删除 `governance` 字段
  2. ✅ `crates/rpc/src/state_bundle.rs` — 从 `StateWriteBundle` / `StateReadBundle` 移除 `governance`
  3. ✅ `crates/rpc/src/handlers/executor.rs` — 更新 `wire_governance_executor` 签名
  4. ✅ `crates/rpc/src/handlers/callchain.rs` — `call_governanceGetProposal` 等读端改为从 EVM storage 读取（`read_gov_proposal_count`、`read_gov_proposal_status`、`read_gov_proposal_proposer`、`read_gov_proposal_title`、`read_gov_proposal_description`、`read_gov_proposal_data_hash`、`read_gov_proposal_votes`、`read_gov_proposal_deposit`、`read_gov_proposal_queued_at`、`read_gov_voter_vote`）
  5. ✅ `crates/consensus/src/exec/evm_instructions.rs` — 新增 governance EVM read helpers
  6. ✅ `crates/node/src/lib.rs` — `CallNode` 新增 `governance: Arc<RwLock<GovernanceManager>>`；BFT / block production / shutdown 传递独立 `governance` 参数
  7. ✅ `crates/node/src/block_producer.rs` / `bft_loop.rs` — 接收独立 `governance` 参数；所有 `state.governance` 改为 `governance`
  8. ✅ `crates/node/src/state_persist.rs` — `persist_state_to_db` / `persist_state_incremental` 接收独立 `governance` 参数
  9. ✅ `crates/node/src/boot.rs` — genesis validator 注册改为 `node.governance`
  10. ✅ `crates/node/tests/e2e/harness.rs` — `TestNode` 新增 `governance` 字段；`produce_block` 改为独立锁
  11. ✅ 修复全部 lib tests 和 integration tests

> **注意**：`GovernanceManager` 本身（提案列表、投票记录、状态机、事件队列）仍保留为内存 sidecar，未迁移到 EVM precompile storage。这是因为 governance 状态机包含大量结构化数据（Proposal 结构体、事件队列、投票映射），全部序列化到 EVM storage 需要大量 precompile 扩展工作。当前架构已满足目标：所有**读端**（RPC 查询）走 EVM，所有**写端**（提案推进、事件广播）走 sidecar。

### Step 5 — `oracle`（Oracle precompile 打通）✅ 已完成

- **目标**：价格数据、TWAP、更新时间只存 EVM storage
- **状态**：已完成（2026-05-02）
- **完成内容**：
  1. ✅ `crates/consensus/src/exec/evm_instructions.rs` — 新增 `read_oracle_price`、`read_oracle_twap`、`read_oracle_timestamp`、`read_oracle_block`、`read_oracle_count`、`seed_oracle_price`
  2. ✅ `crates/oracle/src/manager.rs` — 删除 `aggregated` 和 `history` 字段；`submit_price` 返回 `Result<Option<AggregatedPrice>, OracleError>`；调用者在 quorum 达成时写 EVM
  3. ✅ `crates/oracle/src/tests.rs` — 更新测试以匹配新 API
  4. ✅ `crates/rpc/src/handlers/state.rs` — 从 `RpcState` 删除 `oracle` 字段和 `new()` 参数
  5. ✅ `crates/rpc/src/state_bundle.rs` — 从 `StateWriteBundle` / `StateReadBundle` 移除 `oracle`
  6. ✅ `crates/rpc/src/handlers/callchain.rs` — `call_oracleGetPrice` / `call_oracleGetTwap` 改为从 EVM storage 读取
  7. ✅ `crates/rpc/src/handlers/executor.rs` — 删除 `oracle.` 参数变更分支
  8. ✅ `crates/node/src/lib.rs` — `CallNode` 新增 `oracle: Arc<RwLock<OracleManager>>`；加载/保存独立进行
  9. ✅ `crates/node/src/block_producer.rs` / `bft_loop.rs` — 接收独立 `oracle` 参数；所有 `state.oracle` 改为 `oracle`
  10. ✅ `crates/node/src/network_handler.rs` — 接收独立 `oracle` 参数；价格请求改为读 EVM；P2P 提交在 quorum 时写 EVM
  11. ✅ `crates/node/src/state_persist.rs` — `persist_state_to_db` / `persist_state_incremental` 接收独立 `oracle` 参数
  12. ✅ `crates/chainspec/src/genesis.rs` — 从 `GenesisState` / `LoadedState` 删除 `oracle`
  13. ✅ `crates/protocol/src/fee_currency.rs` — 移除 `OracleManager` 依赖（fallback 值）
  14. ✅ 修复全部 lib tests、integration tests 和 e2e harness

### Step 6 — `agent_registry` + `shielded_state`（Agent / Shielded precompile 打通）✅ 已完成

- **目标**：Agent 注册信息和 Shielded 的 nullifier/commitment 树只存 EVM storage
- **状态**：已完成（2026-05-02）
- **完成内容**：
  1. ✅ `crates/consensus/src/exec/evm_instructions.rs` — 新增 `read_agent_count`、`agent_set_pubkey`
  2. ✅ `crates/node/src/block_producer.rs` / `bft_loop.rs` — `shielded_root` / `agent_root` 快照改为从 EVM storage 读取（`read_shielded_merkle_root`、`read_agent_count` + `agent_get_owner`/`name`/`registered_at`）
  3. ✅ `crates/rpc/src/handlers/state.rs` — 从 `RpcState` 删除 `agent_registry`、`agent_balances`、`shielded_state`；`register_agent` / `grant_agent_balance` / `revoke_agent_balance` / `get_shielded_tree_state` 全部改为读写 EVM storage
  4. ✅ `crates/rpc/src/state_bundle.rs` — 从 `StateWriteBundle` / `StateReadBundle` 移除 `agent_registry`、`agent_balances`、`shielded`
  5. ✅ `crates/rpc/src/handlers/callchain.rs` — `call_shieldedBalance` / `call_lightVerifyShieldedTx` / `call_lightGetShieldedBalance` / `call_lightGetBalanceProof` 改为读 EVM storage（Merkle tree 详细证明暂返回空，需完整 sidecar 节点）
  6. ✅ `crates/node/src/state_persist.rs` — 删除 `shielded_state`、`agent_registry`、`agent_balances` persistence（`load_shielded_state_inner` / `save_shielded_state_inner` / `load_agent_state_inner` / `save_agent_state_inner` / `CallShieldedNullifiers` / `CallShieldedCommitments` / `CallAgents` / `CallAgentBalances`）
  7. ✅ `crates/node/src/lib.rs` — 删除 `ShieldedState`、`AgentRegistry`、`AgentBalances` 初始化；`LoadedState` 移除对应字段
  8. ✅ `crates/node/tests/e2e/harness.rs` / `test_shielded_e2e.rs` — 更新 `RpcState::new()` 调用和 shielded 断言改为读 EVM
  9. ✅ 修复全部 lib tests 和 integration tests

### Step 7 — `fee_currency_registry` + `compliance_engine` ✅ 已完成

- **目标**：费用币种和合规策略只存 EVM storage
- **状态**：已完成（2026-05-02）
- **完成内容**：
  1. ✅ 从 `RpcState` 删除 `fee_currency_registry` 和 `compliance_engine` 字段
  2. ✅ 从 `state_bundle.rs` 移除对应锁
  3. ✅ 从 `state_persist.rs` 移除 persistence 代码
  4. ✅ 修复全部 tests

### Step 8 — persistence 清理 ✅ 已完成

- **目标**：删除 persistence 层死代码，简化 `state_persist.rs`
- **状态**：已完成（2026-05-03）
- **完成内容**：
  1. ✅ `crates/node/src/state_persist.rs` — 删除死代码 `load_receipts_by_block`、`delete_receipts_by_block`
  2. ✅ 验证 `LoadedState` 已精简（只含 `evm_state`、`agent_nonces`、`governance`、`fee_params`）
  3. ✅ 验证 `state_bundle.rs` 的 `StateWriteBundle` / `StateReadBundle` 只含 `evm`、`fee_params`、`fork_manager`
  4. ✅ 验证 `RpcState` 中所有已移除字段（balance_state、asset_registry、validator_state、bridge_state、agent_registry、agent_balances、shielded_state、oracle、compliance_engine、fee_currency_registry、governance）均已删除

> **注意**：由于 oracle 和 governance 采用 sidecar 模式（未完全迁移到 EVM），它们的 persistence 函数（`save_oracle_state` / `load_oracle_state`、`save_governance_state` / `load_governance_state`）仍需保留。consensus、fee params、agent nonces、receipts、fork state 同样仍需 persistence。

---

## 优先级建议

按**依赖链**和**改动量**排序：

1. ✅ **Step 2**（validator_state 收尾）— 已完成
2. ✅ **Step 1**（balance_state + asset_registry）— 已完成
3. ✅ **Step 3**（bridge_state）— 已完成
4. ✅ **Step 5**（oracle）— 已完成
5. ✅ **Step 6**（agent + shielded）— 已完成
6. ✅ **Step 7**（fee_currency + compliance）— 已完成
7. ✅ **Step 4**（governance sidecar 化）— 已完成
8. **Step 8**（persistence 简化）— 最终清理
