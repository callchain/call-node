# Callchain Ethereum JSON-RPC 兼容性审计

**Scope**: 所有 `eth_*` 方法 — 已实现方法的功能完整性审计 + 未实现方法的实现方案
**Files**: `crates/rpc/src/standard.rs`, `crates/rpc/src/handlers/state.rs`, `crates/rpc/src/ws.rs`

---

## 零、完整接口清单（47 个）

| # | 方法 | 状态 | 说明 |
|---|---|---|---|
| 1 | `eth_protocolVersion` | ❌ 未实现 | 返回固定值即可 |
| 2 | `eth_syncing` | ⚠️ 已实现(hardcoded) | 始终返回 `false`，缺同步进度跟踪 |
| 3 | `eth_coinbase` | ❌ 未实现 | 需读取 validator address |
| 4 | `eth_chainId` | ✅ 已实现 | |
| 5 | `eth_mining` | ❌ 未实现 | PoS 链，返回 `false` |
| 6 | `eth_hashrate` | ❌ 未实现 | PoS 链，返回 `"0x0"` |
| 7 | `eth_gasPrice` | ⚠️ 已实现(语义偏差) | 返回 base_fee，非 suggested price |
| 8 | `eth_maxPriorityFeePerGas` | ❌ 未实现 | 1559 兼容所需 |
| 9 | `eth_feeHistory` | ❌ 未实现 | 1559 兼容所需 |
| 10 | `eth_accounts` | ❌ 未实现 | 无本地 keystore，返回 `[]` |
| 11 | `eth_getBalance` | ✅ 已实现 | 缺 blockTag 历史查询 |
| 12 | `eth_getStorageAt` | ✅ 已实现 | 缺 blockTag |
| 13 | `eth_getTransactionCount` | ✅ 已实现 | 缺 blockTag |
| 14 | `eth_getCode` | ✅ 已实现 | 缺 blockTag |
| 15 | `eth_sign` | ❌ 未实现 | 需本地 keystore |
| 16 | `eth_signTransaction` | ❌ 未实现 | 需本地 keystore |
| 17 | `eth_sendTransaction` | ❌ 未实现 | 需本地 keystore |
| 18 | `eth_sendRawTransaction` | ✅ 已实现 | |
| 19 | `eth_call` | ✅ 已实现 | 缺 blockTag |
| 20 | `eth_estimateGas` | ✅ 已实现 | 缺 blockTag |
| 21 | `eth_createAccessList` | ❌ 未实现 | 需 EVM access list 模拟 |
| 22 | `eth_getBlockByHash` | ❌ 未实现 | 始终 Null，缺 hash→height 索引 |
| 23 | `eth_getBlockByNumber` | ⚠️ 已实现(严重缺陷) | 缺 transactions/gasUsed/size |
| 24 | `eth_getBlockTransactionCountByHash` | ❌ 未实现 | |
| 25 | `eth_getBlockTransactionCountByNumber` | ❌ 未实现 | |
| 26 | `eth_getUncleCountByBlockHash` | ❌ 未实现 | PoS 无 uncle，返回 `"0x0"` |
| 27 | `eth_getUncleCountByBlockNumber` | ❌ 未实现 | PoS 无 uncle，返回 `"0x0"` |
| 28 | `eth_getUncleByBlockHashAndIndex` | ❌ 未实现 | PoS 无 uncle，返回 `null` |
| 29 | `eth_getUncleByBlockNumberAndIndex` | ❌ 未实现 | PoS 无 uncle，返回 `null` |
| 30 | `eth_getTransactionByHash` | ⚠️ 已实现(严重缺陷) | 只有 5 个字段，缺 to/value/gas/input/nonce 等 |
| 31 | `eth_getTransactionByBlockHashAndIndex` | ❌ 未实现 | |
| 32 | `eth_getTransactionByBlockNumberAndIndex` | ❌ 未实现 | |
| 33 | `eth_getTransactionReceipt` | ⚠️ 已实现(字段不全) | 缺 blockHash/txIndex/to/contractAddress/logsBloom |
| 34 | `eth_getBlockReceipts` | ❌ 未实现 | 后端已就绪，trivial |
| 35 | `eth_getLogs` | ⚠️ 已实现(严重缺陷) | 缺 fromBlock/toBlock/topics/blockHash/txIndex/logIndex |
| 36 | `eth_newFilter` | ❌ 未实现 | 依赖 eth_getLogs filter |
| 37 | `eth_newBlockFilter` | ❌ 未实现 | |
| 38 | `eth_newPendingTransactionFilter` | ❌ 未实现 | |
| 39 | `eth_uninstallFilter` | ❌ 未实现 | |
| 40 | `eth_getFilterChanges` | ❌ 未实现 | |
| 41 | `eth_getFilterLogs` | ❌ 未实现 | |
| 42 | `eth_getWork` | ⛔ 不需要 | PoW only，返回 unsupported |
| 43 | `eth_submitWork` | ⛔ 不需要 | PoW only，返回 unsupported |
| 44 | `eth_submitHashrate` | ⛔ 不需要 | PoW only，返回 unsupported |
| 45 | `eth_getProof` | ⚠️ 已实现(stub) | storage proof 非真正 Merkle proof |
| 46 | `eth_subscribe` | ❌ 未实现 | WebSocket，需扩展 subscription 框架 |
| 47 | `eth_unsubscribe` | ❌ 未实现 | WebSocket，需扩展 subscription 框架 |

> **图例**: ✅ 已实现 / ⚠️ 已实现但有缺陷 / ❌ 未实现 / ⛔ 不需要(PoW 专用)

**统计**: 完全实现 9 个 (19%) / 有缺陷 8 个 (17%) / 未实现 27 个 (57%) / 不需要 3 个 (6%)

---

## 一、已实现方法审计（17 个）

### 1. `eth_getBalance` — ✅ 语义正确

```
eth_getBalance(address, blockTag)
```

| 检查项 | 状态 | 说明 |
|---|---|---|
| 参数解析 | ✅ | address 用 `alloy_primitives::Address` parse |
| blockTag | ⚠️ | **忽略**。始终读当前 EVM state，不支持 `"latest"`/`"pending"`/`"0xN"` 等 |
| 返回值 | ✅ | `0x...` 格式，U256 hex |

**语义**：在大多数场景下 `"latest"` 是默认行为，不影响基础使用。

---

### 2. `eth_call` — ✅ 语义正确

```
eth_call(transactionObject, blockTag)
```

| 检查项 | 状态 | 说明 |
|---|---|---|
| 参数解析 | ✅ | from/to/value/data/gas/gasPrice 都解析 |
| blockTag | ⚠️ | **忽略**，始终用当前 state clone 执行 |
| 执行语义 | ✅ | clone EVM state → execute → discard，纯只读 |
| 返回值 | ✅ | 成功返回 `0x` + output hex；失败返回 revert error |

---

### 3. `eth_sendRawTransaction` — ✅ 语义正确

```
eth_sendRawTransaction(signedTransactionData)
```

| 检查项 | 状态 | 说明 |
|---|---|---|
| RLP 解码 | ✅ | 支持 Legacy/EIP-1559/EIP-2930/EIP-7702/EIP-4844 |
| 签名恢复 | ✅ | `recover_signer()` 验证 |
| nonce 检查 | ✅ | 与 EVM state nonce 比对 |
| balance 检查 | ✅ | gas_price * gas_limit <= balance |
| mempool 入池 | ✅ | 防御层 rate limit + 入 mempool |
| P2P gossip | ✅ | broadcast 到 peers |
| **不立即执行** | ✅ | 正确 defer 到 block production |

**注意**：返回的 tx_hash 是 raw bytes 的 keccak256，不是 envelope hash。

---

### 4. `eth_getTransactionReceipt` — ⚠️ 字段不全

```
eth_getTransactionReceipt(txHash)
```

| 标准字段 | 状态 | 说明 |
|---|---|---|
| `transactionHash` | ✅ | |
| `transactionIndex` | ❌ | **缺失** — receipt 无 index 字段 |
| `blockHash` | ❌ | **缺失** — receipt 只有 block_number |
| `blockNumber` | ✅ | |
| `from` | ✅ (`gasPayer`) | |
| `to` | ❌ | **缺失** |
| `cumulativeGasUsed` | ❌ | **缺失** |
| `gasUsed` | ✅ | |
| `effectiveGasPrice` | ❌ | **缺失** |
| `contractAddress` | ❌ | **缺失** — 没有合约创建检测 |
| `logs` | ✅ | |
| `logsBloom` | ❌ | **缺失** |
| `status` | ✅ (`0x1`/`0x0`) | |
| `revertReason` | ✅ (Callchain 扩展字段) | |

**修复路径**：
- `to` / `contractAddress` / `transactionIndex` / `blockHash` 需要 `ProtocolReceipt` 扩展字段，或从 block 数据反查
- `effectiveGasPrice` 对于 protocol tx = fee_amount / gas_used
- `logsBloom` 需要 block header 或 receipt 中预计算

---

### 5. `eth_blockNumber` — ✅ 完整

返回当前 block height 的 hex。语义正确。

---

### 6. `eth_getLogs` — ❌ 严重不完整

```
eth_getLogs(filterObject)
```

| 标准 filter 字段 | 状态 | 说明 |
|---|---|---|
| `address` (string/array) | ✅ | 有 log_index 优化 |
| `fromBlock` | ❌ | **不支持** |
| `toBlock` | ❌ | **不支持** |
| `blockHash` | ❌ | **不支持** |
| `topics` | ❌ | **不支持** |

| 标准返回字段 | 状态 | 说明 |
|---|---|---|
| `address` | ✅ | |
| `topics` | ✅ | |
| `data` | ✅ | |
| `blockNumber` | ❌ | **缺失** |
| `blockHash` | ❌ | **缺失** |
| `transactionHash` | ✅ | |
| `transactionIndex` | ❌ | **缺失** |
| `logIndex` | ❌ | **缺失** |
| `removed` | ❌ | **缺失** |

**影响**：dApp 广泛使用 `fromBlock`/`toBlock`/`topics` 做事件监听，不支持会导致大量 dApp 无法工作。

---

### 7. `eth_getProof` — ⚠️ Storage proof 是 stub

```
eth_getProof(address, storageKeys, blockTag)
```

| 字段 | 状态 | 说明 |
|---|---|---|
| `address` | ✅ | |
| `balance` | ✅ | EVM balance |
| `codeHash` | ✅ | keccak256(code) |
| `nonce` | ✅ | EVM nonce |
| `storageHash` | ✅ | EVM state root |
| `accountProof` | ⚠️ | **stub** — 只返回 `[state_root]`，不是真正的 Merkle proof |
| `storageProof` | ⚠️ | **stub** — 每个 key 只返回 `[state_root]`，不是真正的 proof |

**影响**：只有做轻客户端验证的 dApp（如 bridge）才需要。大多数 dApp 不受影响。真正的 Merkle proof 需要 EVM state trie 支持。

---

### 8. `eth_chainId` — ✅ 完整

---

### 9. `eth_gasPrice` — ⚠️ 语义不准确

返回 `fee_params.base_fee`。但 Ethereum 语义中 `eth_gasPrice` 是 "suggested gas price for a transaction"，对于 EIP-1559 链通常返回 `baseFee + suggestedPriorityFee`。

Callchain 已实现动态 priority fee 市场。`eth_gasPrice` 返回 `base_fee + MIN_PRIORITY_FEE_PER_GAS`，`eth_maxPriorityFeePerGas` 和 `eth_feeHistory` 已注册并返回真实数据。

---

### 10. `eth_syncing` — ⚠️ Hardcoded false

始终返回 `false`。正确语义：
- 未同步时返回 `false`
- 同步中返回对象：`{ startingBlock, currentBlock, highestBlock, ... }`

Callchain 没有 sync progress tracking。单节点 devnet 返回 false 没问题；多节点网络需要实现。

---

### 11. `eth_getTransactionCount` — ✅ 完整（缺 blockTag）

读取 EVM nonce。`blockTag` 被忽略，始终当前状态。

---

### 12. `eth_getCode` — ✅ 完整（缺 blockTag）

---

### 13. `eth_getStorageAt` — ✅ 完整（缺 blockTag）

---

### 14. `eth_estimateGas` — ✅ 完整（缺 blockTag）

模拟执行返回 `result.gas_used`。语义正确。

---

### 15. `eth_getBlockByNumber` — ❌ 严重不完整

```
eth_getBlockByNumber(blockTag, fullTransactions)
```

| 标准字段 | 状态 | 说明 |
|---|---|---|
| `number` | ✅ | |
| `hash` | ✅ (disk) / stub (fallback) | |
| `parentHash` | ✅ | |
| `timestamp` | ✅ | |
| `gasLimit` | ✅ (`0x1c9c380`) | |
| `gasUsed` | ❌ | **固定 0x0** — 需要从 receipts 累加 |
| `transactions` | ❌ | **始终 `[]`** — 需要从 block 的 evm_txs 填充 |
| `logsBloom` | ✅ (`0x00...`) | |
| `miner` | ✅ (`0x00...`) | |
| `difficulty` | ✅ (`0x0`) | |
| `totalDifficulty` | ✅ (`0x0`) | |
| `nonce` | ✅ (`0x0`) | |
| `sha3Uncles` | ✅ (`0x00...`) | |
| `receiptsRoot` | ✅ | |
| `transactionsRoot` | ✅ (payment_root) | |
| `stateRoot` | ✅ | |
| `baseFeePerGas` | ✅ | |
| `size` | ❌ | **固定 0x0** |

**最关键缺失**：
1. **`transactions` 数组** — `Block` 结构体有 `evm_txs: Vec<Vec<u8>>` (RLP raw)。对于 `fullTransactions=false`，应返回 tx hash 列表；对于 `fullTransactions=true`，应将 EVM txs 反序列化为 Ethereum tx 对象
2. **`gasUsed`** — 应累加 block 中所有 receipts 的 gas_used
3. **`size`** — block JSON 序列化后的字节数

---

### 16. `eth_getBlockByHash` — ❌ 未实现

始终返回 `Null`。

**实现路径**：需要 hash→block_number 的索引。Block header hash 可通过 `block.header.hash()` 计算，但目前没有建立反向索引。

- **Path A**: 新增 `CallBlockHashIndex` DB table (`hash -> height`)，在 persist_block 时写入
- **Path B**: 遍历 disk 上的 block 文件，计算 hash 匹配。简单但 O(n)

---

### 17. `eth_getTransactionByHash` — ⚠️ 严重不完整

```
eth_getTransactionByHash(txHash)
```

| 标准字段 | 状态 | 说明 |
|---|---|---|
| `hash` | ✅ | |
| `blockHash` | ❌ | **缺失** |
| `blockNumber` | ✅ | |
| `transactionIndex` | ❌ | **缺失** |
| `from` | ✅ | |
| `to` | ❌ | **缺失** |
| `gas` | ❌ | **缺失** |
| `gasPrice` | ❌ | **缺失** |
| `maxFeePerGas` | ❌ | **缺失** |
| `maxPriorityFeePerGas` | ❌ | **缺失** |
| `nonce` | ❌ | **缺失** |
| `input` | ❌ | **缺失** |
| `value` | ❌ | **缺失** |
| `v/r/s` | ❌ | **缺失** |
| `type` | ❌ | **缺失** |
| `status` | ✅ (非标准字段) | |
| `gasUsed` | ✅ (非标准字段) | |

**当前逻辑**：只从 receipt 查，找不到就 Null。没有查 mempool pending tx 的逻辑。

**实现路径**：
- EVM tx：从 receipt 找到 block_number → load_block → 在 block.evm_txs 中匹配 tx_hash → RLP decode → 填充所有字段
- Protocol tx：需要定义 ETH-compatible 序列化格式（nonce → nonce, gas_limit → gas, max_fee → gasPrice, sender → from, Transfer 的 to/amount → to/value）
- Pending tx：从 mempool 查找

---

## 二、未实现方法分析（18 个）

### 🔴 高优先级

#### 18. `eth_getBlockReceipts`

```
eth_getBlockReceipts(blockTag) → [receipt, ...]
```

**后端就绪度**：✅ `RpcState::get_receipts_by_block()` 和 `state_persist::load_receipts_by_block()` 都已实现
**实现工作量**：极小（1 行注册 + 少量包装代码）

---

#### 19. `eth_maxPriorityFeePerGas`

```
eth_maxPriorityFeePerGas() → "0x..."
```

**状态**：已实现。`eth_maxPriorityFeePerGas` 返回当前最低 priority fee（1 wei）。用户可通过 `maxPriorityFee` 字段在交易中指定更高的 priority fee 以加速确认。

---

#### 20. `eth_feeHistory`

```
eth_feeHistory(blockCount, newestBlock, rewardPercentiles) → { ... }
```

**问题**：需要历史 fee 数据。

**方案**：
- 新增 `CallFeeHistory` DB table，在 finalize 时写入 `(block_number, base_fee, gas_used_ratio)`

---

#### 21. `eth_accounts`

```
eth_accounts() → [address, ...]
```

**问题**：Callchain 没有本地 keystore。
**建议**：返回 `[]`。Ethereum 规范允许空数组。

---

#### 22. `eth_coinbase`

```
eth_coinbase() → "0x..."
```

**实现**：从 `RpcState.validator_state` 读取当前 validator 的 address。

---

#### 23. `eth_protocolVersion`

```
eth_protocolVersion() → "0x41" (65)
```

**建议**：返回固定值 `"0x41"`（Ethereum 主网协议版本），或映射到 Callchain 的 ProtocolVersion。

---

### 🟡 中优先级

#### 24. `eth_getBlockTransactionCountByHash` / `eth_getBlockTransactionCountByNumber`

```
eth_getBlockTransactionCountByHash(blockHash) → count
eth_getBlockTransactionCountByNumber(blockTag) → count
```

依赖 block 中 tx list 的构建能力。与 `eth_getBlockByNumber` 共用同一套序列化逻辑。

---

#### 25. `eth_sendTransaction`

```
eth_sendTransaction(transactionObject) → txHash
```

需要本地 keystore 签名交易。Callchain 没有 key management。
**建议**：返回 `method not supported` 错误。

---

#### 26. `eth_mining` / 27. `eth_hashrate`

```
eth_mining() → false
eth_hashrate() → "0x0"
```

PoS 链不挖矿。直接返回固定值即可。

---

#### 28-31. Filter API

```
eth_newFilter / eth_getFilterChanges / eth_getFilterLogs / eth_uninstallFilter
```

需要 filter manager（filter id → filter state 的内存映射）。与 `eth_getLogs` 共用查询逻辑。

---

#### 32-33. WebSocket 订阅 (`eth_subscribe` / `eth_unsubscribe`)

**后端就绪度**：✅ `SubscriptionManager` + `register_subscription` 框架已存在，但只注册了 Callchain-specific 订阅。

**需要新增的事件类型**：
- `newHeads` — 已有 `block_tx` 广播，需包装成 ETH block header 格式
- `newPendingTransactions` — 需 mempool 广播通道
- `logs` — 需按 filter 匹配后广播

---

### 🟢 低优先级（Callchain 无此概念或高级功能）

#### 34-37. Uncle API

```
eth_getUncleCountByBlockHash
eth_getUncleCountByBlockNumber
eth_getUncleByBlockHashAndIndex
eth_getUncleByBlockNumberAndIndex
```

Callchain 没有 uncle/ommer 概念（PoS 共识）。返回 `0x0` 或 `null`。

---

#### 38. `eth_createAccessList`

```
eth_createAccessList(transactionObject, blockTag) → { accessList, gasUsed }
```

EIP-2930 功能。需要 EVM 支持 access list 模拟。大多数 dApp 不强制使用。

---

### ⛔ 已废弃（无需实现）

#### 39-42. 编译器方法（已废弃）

```
eth_getCompilers
eth_compileLLL
eth_compileSolidity
eth_compileSerpent
```

Ethereum 官方已废弃（deprecated），所有客户端返回空数组或错误。Callchain 无需实现。

---

## 三、按数据依赖分组

| 能力 | 当前状态 | 阻塞的 RPC |
|---|---|---|
| block 中 tx list 序列化 | ❌ 未实现 | `eth_getBlockByNumber`, `eth_getTransactionByHash` |
| receipt 中 blockHash / txIndex | ❌ 需扩展 `ProtocolReceipt` | `eth_getTransactionReceipt`, `eth_getTransactionByHash` |
| tx 中 to/value/input/gasPrice 提取 | ⚠️ EVM 有，Protocol 需映射 | `eth_getTransactionByHash` |
| hash→block_number 索引 | ❌ 需新 DB table | `eth_getBlockByHash` |
| mempool pending tx 查询 | ⚠️ 有 mempool 但无 by-hash 查询 | `eth_getTransactionByHash` (pending) |
| `eth_getLogs` topics + blockRange filter | ❌ 未实现 | `eth_getLogs`, `eth_newFilter`, `eth_subscribe(logs)` |
| fee history 存储 | ❌ 未实现 | `eth_feeHistory` |

---

## 四、建议实施顺序

### Phase 1（最小可行，让大多数 dApp 工作）

1. `eth_getBlockReceipts` — 后端就绪，trivial
2. `eth_getBlockByNumber` 填充 `transactions` + `gasUsed` + `size`
3. `eth_getTransactionByHash` 完整字段（从 block 反查）
4. `eth_getBlockByHash`（加 hash→height DB 索引）
5. `eth_getTransactionReceipt` 补全字段（blockHash, to, contractAddress, txIndex）

### Phase 2（事件/日志兼容）

6. `eth_getLogs` 补全 filter（fromBlock/toBlock/topics/blockHash）和返回字段
7. `eth_newFilter` / `eth_getFilterChanges` / `eth_getFilterLogs` / `eth_uninstallFilter`
8. `eth_subscribe(newHeads)` / `eth_subscribe(logs)` / `eth_subscribe(newPendingTransactions)`
9. `eth_maxPriorityFeePerGas` — 返回固定值让 1559 dApp 工作
10. `eth_feeHistory` — 返回 mock 或实现历史存储

### Phase 3（兼容性 fluff）

11. `eth_accounts` → `[]`
12. `eth_coinbase` → validator address
13. `eth_protocolVersion` → 固定值
14. `eth_mining` → `false`
15. `eth_hashrate` → `"0x0"`
16. `eth_syncing` → 实现或保持 `false`
17. `eth_sendTransaction` → 返回 unsupported error
18. `eth_getUncle*` → 返回 `0x0` / `null`
19. `eth_createAccessList` → 返回 unsupported error
20. `eth_getBlockTransactionCountByHash` / `eth_getBlockTransactionCountByNumber` — 与 block tx list 共用逻辑

---

## 五、已发现问题清单

| # | 问题 | 严重性 | 影响 |
|---|---|---|---|
| 1 | `eth_getBlockByNumber` gasUsed 固定 0x0 | 🔴 高 | 所有读 block gas 的 dApp |
| 2 | `eth_getBlockByNumber` transactions 为空 | 🔴 高 | 所有读 block tx 的 dApp |
| 3 | `eth_getTransactionByHash` 字段只有 4 个 | 🔴 高 | 所有需要 tx detail 的 dApp |
| 4 | `eth_getLogs` 不支持 fromBlock/toBlock/topics | 🔴 高 | 所有事件监听 dApp |
| 5 | `eth_getTransactionReceipt` 缺 blockHash/txIndex/to | 🟡 中 | 事件解析、tx 确认 |
| 6 | `eth_getBlockByHash` 始终 Null | 🟡 中 | 按 hash 查 block 的场景 |
| 7 | 所有带 blockTag 的方法忽略 blockTag | 🟡 中 | 历史状态查询 |
| 8 | `eth_gasPrice` 语义不完全匹配 | 🟢 低 | 1559 dApp gas 建议 |
| 9 | `eth_getProof` storage proof 是 stub | 🟢 低 | 轻客户端验证 |
| 10 | `eth_syncing` 始终 false | 🟢 低 | 同步状态显示 |

---

## 六、总结

Ethereum JSON-RPC 规范共 **47 个** `eth_*` 方法（不含已废弃的 4 个编译器方法）。Callchain 当前状态：

| 类别 | 数量 | 占比 |
|---|---|---|
| 完全实现（无缺陷） | 9 | 19% |
| 已实现但有缺陷 | 8 | 17% |
| 未实现 | 27 | 57% |
| 不需要（PoW / 已废弃） | 3 | 6% |
| **总计** | **47** | **100%** |

**`eth_getBlockByNumber` 的 tx list、`eth_getTransactionByHash` 的字段完整性、`eth_getLogs` 的 filter 支持** 是三大 blocking gap。补全后（Phase 1 + Phase 2）可让 ~80% 的 EVM dApp 正常工作。

后端数据（block、receipt、mempool）都已就绪，主要工作是 RPC 层的字段映射和索引建设。

---

## 七、逐接口实现建议

> 以下按 Ethereum JSON-RPC 规范分类，每个方法给出具体的实现路径、所需数据结构修改、以及代码位置。

---

### 7.1 Client / Node (6 个)

#### `eth_protocolVersion` — ❌ 未实现

**实现**：1 行代码，返回固定值。
```rust
Ok("0x41".to_string())  // 65，Ethereum 主网协议版本
```
或映射到 Callchain 的 `ProtocolVersion`：`format!("0x{:x}", state.protocol_version)`。

**代码位置**：`crates/rpc/src/standard.rs`

---

#### `eth_syncing` — ⚠️ hardcoded false

**当前**：始终返回 `false`。
**语义**：未同步返回 `false`；同步中返回 `{ startingBlock, currentBlock, highestBlock, ... }`。

**实现方案**：
- 短期：保持 `false`（单节点 devnet 可接受）
- 长期：在 `RpcState` 添加 `sync_progress: RwLock<Option<SyncProgress>>`，由 sync 模块写入，RPC 读取返回

```rust
pub struct SyncProgress {
    pub starting_block: u64,
    pub current_block: u64,
    pub highest_block: u64,
}
```

---

#### `eth_coinbase` — ❌ 未实现

**实现**：从 `validator_state` 读取当前节点的 validator address。

```rust
let vs = state.validator_state.read().map_err(...)?;
// 当前节点如果有 signer，找到对应的 validator
let coinbase = vs.get_all_validators().values()
    .find(|v| /* match against node identity */)
    .map(|v| v.address)
    .unwrap_or(Address::ZERO);
Ok(format!("0x{}", hex::encode(coinbase)))
```

**注意**：非 validator 节点没有 coinbase，返回 `Address::ZERO`。

---

#### `eth_chainId` — ✅ 已实现

无需修改。

---

#### `eth_mining` — ❌ 未实现

**实现**：1 行代码，PoS 链返回 `false`。
```rust
Ok(serde_json::Value::Bool(false))
```

---

#### `eth_hashrate` — ❌ 未实现

**实现**：1 行代码，PoS 链返回 `"0x0"`。
```rust
Ok("0x0".to_string())
```

---

### 7.2 Gas (3 个)

#### `eth_gasPrice` — ⚠️ 语义偏差

**当前**：返回 `base_fee`。
**问题**：Ethereum 语义是 "suggested gas price"，1559 链应返回 `baseFee + suggestedPriorityFee`。

**实现方案**：
- 短期：返回 `base_fee + 1`（让 dApp 能工作）
- 长期：实现动态 priority fee 建议（基于 mempool 拥堵程度）

```rust
let base_fee = state.fee_params.read()?.base_fee;
Ok(format!("0x{:x}", base_fee + 1))
```

---

#### `eth_maxPriorityFeePerGas` — ❌ 未实现

**实现**：返回固定小值（1 wei），让 1559 dApp 能构造 EIP-1559 tx。

```rust
Ok("0x1".to_string())  // 1 wei priority fee
```

**长期**：基于 mempool 中 pending tx 的 priority fee 分布计算中位数。

---

#### `eth_feeHistory` — ❌ 未实现

```
eth_feeHistory(blockCount, newestBlock, rewardPercentiles)
```

**实现方案 A（mock，让 dApp 工作）**：
```rust
Ok(serde_json::json!({
    "oldestBlock": "0x1",
    "baseFeePerGas": ["0x1", "0x1"],
    "gasUsedRatio": [0.5],
    "reward": [["0x1"]],
}))
```

**实现方案 B（真实数据）**：
1. 新增 `CallFeeHistory` DB table：`(block_number, base_fee, gas_used_ratio)`
2. 在 `bft_loop.rs` finalize 时写入
3. RPC 读取最近 N 个 block 的数据组装返回

---

#### Callchain Fee 模型与 Priority Fee 市场

Callchain 已实现完整的动态 priority fee 市场，支持用户通过 `max_priority_fee` 字段表达 priority fee 意图。

**现状（代码层面）**：

| 层面 | 状态 | 证据 |
|---|---|---|
| `compute_fee()` 函数 | ✅ 支持 | `compute_fee(gas_units, priority_fee_per_gas, base_fee)` 接受 priority fee 参数（`crates/protocol/src/tx/fee.rs:10`） |
| Fee 分配 | ✅ 支持 | `allocate_call_fee()` 区分 `base_fee_total`（50% burn + 50% validator）和 `priority_fee_total`（100% proposer）（`crates/protocol/src/tx/fee.rs:99`） |
| EVM precompile calls | ✅ 支持 | 所有协议操作通过 EVM 预编译 (`0x101`–`0x209`) 调用，标准 EIP-1559 交易 |
| Mempool/Execution | ✅ 动态 | `Block::execute()` 从 tx 读取 `max_priority_fee`，最低值为 `MIN_PRIORITY_FEE_PER_GAS = 1` wei |

**实际行为**：
- 用户提交的 tx 包含 `max_fee`（总费用上限）和 `max_priority_fee`（每 gas priority fee）
- 实际 priority fee = `max(tx.max_priority_fee, MIN_PRIORITY_FEE_PER_GAS)`
- 总 fee = `gas_units * (base_fee + priority_fee)`，但不超过 `max_fee`
- Priority fee 100% 归出块 validator；base fee 50% burn + 50% validator

**与 Ethereum EIP-1559 对比**：

```
Ethereum 1559:
  fee = gas_used * (base_fee + priority_fee)
  user specifies: max_fee_per_gas, max_priority_fee_per_gas

Callchain:
  fee = gas_used * (base_fee + priority_fee)   // priority fee 来自 tx.max_priority_fee
  user specifies: max_fee (total cap), max_priority_fee (per-gas)
```

**已实现的 RPC 方法**：
- `eth_gasPrice` — 返回 `base_fee + MIN_PRIORITY_FEE_PER_GAS`
- `eth_maxPriorityFeePerGas` — 返回 `0x1`（当前最低值）
- `eth_feeHistory` — 返回真实历史数据，含 per-block baseFee、gasUsedRatio 和 priority fee percentiles

**结论**：动态 priority fee 市场已完全实现。用户可通过提高 `max_priority_fee` 来加速交易确认。

---

### 7.3 Accounts / State (5 个)

#### `eth_accounts` — ❌ 未实现

**实现**：返回空数组（Callchain 没有本地 keystore，符合 Ethereum 规范）。
```rust
Ok(serde_json::Value::Array(vec![]))
```

---

#### `eth_getBalance` — ✅ 已实现（缺 blockTag）

**缺**：`blockTag` 参数被忽略，始终读当前 EVM state。

**修复**：
- `blockTag = "latest" | "pending"`：当前行为
- `blockTag = "earliest"`：返回 genesis balance
- `blockTag = 具体块号`：需要历史 EVM state。Callchain 目前只存当前 state，不支持归档查询

**建议**：短期对非 `"latest"` 返回错误 `"historical state query not supported"`；长期需要 EVM state 快照/归档节点支持。

---

#### `eth_getStorageAt` — ✅ 已实现（缺 blockTag）

**修复**：同 `eth_getBalance`，对非 `"latest"` 返回错误。

---

#### `eth_getTransactionCount` — ✅ 已实现（缺 blockTag）

**修复**：同 `eth_getBalance`。

---

#### `eth_getCode` — ✅ 已实现（缺 blockTag）

**修复**：同 `eth_getBalance`。

---

### 7.4 Blocks (8 个)

#### `eth_getBlockByHash` — ❌ 未实现

**缺**：hash→block_number 反向索引。

**实现方案**：

**Path A（推荐）**：新增 DB 索引
1. `crates/storage/src/reth_db.rs` 添加 `CallBlockHashIndex` table：
```rust
pub struct CallBlockHashIndex;
impl Table for CallBlockHashIndex {
    const NAME: &'static str = "call_block_hash_index";
    type Key = Vec<u8>;      // block_hash
    type Value = Vec<u8>;    // block_number (u64 BE)
}
```
2. `crates/node/src/state_persist.rs` 在 `persist_block` 时写入：
```rust
let hash = block.header.hash();
db_put::<CallBlockHashIndex>(db, hash.as_slice(), &height.to_be_bytes());
```
3. RPC 层读取索引 → load_block

**Path B（快速但低效）**：遍历 disk 上的 block 文件，计算 hash 匹配。O(n)，仅用于 devnet。

---

#### `eth_getBlockByNumber` — ⚠️ 严重缺陷

**缺 3 个关键字段**：`transactions`, `gasUsed`, `size`

**实现 `gasUsed`**：
```rust
// 从 receipts 累加该 block 的所有 gas_used
let receipts = state.get_receipts_by_block(block_number);
let gas_used: u64 = receipts.iter().map(|r| r.gas_used).sum();
```

**实现 `size`**：
```rust
let block_json = serde_json::to_vec(&block).unwrap_or_default();
let size = block_json.len() as u64;
```

**实现 `transactions`**（最复杂）：

`Block` 结构体有 `evm_txs: Vec<Vec<u8>>`（RLP raw）。所有交易均为标准 EVM 交易。

对于 `fullTransactions = false`：
```rust
let mut tx_hashes = Vec::new();
// EVM txs: 计算 envelope hash
for raw in &block.evm_txs {
    let hash = keccak256(raw);
    tx_hashes.push(format!("0x{}", hex::encode(hash)));
}
// Protocol txs: 用 compute_tx_hash()
// All transactions are EVM transactions (RLP raw)
// Precompile calls are included in evm_txs
```

对于 `fullTransactions = true`：
- EVM txs：RLP decode `TxEnvelope`，映射为 Ethereum tx JSON 对象
- Protocol txs：定义 ETH-compatible 映射格式：
```json
{
  "hash": "0x...",
  "nonce": "0x...",
  "blockHash": "0x...",
  "blockNumber": "0x...",
  "transactionIndex": "0x...",
  "from": "0x...",
  "to": "0x...",           // Transfer 指令的 to
  "value": "0x...",        // Transfer 指令的 amount
  "gas": "0x...",          // gas_limit
  "gasPrice": "0x...",     // max_fee
  "input": "0x",           // protocol tx 无 input
  "v": "0x...",
  "r": "0x...",
  "s": "0x...",
  "type": "0x0"
}
```

---

#### `eth_getBlockTransactionCountByHash` / `eth_getBlockTransactionCountByNumber`

**实现**：基于 `eth_getBlockByNumber` 的 block 加载逻辑，返回 tx count。

```rust
let block = state.load_block(height)?;
let count = block.evm_txs.len() + block.system_txs.len();
Ok(format!("0x{:x}", count))
```

---

#### `eth_getUncleCountByBlockHash` / `eth_getUncleCountByBlockNumber`

**实现**：PoS 无 uncle，始终返回 `"0x0"`。

---

#### `eth_getUncleByBlockHashAndIndex` / `eth_getUncleByBlockNumberAndIndex`

**实现**：PoS 无 uncle，始终返回 `null`。

---

### 7.5 Transactions (9 个)

#### `eth_getTransactionByHash` — ⚠️ 严重缺陷

**当前**：只从 receipt 查，只返回 5 个字段。

**完整实现路径**：

1. **查 receipt** → 获取 `block_number`
2. **load block** → 在 `block.evm_txs` 中匹配 tx hash
3. **EVM tx**：RLP decode → 填充所有 ETH 字段（nonce, gasPrice, gas, to, value, input, v, r, s, type）
4. **查 mempool**（如果 receipt 找不到）：在 `mempool.evm_pool` 中查找 pending tx

需要在 `RpcState` 添加 mempool by-hash 查询辅助方法：
```rust
pub fn get_mempool_tx_by_hash(&self, tx_hash: &TxHash) -> Option<MempoolTx> { ... }
```

---

#### `eth_getTransactionByBlockHashAndIndex` / `eth_getTransactionByBlockNumberAndIndex`

**实现**：
1. 通过 hash/number 加载 block
2. 按 index 取 tx（evm_txs）
3. 序列化为 ETH tx 格式（同 `eth_getTransactionByHash`）

---

#### `eth_getTransactionReceipt` — ⚠️ 字段不全

**缺**：`transactionIndex`, `blockHash`, `to`, `cumulativeGasUsed`, `effectiveGasPrice`, `contractAddress`, `logsBloom`

**修复方案**：

**扩展 `ProtocolReceipt`**（`crates/protocol/src/receipts.rs`）：
```rust
pub struct ProtocolReceipt {
    // ... 现有字段 ...
    pub block_hash: Hash,           // 新增
    pub transaction_index: u64,     // 新增
    pub to: Option<Address>,        // 新增
    pub contract_address: Option<Address>, // 新增
    pub cumulative_gas_used: u64,   // 新增
}
```

**在 receipt 生成时填充**（`bft_loop.rs:577`, `block_producer.rs:316`, `sync.rs:140`）：
- `block_hash`：从 block header 取
- `transaction_index`：遍历 block 时维护计数器
- `to`：从 tx 中提取（EVM tx 的 to 字段；protocol tx 的 Transfer 指令的 to）
- `contract_address`：如果 EVM tx 是合约创建（to = None），计算 `create_address`
- `cumulative_gas_used`：累加到当前 tx 为止的 gas

**更新 `receipt_to_json`**（`standard.rs:455`）：
```rust
value["transactionIndex"] = format!("0x{:x}", receipt.transaction_index);
value["blockHash"] = format!("0x{}", hex::encode(receipt.block_hash));
value["to"] = receipt.to.map(|a| format!("{:?}", a)).unwrap_or("0x".into());
value["contractAddress"] = receipt.contract_address.map(|a| format!("{:?}", a));
value["cumulativeGasUsed"] = format!("0x{:x}", receipt.cumulative_gas_used);
value["effectiveGasPrice"] = format!("0x{:x}", receipt.fee_amount / receipt.gas_used as u128);
```

---

#### `eth_getBlockReceipts` — ❌ 未实现

**后端已就绪**：`RpcState::get_receipts_by_block()` 和 `state_persist::load_receipts_by_block()` 已实现。

**实现**：
```rust
module.register_async_method("eth_getBlockReceipts", |params, state, _ctx| async move {
    let block_tag: String = params.one().map_err(...)?;
    let block_number = parse_block_tag(&block_tag, state.get_current_block());
    let receipts = state.get_receipts_by_block(block_number);
    let json_receipts: Vec<_> = receipts.iter().map(receipt_to_json).collect();
    Ok::<_, ErrorObjectOwned>(json_receipts)
})?;
```

---

#### `eth_sendTransaction` — ❌ 未实现

**实现**：返回 unsupported error。
```rust
Err(invalid_params("eth_sendTransaction is not supported. Use eth_sendRawTransaction instead.".into()))
```

---

#### `eth_sendRawTransaction` — ✅ 已实现

无需修改。

---

#### `eth_call` — ✅ 已实现（缺 blockTag）

对非 `"latest"` 返回 `"historical state query not supported"`。

---

#### `eth_estimateGas` — ✅ 已实现（缺 blockTag）

对非 `"latest"` 返回 `"historical state query not supported"`。

---

### 7.6 Logs / Filter (7 个)

#### `eth_getLogs` — ⚠️ 严重缺陷

**缺 filter**：`fromBlock`, `toBlock`, `blockHash`, `topics`
**缺返回字段**：`blockNumber`, `blockHash`, `transactionIndex`, `logIndex`, `removed`

**实现 filter 逻辑**：

```rust
// 解析 filter
let from_block = filter.get("fromBlock").map(parse_block_tag).unwrap_or(0);
let to_block = filter.get("toBlock").map(parse_block_tag).unwrap_or(current);
let block_hash = filter.get("blockHash").and_then(|v| v.as_str());
let topics = filter.get("topics").and_then(|v| v.as_array())
    .map(|arr| arr.iter().filter_map(|t| t.as_str().map(parse_hash)).collect());
```

**查询策略**：
- 如果有 `blockHash`：hash→height → 只查该 block 的 receipts
- 如果有 `fromBlock`/`toBlock`：遍历该范围的 receipts（用 `CallReceiptsByBlock` 索引）
- 如果有 `topics`：对每个 receipt 的 logs 做 topic 匹配
- 如果有 `address`：复用现有 `log_index` 优化

**填充返回字段**：
```rust
serde_json::json!({
    "address": format!("{:?}", log.address),
    "topics": ..., "data": ...,
    "blockNumber": format!("0x{:x}", receipt.block_number),
    "blockHash": format!("0x{}", hex::encode(receipt.block_hash)),
    "transactionHash": format!("0x{}", hex::encode(receipt.tx_hash)),
    "transactionIndex": format!("0x{:x}", receipt.transaction_index),
    "logIndex": format!("0x{:x}", log_idx),
    "removed": false,
})
```

---

#### `eth_newFilter` / `eth_newBlockFilter` / `eth_newPendingTransactionFilter`

**需要新增 `FilterManager`**：

```rust
// crates/rpc/src/handlers/state.rs
pub struct FilterManager {
    filters: HashMap<u64, Filter>,
    next_id: u64,
    // 每个 filter 记录 last polled block
}

pub enum Filter {
    Log(LogFilter),              // eth_newFilter
    Block,                       // eth_newBlockFilter
    PendingTransaction,          // eth_newPendingTransactionFilter
}

pub struct LogFilter {
    from_block: u64,
    to_block: Option<u64>,
    addresses: Vec<Address>,
    topics: Vec<Option<Vec<Hash>>>,
    last_polled_block: u64,
}
```

**`eth_newFilter`**：解析 filter 参数，分配 ID，存入 FilterManager，返回 ID。
**`eth_newBlockFilter`**：分配 ID，存入 Block filter，返回 ID。
**`eth_newPendingTransactionFilter`**：分配 ID，存入 Pending filter，返回 ID。

---

#### `eth_getFilterChanges`

**实现**：根据 filter 类型返回自上次轮询以来的新数据。
- `Log` filter：从 `last_polled_block + 1` 到 current block，查 receipts，匹配 filter 条件
- `Block` filter：返回自上次轮询以来的新 block hashes
- `Pending` filter：返回自上次轮询以来的新 pending tx hashes

更新 `last_polled_block` 为 current block。

---

#### `eth_getFilterLogs`

**实现**：同 `eth_getFilterChanges` 但返回所有匹配的 logs（不更新 cursor）。

---

#### `eth_uninstallFilter`

**实现**：从 FilterManager 中删除 filter ID。
```rust
let removed = filter_manager.remove(filter_id);
Ok(serde_json::Value::Bool(removed))
```

---

### 7.7 PoW (3 个) — ⛔ 不需要

#### `eth_getWork` / `eth_submitWork` / `eth_submitHashrate`

**实现**：注册方法但返回 unsupported error。
```rust
module.register_async_method("eth_getWork", |_params, _state, _ctx| async move {
    Err(internal_error("eth_getWork is not supported on PoS chains".into()))
})?;
```

---

### 7.8 Signing (3 个) — 不支持

#### `eth_sign` / `eth_signTransaction`

**实现**：返回 unsupported error（Callchain 没有本地 keystore）。
```rust
Err(invalid_params("eth_sign is not supported. Use a local wallet to sign transactions.".into()))
```

---

### 7.9 Proofs (1 个)

#### `eth_getProof` — ⚠️ stub

**当前**：accountProof 和 storageProof 都返回 `[state_root]` stub。

**真正 Merkle proof 的实现**：
需要 EVM state 使用 Merkle Patricia Trie 存储账户和 storage。当前 `EvmState` 使用 HashMap，不支持 proof 生成。

**方案**：
- 短期：保持 stub，在文档中标注 "storage proofs not yet supported"
- 长期：将 `EvmState` 迁移到 reth 的 `StateRoot` + `TrieWitness` 系统，或使用 alloy-trie

---

### 7.10 Access Lists (1 个)

#### `eth_createAccessList`

**实现**：返回 unsupported error。
```rust
Err(internal_error("eth_createAccessList is not supported".into()))
```

**长期**：需要 EVM 模拟执行时跟踪 touched addresses/storage slots，生成 access list。

---

### 7.11 WebSocket (2 个)

#### `eth_subscribe` / `eth_unsubscribe`

**需要扩展 `ws.rs`**：

1. **新增广播通道**：
```rust
// SubscriptionManager 新增
pub eth_new_heads_tx: broadcast::Sender<serde_json::Value>,
pub eth_logs_tx: broadcast::Sender<serde_json::Value>,
pub eth_pending_tx_tx: broadcast::Sender<serde_json::Value>,
```

2. **注册 `eth_subscribe` / `eth_unsubscribe`**：
```rust
module.register_subscription("eth_subscribe", "eth_subscription", "eth_unsubscribe", ...)?;
```

3. **事件类型处理**：
- `newHeads`：在 block finalize 时广播 ETH 格式的 block header JSON
- `logs`：在 block finalize 时，对匹配 filter 的 logs 广播
- `newPendingTransactions`：在 `submit_evm_tx` / `insert_protocol_tx` 时广播 tx hash

4. **block finalize 广播 hook**：在 `bft_loop.rs` / `block_producer.rs` finalize 后调用：
```rust
state.subscriptions.broadcast_eth_new_heads(block_header_json);
state.subscriptions.broadcast_eth_logs(matched_logs);
```

---

## 八、数据结构变更清单

实现以上接口需要以下数据结构变更：

| 变更 | 位置 | 影响 |
|---|---|---|
| `ProtocolReceipt` 新增 `block_hash`, `transaction_index`, `to`, `contract_address`, `cumulative_gas_used` | `crates/protocol/src/receipts.rs` | receipt 序列化兼容性 |
| `CallBlockHashIndex` 新表 | `crates/storage/src/reth_db.rs` | block hash 反向索引 |
| `CallFeeHistory` 新表 | `crates/storage/src/reth_db.rs` | fee history 存储 |
| `FilterManager` 新结构 | `crates/rpc/src/handlers/state.rs` | filter 生命周期管理 |
| `SyncProgress` 新结构 | `crates/rpc/src/handlers/state.rs` | 同步进度跟踪 |
| `SubscriptionManager` 新增 ETH 广播通道 | `crates/rpc/src/ws.rs` | WebSocket ETH 订阅 |

---

## 九、工作量估算

| Phase | 接口数 | 预计工作量 | 阻塞 dApp 比例 |
|---|---|---|---|
| Phase 1（核心修复） | 8 个 | 2-3 天 | 解决 80% blocking gap |
| Phase 2（日志/filter） | 7 个 | 2-3 天 | 事件监听可用 |
| Phase 3（trivial/兼容） | 12 个 | 0.5-1 天 | 兼容性 fluff |
| Phase 4（WebSocket） | 2 个 | 1 天 | 实时推送 |
| Phase 5（高级功能） | 3 个 | 待定 | access list / proof |
| **总计** | **32 个** | **6-8 天** | **~90% dApp 兼容** |
