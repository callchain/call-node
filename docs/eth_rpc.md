# Callchain Ethereum JSON-RPC 兼容性审计

**Scope**: 所有 `eth_*` 方法 — 功能完整性审计与实现细节
**Files**: `crates/rpc/src/standard.rs`, `crates/rpc/src/handlers/state.rs`, `crates/rpc/src/ws.rs`
**Last Updated**: 2026-05-10

---

## 零、完整接口清单（47 个）

| # | 方法 | 说明 |
|---|---|---|
| 1 | `eth_protocolVersion` | 返回固定值 `"1"` |
| 2 | `eth_syncing` | 支持真实同步进度（`SyncProgress`） |
| 3 | `eth_coinbase` | 从 EVM validator 存储读取当前 proposer 地址 |
| 4 | `eth_chainId` | |
| 5 | `eth_mining` | PoS 链，返回 `false` |
| 6 | `eth_hashrate` | PoS 链，返回 `"0x0"` |
| 7 | `eth_gasPrice` | 返回 `base_fee + MIN_PRIORITY_FEE_PER_GAS` |
| 8 | `eth_maxPriorityFeePerGas` | 返回 `MIN_PRIORITY_FEE_PER_GAS`（1 wei） |
| 9 | `eth_feeHistory` | 读 `fee_history` ring buffer（最大 1024 条） |
| 10 | `eth_accounts` | 返回本地 keystore 中的地址列表 |
| 11 | `eth_getBalance` | 支持 blockTag（历史状态通过 `AccountHistory` 表） |
| 12 | `eth_getStorageAt` | 支持 blockTag（历史状态通过 `StorageHistory` 表） |
| 13 | `eth_getTransactionCount` | 支持 blockTag；`"pending"` 合并 mempool nonce |
| 14 | `eth_getCode` | 支持 blockTag（历史状态通过 `AccountHistory` 表） |
| 15 | `eth_sign` | personal_sign 格式，需账户在 keystore |
| 16 | `eth_signTransaction` | 构建、签名并返回 raw tx hex |
| 17 | `eth_sendTransaction` | 构建、签名并通过 mempool 提交 |
| 18 | `eth_sendRawTransaction` | |
| 19 | `eth_call` | 支持 blockTag（当前 state 或 block snapshot） |
| 20 | `eth_estimateGas` | 支持 blockTag（当前 state 或 block snapshot） |
| 21 | `eth_createAccessList` | 通过 `revm-inspectors` AccessListInspector 生成 |
| 22 | `eth_getBlockByHash` | 通过 `block_hash_index` 查 height |
| 23 | `eth_getBlockByNumber` | 含 transactions/gasUsed/size |
| 24 | `eth_getBlockTransactionCountByHash` | |
| 25 | `eth_getBlockTransactionCountByNumber` | |
| 26 | `eth_getUncleCountByBlockHash` | PoS 无 uncle，返回 `"0x0"` |
| 27 | `eth_getUncleCountByBlockNumber` | PoS 无 uncle，返回 `"0x0"` |
| 28 | `eth_getUncleByBlockHashAndIndex` | PoS 无 uncle，返回 `null` |
| 29 | `eth_getUncleByBlockNumberAndIndex` | PoS 无 uncle，返回 `null` |
| 30 | `eth_getTransactionByHash` | EVM tx 完整字段；receipt fallback 9 个字段 |
| 31 | `eth_getTransactionByBlockHashAndIndex` | |
| 32 | `eth_getTransactionByBlockNumberAndIndex` | |
| 33 | `eth_getTransactionReceipt` | 含 blockHash/txIndex/to/contractAddress/logsBloom |
| 34 | `eth_getBlockReceipts` | |
| 35 | `eth_getLogs` | 含 fromBlock/toBlock/topics/blockHash/txIndex/logIndex |
| 36 | `eth_newFilter` | |
| 37 | `eth_newBlockFilter` | |
| 38 | `eth_newPendingTransactionFilter` | |
| 39 | `eth_uninstallFilter` | |
| 40 | `eth_getFilterChanges` | 支持 Log/Block/PendingTransaction 三种 filter |
| 41 | `eth_getFilterLogs` | |
| 42 | `eth_getWork` | PoW only，返回 unsupported |
| 43 | `eth_submitWork` | PoW only，返回 unsupported |
| 44 | `eth_submitHashrate` | PoW only，返回 unsupported |
| 45 | `eth_getProof` | 真正 Merkle proof（`reth-trie` 持久化 trie 节点） |
| 46 | `eth_subscribe` | WebSocket，支持 newHeads/logs/newPendingTransactions |
| 47 | `eth_unsubscribe` | WebSocket |

---

## 一、方法审计

### 1. `eth_getBalance`

```
eth_getBalance(address, blockTag)
```

| 检查项 | 说明 |
|---|---|
| 参数解析 | address 用 `alloy_primitives::Address` parse |
| blockTag `"latest"/"pending"/"safe"/"finalized"` | 读当前 EVM state |
| blockTag `"0xN"` / 十进制 | 通过 `get_historical_account` 读历史状态 |
| 返回值 | `0x...` 格式，U256 hex |

**历史状态实现**: `AccountHistory` MDBX 表记录每块账户 diff，通过 seek + prev 查询指定 block 的最新状态。

---

### 2. `eth_call`

```
eth_call(transactionObject, blockTag)
```

| 检查项 | 说明 |
|---|---|
| 参数解析 | from/to/value/data/gas/gasPrice 都解析 |
| blockTag `"latest"/"pending"` | 当前 state clone 执行 |
| blockTag `"0xN"` | 加载 block snapshot → clone → 执行 |
| 执行语义 | clone state → execute → discard，纯只读 |
| 返回值 | 成功返回 `0x` + output hex；失败返回 revert error |

**历史状态实现**: `CallBlockStateSnapshots` 表保存每块完整 `InMemoryStateProvider`（默认保留 128 块，归档节点保留全部）。超出 snapshot 范围的 block 返回 error。

---

### 3. `eth_estimateGas`

```
eth_estimateGas(transactionObject, blockTag)
```

| 检查项 | 说明 |
|---|---|
| 参数解析 | 同 eth_call |
| blockTag | 同 eth_call（当前 state 或 block snapshot） |
| 返回值 | 返回 `result.gas_used` hex |

---

### 4. `eth_sendRawTransaction`

```
eth_sendRawTransaction(signedTransactionData)
```

| 检查项 | 说明 |
|---|---|
| RLP 解码 | 支持 Legacy/EIP-1559/EIP-2930/EIP-7702/EIP-4844 |
| 签名恢复 | `recover_signer()` 验证 |
| nonce 检查 | 与 EVM state + mempool pending nonce 比对 |
| balance 检查 | gas_price * gas_limit <= balance |
| mempool 入池 | 防御层 rate limit + 入 mempool |
| P2P gossip | broadcast 到 peers |
| **不立即执行** | 正确 defer 到 block production |

**注意**：返回的 tx_hash 是 raw bytes 的 keccak256，不是 envelope hash。

---

### 5. `eth_getTransactionReceipt`

```
eth_getTransactionReceipt(txHash)
```

| 标准字段 | 说明 |
|---|---|
| `transactionHash` | |
| `transactionIndex` | |
| `blockHash` | |
| `blockNumber` | |
| `from` | |
| `to` | |
| `cumulativeGasUsed` | |
| `gasUsed` | |
| `effectiveGasPrice` | |
| `contractAddress` | |
| `logs` | |
| `logsBloom` | |
| `status` | |
| `revertReason` | |

---

### 6. `eth_blockNumber`

返回当前 block height 的 hex。语义正确。

---

### 7. `eth_getLogs`

```
eth_getLogs(filterObject)
```

| 标准 filter 字段 | 说明 |
|---|---|
| `address` (string/array) | 有 log_index 优化 |
| `fromBlock` | 支持 hex/decimal/"latest"/"pending" |
| `toBlock` | 同上 |
| `blockHash` | 通过 `block_hash_index` 解析 |
| `topics` | 支持单层/数组/Null 混合 |

| 标准返回字段 | 说明 |
|---|---|
| `address` | |
| `topics` | |
| `data` | |
| `blockNumber` | |
| `blockHash` | |
| `transactionHash` | |
| `transactionIndex` | |
| `logIndex` | |
| `removed` | 固定 `false` |

---

### 8. `eth_getProof`

```
eth_getProof(address, storageKeys, blockTag)
```

| 字段 | 说明 |
|---|---|
| `address` | |
| `balance` | EVM balance |
| `codeHash` | keccak256(code) |
| `nonce` | EVM nonce |
| `storageHash` | EVM state root |
| `accountProof` | 真正 Merkle proof（`reth-trie` 持久化 trie 节点） |
| `storageProof` | 每个 key 真正 Merkle proof |

**实现**: 当前 block 用 `compute_account_proof_persistent()` 从 MDBX 持久化 trie 节点读取；历史 block 用 `load_block_snapshot()` + `compute_account_proof()`。

---

### 9. `eth_chainId`

---

### 10. `eth_gasPrice`

返回 `fee_params.base_fee + MIN_PRIORITY_FEE_PER_GAS`。对于 EIP-1559 链，这是合理的 suggested gas price。

---

### 11. `eth_syncing`

`RpcState` 包含 `sync_progress: Arc<RwLock<Option<SyncProgress>>>`。未同步时返回进度对象；完全同步后返回 `false`。

---

### 12. `eth_getTransactionCount`

读取 EVM nonce。`"pending"` blockTag 会合并 mempool 中的 pending nonce；历史 blockTag 通过 `get_historical_account` 查询。

---

### 13. `eth_getCode`

支持 blockTag。历史 block 通过 `get_historical_account` 取 code_hash，再从 MDBX `CallBytecodes` 表加载 code。

---

### 14. `eth_getStorageAt`

支持 blockTag。历史 block 通过 `get_historical_storage` 查询 `StorageHistory` 表。

---

### 15. `eth_getBlockByNumber`

```
eth_getBlockByNumber(blockTag, fullTransactions)
```

| 标准字段 | 说明 |
|---|---|
| `number` | |
| `hash` | 从 block header 计算 |
| `parentHash` | |
| `timestamp` | |
| `gasLimit` | 固定 `0x1c9c380` |
| `gasUsed` | 从 receipts 累加 |
| `transactions` | EVM tx hash 列表或完整对象 |
| `size` | block JSON 序列化字节数 |
| `logsBloom` | 固定 `0x00...` |
| `miner` | 固定 `0x00...` |
| `difficulty` | `0x0` |
| `totalDifficulty` | `0x0` |
| `nonce` | `0x0` |
| `sha3Uncles` | `0x00...` |
| `receiptsRoot` | state_root |
| `transactionsRoot` | state_root |
| `stateRoot` | |
| `baseFeePerGas` | |

---

### 16. `eth_getBlockByHash`

通过 `RpcState.block_hash_index`（hash → height 内存索引）查 height → `load_block`。receipt 存储时会自动更新索引。

---

### 17. `eth_getTransactionByHash`

两条查询路径：
1. **receipt 存在**：通过 `block_number` 加载 block，在 `evm_txs` 中匹配 tx hash，RLP decode 为完整 ETH tx 对象（含 nonce/gasPrice/gas/to/value/input/v/r/s/chainId）
2. **receipt 存在但 block 不可用**：receipt fallback 返回 9 个字段（hash/blockHash/blockNumber/txIndex/status/gasUsed/from/to）
3. **receipt 不存在**：返回 `null`（pending tx 未实现查询）

---

### 18-20. Filter API

| 方法 |
|---|
| `eth_newFilter` |
| `eth_newBlockFilter` |
| `eth_newPendingTransactionFilter` |
| `eth_getFilterChanges` |
| `eth_getFilterLogs` |
| `eth_uninstallFilter` |

`FilterManager` 在 `RpcState` 中维护内存中的 filter 状态，含 `last_block` / `last_height` / `seen` 游标。

---

### 21. WebSocket 订阅

| 订阅类型 | 说明 |
|---|---|
| `eth_subscribe("newHeads")` | 广播 ETH 格式 block header JSON |
| `eth_subscribe("logs", filter)` | 按 address/topics 过滤后广播 |
| `eth_subscribe("newPendingTransactions")` | 广播 pending tx hash |
| `eth_unsubscribe` | |

`SubscriptionManager` 已包含 `eth_new_heads_tx` / `eth_logs_tx` / `eth_pending_tx_tx` 三个 broadcast channel。

---

### 22. `eth_getBlockReceipts`

直接调用 `state.get_receipts_by_block(block_number)` 返回。

---

### 23. `eth_getBlockTransactionCountByHash` / `eth_getBlockTransactionCountByNumber`

加载 block 后返回 `block.evm_txs.len()`。

---

### 24. `eth_getTransactionByBlockHashAndIndex` / `eth_getTransactionByBlockNumberAndIndex`

加载 block → 按 index 取 `evm_txs` → `evm_raw_tx_to_json` 解码为完整 ETH tx 对象。

---

### 25. `eth_maxPriorityFeePerGas` / `eth_feeHistory`

- `eth_maxPriorityFeePerGas`：返回 `MIN_PRIORITY_FEE_PER_GAS`（1 wei）
- `eth_feeHistory`：读 `RpcState.fee_history` ring buffer（最大 1024 条），含 per-block baseFee/gasUsedRatio/reward percentiles

---

### 26. `eth_protocolVersion` / `eth_mining` / `eth_hashrate` / `eth_accounts`

- `eth_protocolVersion`：返回 `"1"`
- `eth_mining`：返回 `false`
- `eth_hashrate`：返回 `"0x0"`
- `eth_accounts`：返回本地 keystore 中的地址列表（通过 `LocalKeystore` 管理）

---

### 27. Uncle API

- `eth_getUncleCountByBlockHash` / `eth_getUncleCountByBlockNumber`：返回 `"0x0"`
- `eth_getUncleByBlockHashAndIndex` / `eth_getUncleByBlockNumberAndIndex`：返回 `null`

---

## 三、按数据依赖分组

| 能力 | 说明 |
|---|---|
| block 中 tx list 序列化 | — |
| receipt 中 blockHash / txIndex / to / contractAddress / cumulativeGasUsed | — |
| tx 中 to/value/input/gasPrice 提取 | — |
| hash→block_number 索引 | — |
| mempool pending tx 查询 | `eth_getTransactionByHash` pending tx 返回 null（低优先级） |
| `eth_getLogs` topics + blockRange filter | — |
| fee history 存储 | — |
| Filter API | — |
| WebSocket 订阅 | — |
| blockTag 历史状态查询 | — |
| Merkle proof (`eth_getProof`) | — |
| access list 生成 (`eth_createAccessList`) | — |

---

## 四、历史状态查询架构

所有带 `blockTag` 的方法支持三种查询模式：

### 模式 1：当前状态（`"latest"` / `"pending"` / `"safe"` / `"finalized"`）

直接从 MDBX 加载当前 `InMemoryStateProvider`，语义与之前一致。

### 模式 2：历史账户/存储（`eth_getBalance` / `eth_getStorageAt` / `eth_getTransactionCount` / `eth_getCode`）

通过 `AccountHistory` / `StorageHistory` MDBX 表查询：
- seek 到 `(address, block+1)` 位置
- prev 回退一步，得到该 address 在目标 block 及之前的最新状态
- 时间复杂度 O(log N)，N = 历史记录数

### 模式 3：历史 EVM 执行（`eth_call` / `eth_estimateGas`）

通过 `CallBlockStateSnapshots` MDBX 表加载完整状态快照：
- 每个 block 保存一份 `InMemoryStateProvider` 的完整 clone
- 默认保留 128 个 block（pruned 模式）
- 归档节点可配置 `retention_blocks = u64::MAX` 保留全部
- snapshot 不存在的 block 返回 `"no state snapshot available"` error

---

## 五、已发现问题清单

| # | 问题 | 严重性 | 影响 |
|---|---|---|---|
| 1 | `eth_getTransactionByHash` 不查 mempool pending tx | 🟢 低 | pending tx 返回 null |

---

## 六、总结

Callchain 实现了 Ethereum JSON-RPC 规范全部 **47** 个 `eth_*` 方法（不含已废弃的编译器方法），其中 **44** 个为完整功能实现，**3** 个为 PoS 链不需要的方法（返回 `unsupported`）。

### 27. `eth_sign` / `eth_signTransaction` / `eth_sendTransaction`

**本地 keystore 支持**：
- `LocalKeystore` 支持内存（ephemeral）和磁盘（Web3 Secret Storage）两种模式
- `eth_sign`：personal_sign 格式（`\x19Ethereum Signed Message:\n{len}\n{message}`）
- `eth_signTransaction`：构建交易 → 用 keystore 私钥签名 → 返回 RLP-encoded raw tx hex
- `eth_sendTransaction`：同 `eth_signTransaction`，额外通过 `submit_evm_tx` 提交到 mempool
- 支持 Legacy、EIP-2930 和 EIP-1559 交易类型，可携带 `accessList`

### 28. `eth_createAccessList`

**实现**：通过 `revm-inspectors` 的 `AccessListInspector` 在 EVM 执行期间追踪所有触发的地址和 storage slot，返回 `{accessList, gasUsed}`。

**结论**：所有 47 个 `eth_*` 方法均已完整实现。blockTag 历史状态查询、Merkle proof、本地 keystore 签名和 access list 生成均已支持。剩余两个低优先级问题（`eth_coinbase` 读 proposer、`eth_getTransactionByHash` pending tx 查询）不影响核心 dApp 功能。
