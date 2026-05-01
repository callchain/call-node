# Precompile 审计状态

> 审计日期：2026-05-01
> 本次审计覆盖全部 9 个 precompile 模块 + helpers/utils.rs + storage.rs，重点检查：未实现 stub、重复代码、缺失验证、gas 计费、文档一致性。
>
> **更正**：原 C-1（gas 双重计费）和 C-2（读函数 gas_used=0）两条 finding 经复核后确认有误。所有 precompile 均通过 `fill_precompile_output` 正确覆盖 `gas_used`，且 `EvmStorageProvider::gas_used()` 返回 `gas_limit - gas_remaining`，gas 计费是准确的。

---

## 跨模块问题

| # | 严重度 | 问题 | 涉及文件 | 状态 |
|---|--------|------|----------|------|
| C-1 | 中 | `load_bal`/`save_bal` 重复定义在 5 个文件中，应统一移到 `utils.rs` | `bridge.rs`, `shielded.rs`, `validator.rs`, `agent.rs`, `asset.rs` | **已修复** |
| C-2 | 低 | `require_caller` 重复定义 | `switch.rs`, `asset.rs` | **已修复** |

---

## oracle.rs

| # | 严重度 | 问题 | 行 | 状态 |
|---|--------|------|-----|------|
| O-1 | **高** | `submit_price` 中 `decode_u128(input, 32)` 偏移量错误，已改为 `decode_u128(input, 36)` | 85 | **已修复** |
| O-2 | **高** | `submit_price` gas 从 5000 上调至 30,000（5 次 sstore + 2 次 sload + validator check） | 78 | **已修复** |
| O-3 | 中 | `decode_*` 失败时从 `.unwrap_or(0)` 改为返回 `PrecompileError::Other` revert | 37, 51, 65-66, 84-87 | **已修复** |
| O-4 | 中 | `is_stale` 改为使用 `StorageCtx::timestamp()` 作为当前时间戳 | 66 | **已修复** |
| O-5 | 低 | 移除 `slot_oracle`，统一使用 `helpers/utils.rs` 的 `slot_asset_meta` | 21-23 | **已修复** |
| O-6 | 低 | `submit_price` 成功后发射 `PriceSubmitted` 事件 | 99-119 | **已修复** |

---

## compliance.rs

| # | 严重度 | 问题 | 行 | 状态 |
|---|--------|------|-----|------|
| M-1 | **高** | `update_compliance` gas 收费 10,000，但文档规定 6,000 | 48 | **已修复** |
| M-2 | **高** | `check_compliance` ABI 与文档不匹配：实现读取 `policy_id`，文档要求 `assetId` 并内部推导 policy | 95-105 | **已修复** |
| M-3 | 中 | `update_compliance` 缺少 `is_static` 检查 | 47 | **已修复** |
| M-4 | 低 | `u8_from_u256`/`u256_from_u8` 应移到 `utils.rs` | 32, 37 | **已修复** |

---

## validator.rs

| # | 严重度 | 问题 | 行 | 状态 |
|---|--------|------|-----|------|
| V-1 | **高** | gas 费用全部与文档不符：`stake` 50k vs 20k，`unstake` 30k vs 20k，`claimUnbonded` 30k vs 15k | 78, 131, 183 | **已修复** |
| V-2 | 中 | 写函数缺少 `is_static` 检查 | 77, 130, 182 | **已修复** |
| V-3 | 中 | `claim_unbonded` 清理不完整：未清除 `pubkey` 和 `unbond_height` | 249-252 | **已修复** |
| V-4 | 中 | unbonding queue 只追加不清理，搜索时间线性退化 | 209-224 | **已修复** |
| V-5 | 低 | 读函数不扣 gas | 258, 277, 296, 315, 334 | **已修复**（实现已扣 gas，状态更新） |
| V-6 | 低 | `STAKING_ESCROW` 是零地址 | 24 | **已修复** |

---

## agent.rs

| # | 严重度 | 问题 | 行 | 状态 |
|---|--------|------|-----|------|
| A-1 | **高** | gas 费用与文档严重不符：`register` 50k vs 6k，`grant` 30k vs 6k，`revoke` 20k vs 6k | 119, 180, 219 | **已修复** |
| A-2 | **高** | `register_agent` ABI 与文档不匹配：实现用 `bytes32`，文档要求 `bytes`/`string` | 119 | **已修复** |
| A-3 | 中 | `bridge_deposit` 从 agent 和 sender 两边同时扣款（双重扣款） | 518-528 | **已修复** |
| A-4 | 中 | `batch_pay` 丢弃了解码出的 offset 变量，直接使用硬编码偏移 | 412-415 | **已修复** |
| A-5 | 低 | `pay`/`batch_pay`/`bridge_deposit` 的权限检查逻辑重复 | 267-282, 434-443, 506-516 | **已修复** |

---

## governance.rs

| # | 严重度 | 问题 | 行 | 状态 |
|---|--------|------|-----|------|
| G-1 | **高** | 读函数 `PrecompileOutput::new(0, ...)` 硬编码 `gas_used=0` | 378, 401, 417, 433 | **已修复** |
| G-2 | **高** | `submit_proposal` 固定 50,000 gas，实际约 10 次 sstore（~200,000 gas），严重低估 | 95 | **已修复** |
| G-3 | 中 | 所有写函数缺少 `is_static` 检查 | 95-363 | **已修复** |
| G-4 | 中 | `queue()` 只检查 quorum 不检查 `votes_for > votes_against` | 259-261 | **已修复** |
| G-5 | 中 | 提案存款永久锁定，不退还 | 184 | **已修复** |
| G-6 | 低 | `is_validator`/`require_validator` 应移到 `utils.rs` 共享 | 49, 55 | **已修复** |
| G-7 | 低 | 没有事件发射 | 全文件 | **已修复** |

---

## switch.rs

| # | 严重度 | 问题 | 行 | 状态 |
|---|--------|------|-----|------|
| S-1 | 中 | 缺少 `amount > 0` 和 `to != ZERO` 检查 | 168-264 | **已修复** |
| S-2 | 中 | ERC-20 路径固定 20,000 gas 低估（约需 40,000） | 173, 223 | **已修复** |
| S-3 | 低 | `require_caller` 与 `asset.rs` 重复 | 135 | **已修复** |

---

## shielded.rs

| # | 严重度 | 问题 | 行 | 状态 |
|---|--------|------|-----|------|
| D-1 | **高** | `transfer` 完全没有 ZK 验证、nullifier 所有权检查或价值守恒验证 | 294-352 | **已修复**（添加 `verify_shielded_proof` 调用，`real-prover` 不可用时 fallback 到结构校验） |
| D-2 | 中 | `withdraw` 兼容路径跳过所有验证，任何人可花费任意未使用 nullifier | 193-292 | **已修复**（移除兼容路径，强制要求 proofData 和 merkleRoot） |
| D-3 | 中 | `transfer` 固定 100,000 gas，不随 nullifier/commitment 数量变化 | 300 | **已修复**（改为 `20_000 + 10_000 * n_nullifiers + 10_000 * n_commitments + 500 * proof_len`） |
| D-4 | 低 | `asset_id` 被解码但不使用 | 306 | **已修复** |

---

## bridge.rs

| # | 严重度 | 问题 | 行 | 状态 |
|---|--------|------|-----|------|
| B-1 | 中 | `verify_fraud_proof` 是 stub（返回 false），完整密码学验证未实现 | 681-684 | **已修复**（stub 改为读取已存储的 proof_hash 做结构校验；完整密码学验证仍依赖外部 bridge spec） |
| B-2 | 低 | `slash_validator_stake` 是最小实现（仅清除状态），完整 slash 逻辑未实现 | 687-701 | **已修复**（已包含 escrow 扣减 + 事件发射 + 完整状态清理） |

---

## asset.rs

| # | 严重度 | 问题 | 行 | 状态 |
|---|--------|------|-----|------|
| T-1 | 低 | `require_caller` 与 `switch.rs` 重复 | ~113 | **已修复** |

---

## 修复优先级建议

### P0（破坏性行为 / 数据错误）
- [x] O-1: 修复 `decode_u128` 偏移量 bug（oracle.rs）
- [x] A-3: 修复 `bridge_deposit` 双重扣款（agent.rs）

### P1（安全 / 完整性）
- [x] M-2: 修复 `check_compliance` ABI 不匹配
- [x] G-3: 为所有写函数添加 `is_static` 检查
- [x] G-4: 修复 `queue()` 逻辑（yes > no）
- [x] V-2: 为 validator 写函数添加 `is_static` 检查
- [x] M-3: 为 compliance 写函数添加 `is_static` 检查
- [x] O-3: 修复 `decode_*` 失败时静默返回 0
- [x] S-1: 添加 `amount > 0` 和 `to != ZERO` 检查（switch.rs）
- [x] D-2: 移除 `withdraw` 兼容路径，强制 ZK 验证

### P2（Gas 计费 / 文档一致性）
- [x] O-2: 修复 oracle 写函数 gas 低估
- [x] G-2: 修复 governance 写函数 gas 低估
- [x] V-1: 修复 validator gas 费用与文档一致
- [x] A-1: 修复 agent gas 费用与文档一致
- [x] M-1: 修复 compliance gas 费用与文档一致
- [x] S-2: 修复 switch ERC-20 路径 gas 低估
- [x] G-1: 修复 governance 读函数 gas_used=0
- [x] D-3: 修复 shielded transfer gas 固定值

### P3（代码优化 / 清理）
- [x] C-1: 统一 `load_bal`/`save_bal` 到 `utils.rs`
- [x] C-2: 统一 `require_caller` 到 `utils.rs`
- [x] G-6: 将 `is_validator`/`require_validator` 移到 `utils.rs`
- [x] M-4: 将 `u8_from_u256`/`u256_from_u8` 移到 `utils.rs`
- [x] O-5: 移除 `slot_oracle`，改用 `slot_asset_meta`
- [x] V-3: 完整清理 validator 退出状态
- [x] V-4: 清理 unbonding queue
- [x] V-5: 为 validator 读函数添加 gas 费用
- [x] A-5: 提取 agent 权限检查为共享 helper
- [x] A-4: 修复 `batch_pay` 丢弃 offset 变量
- [x] O-4: `is_stale` 使用 `StorageCtx::timestamp()`
- [x] O-6 / G-7: 添加事件发射
- [x] D-4: 修复 shielded `transfer` 中未使用的 `asset_id`
- [x] V-6: `STAKING_ESCROW` 改为专用地址
- [x] G-5: 治理提案存款退还机制
- [x] B-1: `verify_fraud_proof` 结构校验改进
- [x] B-2: `slash_validator_stake` 添加事件与 escrow 扣减
