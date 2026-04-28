# EVM State Merkle Patricia Trie 重构方案

## 背景

当前 `call-node` 的 EVM 状态层 (`EvmState`) 使用内存 `HashMap` 存储账户和 storage，并通过 `alloy_trie::HashBuilder` 一次性聚合所有叶子节点计算 `state_root`。`HashBuilder` 是单向聚合器：只能输入叶子、输出 root，无法回溯路径生成 Merkle proof。

这导致 `eth_getProof` 只能返回 `[state_root]` stub，无法满足轻客户端验证和 dApp 的 EIP-1186 证明需求。

本文档描述**方案 B：DB-backed 增量 trie**，将 EVM 状态持久化到 MDBX，使用 `reth-trie` 维护完整的 Merkle Patricia Trie，支持增量更新和按需 proof 生成。

---

## 目标

1. `eth_getProof` 返回真正的 EIP-1186 兼容 account proof 和 storage proof
2. `compute_state_root()` 保持亚秒级性能（增量更新，非全量重建）
3. block 内的 snapshot/rollback 语义不变
4. 不影响原生协议层（`AccountState`、`ShieldedState` 等）的存储模型
5. 向后兼容：支持现有节点的数据 migration

---

## 非目标

- 历史状态查询（archive node）：只支持最新状态的 proof 和 root
- 跨 block 的状态回滚：只支持 block 内的 transaction-level rollback
- 替换原生协议层的存储模型

---

## 架构设计

### 整体数据流

```
+---------------+      +------------------+      +------------------+
|  EVM 交易执行  |  ->  |  EvmState (cache) |  ->  |  reth-trie (DB)  |
|  (revm)       |      |  (本 block 修改)  |      |  (MDBX 持久化)   |
+---------------+      +------------------+      +------------------+
                              |                           |
                              v                           v
                       snapshot/rollback           state_root / proof
```

### EvmState 新结构

```rust
pub struct EvmState {
    /// MDBX 数据库句柄（只读访问，用于 miss cache 时回退）
    db: Option<Arc<DatabaseEnv>>,

    /// 本 block / 本 transaction 内已修改但未提交的账户缓存
    /// Key: plain Address
    cache: HashMap<Address, EvmAccount>,

    /// 本 block / 本 transaction 内已删除的账户地址集合
    deleted: HashSet<Address>,

    /// 上一个已 finalize block 的 state root
    last_root: B256,

    /// 上一个已 finalize block 的 block number（用于 DB 前缀隔离）
    last_height: u64,
}
```

- `cache` 和 `deleted` 构成 **overlay**：优先读 cache，miss 时读 DB，deleted 标记短路返回 empty
- `last_root` 和 `last_height` 在 block finalize 时更新
- `clone()` 仍可用于 snapshot：clone cache + deleted + last_root（O(m)，m = 本 block 修改数）

---

## 存储层改动

### 新增 DB 表（`crates/storage/src/reth_db.rs`）

```rust
/// Hashed account state: keccak256(address) -> RLP([nonce, balance, storage_root, code_hash])
pub struct CallHashedAccounts;
impl Table for CallHashedAccounts {
    const NAME: &'static str = "call_hashed_accounts";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;   // 32 bytes keccak256(address)
    type Value = Vec<u8>; // RLP encoded account
}

/// Hashed storage state: (keccak256(address), keccak256(slot)) -> RLP(value)
pub struct CallHashedStorage;
impl Table for CallHashedStorage {
    const NAME: &'static str = "call_hashed_storage";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;   // 64 bytes: [32B addr_hash | 32B slot_hash]
    type Value = Vec<u8>; // RLP encoded U256
}

/// Account trie nodes: trie_node_key -> trie_node_rlp
/// trie_node_key 编码了 path nibbles + node type (branch/extension/leaf)
pub struct CallAccountTrie;
impl Table for CallAccountTrie {
    const NAME: &'static str = "call_account_trie";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}

/// Storage trie nodes: (keccak256(address), trie_node_key) -> trie_node_rlp
pub struct CallStorageTrie;
impl Table for CallStorageTrie {
    const NAME: &'static str = "call_storage_trie";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;   // 64+ bytes: [32B addr_hash | trie_node_key]
    type Value = Vec<u8>;
}
```

注册到 `CallTables::tables()`。

### 现有表处理

| 现有表 | 处理方式 |
|--------|---------|
| `CallEvmAccounts` | **废弃**，迁移到 `CallHashedAccounts` |
| `CallEvmStorage` | **废弃**，迁移到 `CallHashedStorage` |
| `CallEvmContracts` | **保留**（字节码本身不参与 trie），或合并进 `CallHashedAccounts` 的 code_hash |

---

## EVM 状态层改动

### `crates/evm/src/state.rs`

#### 1. 新增 `EvmState` 构造函数

```rust
impl EvmState {
    pub fn new() -> Self { /* 纯内存模式（测试用） */ }

    pub fn with_db(db: Arc<DatabaseEnv>, last_root: B256, last_height: u64) -> Self {
        Self { db: Some(db), cache: HashMap::new(), deleted: HashSet::new(), last_root, last_height }
    }
}
```

#### 2. 读路径（带 cache + DB fallback）

```rust
pub fn get_account(&self, address: &Address) -> Option<EvmAccount> {
    if self.deleted.contains(address) {
        return None;
    }
    if let Some(acc) = self.cache.get(address) {
        return Some(acc.clone());
    }
    // DB fallback: 通过 plain address -> keccak256 -> lookup CallHashedAccounts
    self.db.as_ref()?.load_hashed_account(address)
}

pub fn get_storage(&self, address: &Address, key: U256) -> U256 {
    if let Some(acc) = self.cache.get(address) {
        return acc.storage.get(&key).copied().unwrap_or_default();
    }
    self.db.as_ref()?.load_hashed_storage(address, key).unwrap_or_default()
}
```

#### 3. 写路径（只改 cache，不触 trie）

```rust
pub fn set_balance(&mut self, address: Address, balance: U256) {
    let acc = self.cache.entry(address).or_insert_with(|| {
        self.load_or_default(address)
    });
    acc.balance = balance;
}

pub fn set_storage(&mut self, address: Address, key: U256, value: U256) {
    let acc = self.cache.entry(address).or_insert_with(|| {
        self.load_or_default(address)
    });
    acc.storage.insert(key, value);
}
```

#### 4. 计算 state_root（增量）

```rust
use reth_trie::{StateRoot, HashedPostState, HashedPostAccount};

pub fn compute_state_root(&self) -> B256 {
    // 1. 构建本 block 的增量变更（HashedPostState）
    let mut post_state = HashedPostState::default();

    for (addr, acc) in &self.cache {
        let addr_hash = keccak256(*addr);
        if self.deleted.contains(addr) {
            post_state.accounts.insert(addr_hash, HashedPostAccount::new(None));
        } else {
            let storage_root = self.compute_storage_root(addr, acc);
            let rlp_account = rlp_encode_account(acc.nonce, acc.balance, storage_root, code_hash(&acc.code));
            post_state.accounts.insert(addr_hash, HashedPostAccount::new(Some(rlp_account)));

            // 收集 storage 变更
            for (slot, value) in &acc.storage {
                let slot_hash = keccak256(slot.to_be_bytes::<32>());
                post_state.storages
                    .entry(addr_hash)
                    .or_default()
                    .insert(slot_hash, value.clone());
            }
        }
    }

    // 2. 增量计算新 root
    if let Some(ref db) = self.db {
        StateRoot::incremental_root(db, self.last_root, &post_state)
            .expect("trie computation failed")
    } else {
        // 纯内存模式 fallback：临时构建 trie
        self.compute_state_root_from_scratch()
    }
}
```

> **注意**：`reth_trie::StateRoot::incremental_root` 是一个假设 API。实际 reth-trie 的增量 API 可能是 `StateRoot::from_tx` + `TrieUpdates` 模式。具体实现时需要查阅 reth-trie 的源码适配。

#### 5. 生成 proof（按需构建 witness）

```rust
use reth_trie::TrieWitness;

pub fn get_proof(&self, address: Address, storage_keys: Vec<U256>) -> AccountProof {
    let addr_hash = keccak256(address);

    // 1. 先确保 cache 中的变更被反映到临时 overlay
    let mut overlay = self.build_trie_overlay();

    // 2. 用 TrieWitness 收集路径节点
    let account_proof_nodes = TrieWitness::account_witness(&overlay, addr_hash);
    let storage_proofs: Vec<StorageProof> = storage_keys.iter().map(|key| {
        let slot_hash = keccak256(key.to_be_bytes::<32>());
        let nodes = TrieWitness::storage_witness(&overlay, addr_hash, slot_hash);
        StorageProof { key: *key, value: self.get_storage(&address, *key), proof: nodes }
    }).collect();

    AccountProof {
        address,
        balance: self.get_balance(&address),
        nonce: self.get_nonce(&address),
        code_hash: code_hash(&self.get_code(&address)),
        storage_root: self.get_account(&address).map(|a| a.storage_root()).unwrap_or(EMPTY_ROOT),
        account_proof: account_proof_nodes.into_iter().map(|n| hex::encode(n)).collect(),
        storage_proof: storage_proofs,
    }
}
```

---

## 共识层改动

### `crates/consensus/src/block.rs`

#### 1. Snapshot / Rollback

当前实现：
```rust
let evm_snapshot = state.evm_state.clone();
// ... 执行 ...
*state.evm_state = evm_snapshot;
```

新实现**保持不变**。`EvmState::clone()` 只 clone `cache` + `deleted` + `last_root`（O(m)），不 clone DB 内容。snapshot/rollback 仍是轻量操作。

#### 2. Block finalize：提交状态

```rust
fn finalize_block(state: &mut BlockExecutionState, db: &DatabaseEnv) {
    // 1. 计算最终 state_root（增量 trie 更新）
    let new_root = state.evm_state.compute_state_root();

    // 2. 将 cache 中的变更写入 DB（plain state + hashed state）
    state.evm_state.commit_to_db(db).expect("db commit failed");

    // 3. 更新 last_root / last_height
    state.evm_state.last_root = new_root;
    state.evm_state.last_height += 1;

    // 4. 清空本 block 的 cache
    state.evm_state.cache.clear();
    state.evm_state.deleted.clear();

    // 5. 设置 block header 的 evm_state_root
    result.evm_state_root = new_root;
}
```

#### 3. Propose / Verify 时的 state_root 校验

Propose 和 Verify 都需要 `compute_state_root()`。由于 trie 节点已持久化到 DB，增量计算复杂度是 O(m * log n)，m = 本 block 修改数，而非 O(n)。

---

## RPC 层改动

### `crates/rpc/src/standard.rs`

`eth_getProof` 从 stub 改为真实实现：

```rust
.register_async_method("eth_getProof", |params, state, _ctx| async move {
    let (address, storage_keys, block_tag) = parse_get_proof_params(params)?;

    // blockTag 处理：只支持 "latest" 或具体 height（当前 height）
    // 历史状态 proof 需要 archive trie，不在本方案范围内
    let evm = state.evm_state.read().map_err(|_| internal_error("lock poisoned"))?;

    let proof = evm.get_proof(address, storage_keys)
        .map_err(|e| internal_error(format!("proof generation failed: {e}")))?;

    Ok::<_, ErrorObjectOwned>(serde_json::to_value(proof).unwrap())
})
```

### 性能影响

- `eth_getProof` 首次请求：需要构建 overlay + witness 收集，~10-50ms（取决于账户 storage 大小）
- 后续相同地址请求：可从 overlay 缓存复用（可选优化）
- `eth_getBalance` / `eth_getCode` / `eth_getStorageAt`：增加一层 cache 查询，不影响主路径性能

---

## Genesis 初始化

`crates/node/src/lib.rs` 的 genesis 流程需要调整：

1. 部署系统 ERC-20 合约（CALL token）→ 产生初始 EVM 状态
2. **新增**：将初始 EVM 账户写入 `CallHashedAccounts` 和 `CallHashedStorage`
3. **新增**：构建初始 account trie 和 storage trie，写入 `CallAccountTrie` 和 `CallStorageTrie`
4. 计算初始 `evm_state_root`，写入 genesis block header

```rust
fn initialize_genesis_evm_trie(db: &DatabaseEnv, initial_state: &EvmState) -> B256 {
    // 全量构建初始 trie（一次性操作）
    let mut account_trie = Trie::default();
    for (addr, acc) in initial_state.accounts.iter() {
        let addr_hash = keccak256(*addr);
        let storage_trie = build_storage_trie(&acc.storage);
        let rlp = rlp_encode_account(acc, storage_trie.root());
        account_trie.insert(Nibbles::unpack(addr_hash), rlp);
    }

    // 持久化所有 trie 节点到 DB
    let root = account_trie.root();
    persist_trie_nodes(db, &account_trie);
    root
}
```

---

## Migration 策略

### 现有节点升级

1. **DB version bump**：在 `CallMetadata` 或新增 `CallDbVersion` 表中记录 schema version
2. **启动检测**：节点启动时检查 version，若低于新 schema，触发 migration
3. **Migration 流程**：
   ```
   a. 从 CallEvmAccounts + CallEvmStorage 读取所有数据
   b. 对每个账户：keccak256(address) -> 构建 hashed account RLP
   c. 对每个 storage slot：keccak256(slot) -> 构建 hashed storage RLP
   d. 全量构建 account trie 和每个账户的 storage trie
   e. 写入新的 4 个 trie 表
   f. 删除旧的 CallEvmAccounts / CallEvmStorage 数据（或保留到下次 compaction）
   g. 更新 DB version
   ```

4. **预计耗时**：取决于 EVM 状态大小。10k 账户 -> 秒级；100k 账户 -> 分钟级。建议在停机维护窗口执行。

### 新节点

直接走 genesis 初始化流程，无需 migration。

---

## 性能分析

### 时间复杂度

| 操作 | 当前 (HashBuilder) | 方案 B (增量 trie) |
|------|-------------------|-------------------|
| 单次账户读 | O(1) HashMap | O(1) cache，miss 时 O(1) DB |
| 单次账户写 | O(1) HashMap | O(1) cache |
| `compute_state_root` (per block) | O(n) 全量 | O(m * log n) 增量 |
| `eth_getProof` | O(1) stub | O(k * log n) 按需 |
| Snapshot (clone) | O(n) | O(m) |
| Block finalize (commit) | O(1) 无持久化 | O(m * log n) DB 写入 |

n = 总账户数，m = 本 block 修改数，k = proof 请求涉及的 slot 数

### 空间开销

- Trie 节点数量 ≈ 2 * 账户数（Patricia Trie 经验值）
- 每个节点 32-100 bytes RLP
- 10万账户 -> ~20MB trie 节点 + ~10MB hashed state
- 与 Reth 的 archive node 相比，我们**不保存历史 trie**，空间小得多

---

## 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| reth-trie API 不兼容 | 编译失败或运行时 panic | 先在一个分支上 POC，确认 `StateRoot::incremental_root` 或等效 API 存在 |
| DB migration 失败 | 节点无法启动 | migration 前自动备份 DB；失败时回滚到旧 schema |
| 增量 trie 计算结果不一致 | 共识分裂（state_root mismatch） | 在 testnet 上跑 1w+ block 对比旧 `HashBuilder` 的 root |
| `eth_getProof` 性能过差 | RPC 超时 | 设置 proof 缓存（LRU，按 address）；限制 storage_keys 数量（<= 100） |
| Snapshot 内存膨胀 | OOM | cache 不设上限，但 block 内修改数通常 < 1k；监控内存使用 |
| reth-trie 依赖版本漂移 | 与 reth-db 版本不匹配 | 锁定同一 git rev；定期同步升级 |

---

## 回滚方案

如果在 testnet 上发现严重问题：

1. 保留 `CallEvmAccounts` / `CallEvmStorage` 的读写代码（标记 `#[cfg(feature = "legacy-evm-state")]`）
2. 回滚时切换 feature flag，恢复旧的 `HashBuilder` 路径
3. 清理 trie 表，重新从 plain state 启动

---

## 实现优先级

| 阶段 | 内容 | 预计工作量 |
|------|------|----------|
| P0 | POC：验证 reth-trie API 可用性，写一个最小 demo | 1-2 天 |
| P1 | 存储层：新增 4 个 trie 表，DB CRUD 辅助函数 | 1 天 |
| P2 | EvmState 重构：cache + DB fallback，读/写/clone 路径 | 2-3 天 |
| P3 | `compute_state_root` 增量计算 + 全量 fallback | 2-3 天 |
| P4 | `get_proof` 实现 + `eth_getProof` RPC 接入 | 1-2 天 |
| P5 | Genesis 初始化 + Migration 脚本 | 1-2 天 |
| P6 | 共识层 snapshot/rollback 验证 + e2e 测试 | 2-3 天 |
| P7 | Performance benchmark + 调优 | 1-2 天 |

**总计**：~2-3 周（1 名 Rust 工程师全职）

---

## 相关文件索引

| 文件 | 当前角色 | 改动内容 |
|------|---------|---------|
| `crates/storage/src/reth_db.rs` | DB 表定义 | 新增 4 个 trie 表 |
| `crates/evm/src/state.rs` | EVM 状态管理 | 重写为 DB-backed + cache overlay |
| `crates/evm/Cargo.toml` | EVM crate 依赖 | 添加 `reth-trie` 等 |
| `crates/consensus/src/block.rs` | Block 执行 | finalize 时 commit trie；snapshot 不变 |
| `crates/rpc/src/standard.rs` | RPC 注册 | `eth_getProof` 接入 |
| `crates/node/src/lib.rs` | 节点启动 | Genesis trie 初始化；migration 检测 |
| `docs/eth_rpc.md` | RPC 审计文档 | 更新 `eth_getProof` 状态为 "✅ 已实现" |
