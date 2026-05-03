# Precompile 重构路线图

## 背景

目前所有 precompile（Asset、Oracle、Bridge、Governance、Validator 等）都挤在 `crates/precompiles/src/` 一个 crate 里，且每个 precompile 都存在**两条并行但代码不同的**读写路径：

| 路径 | 文件位置 | 存储访问方式 | 使用方 |
|------|----------|-------------|--------|
| **Journal 路径** | `precompiles/src/*.rs` | `StorageCtx::sload/sstore`（TLS 绑定的 revm journal） | EVM 交易执行（precompile 调用） |
| **EvmState 路径** | `protocol/src/evm_instructions.rs` | `evm_state.get_storage/set_storage` | 出块器、共识、RPC、测试、创世 |

这意味着**同一个 `transfer` 逻辑**被写了两遍、验证两遍，容易漂移。例如 `load_bal` + `save_bal`（precompile 内）和 `read_balance` + `seed_balance`（protocol 内）读写的是同一个 slot，但 API 不同。

## 目标架构

将每个 precompile 拆分为**独立的领域 crate**，`call-precompiles` 降级为**纯共享基础设施库**。

```
┌─────────────────────────────────────────────────────────────────────┐
│  领域层：每个 precompile 一个独立 crate                               │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌───────────┐  │
│  │ call-asset  │  │call-bridge  │  │call-oracle  │  │call-gov   │  │
│  │   crate     │  │   crate     │  │   crate     │  │  crate    │  │
│  │             │  │             │  │             │  │           │  │
│  │ ┌─────────┐ │  │ ┌─────────┐ │  │ ┌─────────┐ │  │ ┌───────┐ │  │
│  │ │precompile│ │  │ │precompile│ │  │ │precompile│ │  ││precomp│ │  │
│  │ │ 入口    │ │  │ │ 入口    │ │  │ │ 入口    │ │  ││ 入口  │ │  │
│  │ │(selector│ │  │ │(selector│ │  │ │(selector│ │  ││(select│ │  │
│  │ │ 分发)   │ │  │ │ 分发)   │ │  │ │ 分发)   │ │  ││ 分发) │ │  │
│  │ └────┬────┘ │  │ └────┬────┘ │  │ └────┬────┘ │  │ └───┬───┘ │  │
│  │      │      │  │      │      │  │      │      │  │     │     │  │
│  │ ┌────▼────┐ │  │ ┌────▼────┐ │  │ ┌────▼────┐ │  │ ┌───▼───┐ │  │
│  │ │XxxStorage│ │  │ │XxxStorage│ │  │ │XxxStorage│ │  ││XxxStor│ │  │
│  │ │ 业务逻辑│ │  │ │ 业务逻辑│ │  │ │ 业务逻辑│ │  ││ 业务  │ │  │
│  │ │(B:trait)│ │  │ │(B:trait)│ │  │ │(B:trait)│ │  ││(B:tra)│ │  │
│  │ └────┬────┘ │  │ └────┬────┘ │  │ └────┬────┘ │  │ └───┬───┘ │  │
│  └──────┼──────┘  └──────┼──────┘  └──────┼──────┘  └─────┼─────┘  │
│         │                │                │                │        │
├─────────┼────────────────┼────────────────┼────────────────┼────────┤
│         │                │                │                │        │
│  ┌──────▼────────────────▼────────────────▼────────────────▼──────┐ │
│  │              call-precompiles（共享基础设施）                    │ │
│  │  ┌───────────────┐  ┌───────────────┐  ┌─────────────────────┐│ │
│  │  │ StorageCtx    │  │ helpers.rs    │  │ JournalBackend      ││ │
│  │  │ (TLS journal) │  │ (ABI 编解码)  │  │ (StorageCtx 包装)   ││ │
│  │  └───────────────┘  └───────────────┘  └─────────────────────┘│ │
│  │  ┌────────────────────────────────────────────────────────────┐│ │
│  │  │ StatefulPrecompile trait                                    ││ │
│  │  └────────────────────────────────────────────────────────────┘│ │
│  └────────────────────────────────────────────────────────────────┘ │
│         │                                                           │
│  ┌──────▼──────────────────────────────────────────────────────┐    │
│  │              call-protocol                                   │    │
│  │  ┌───────────────────────────────────────────────────────┐   │    │
│  │  │ StorageBackend trait                                   │   │    │
│  │  │  ├─ load(address, slot) -> U256                       │   │    │
│  │  │  └─ store(address, slot, value)                       │   │    │
│  │  └───────────────────────────────────────────────────────┘   │    │
│  └──────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────┘
                          │
                          ▼
              EVM 存储槽位（单一布局）
```

### 依赖关系

```
call-precompiles ──▶ call-protocol（StorageBackend trait）
                        │
call-asset ─────────────┼──▶ call-precompiles（JournalBackend + helpers）
                        │
call-consensus ─────────┼──▶ call-asset（AssetManager + EvmStateBackend）
call-rpc ───────────────┤
call-node ──────────────┤
call-chainspec ─────────┘
```

**无循环依赖**。`call-protocol` 不依赖任何领域 precompile crate；所有上层调用方（consensus、rpc、node）按需依赖对应的领域 crate。

---

## 第一阶段：Asset Precompile（试点）

### 步骤 1.1 — 新建 `crates/asset/`

创建独立 crate `call-asset`，包含 Asset 领域的全部代码：

```
crates/asset/
├── Cargo.toml
└── src/
    ├── lib.rs              # AssetManager<B> 业务逻辑 + 错误类型
    ├── precompile.rs       # AssetPrecompile（selector 分发，薄层）
    └── backend.rs          # EvmStateBackend（&mut EvmState 包装）
```

**Cargo.toml：**

```toml
[package]
name = "call-asset"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[lints]
workspace = true

[dependencies]
call-primitives.workspace = true
call-protocol.workspace = true        # StorageBackend trait
call-precompiles.workspace = true     # JournalBackend + helpers
call-evm.workspace = true             # EvmState
alloy-primitives.workspace = true
revm-precompile.workspace = true
```

### 步骤 1.2 — `call-precompiles` 瘦身为基础设施

`crates/precompiles/src/` 只保留共享代码：

**保留：**
- `lib.rs` — `StatefulPrecompile` trait、地址常量、槽位辅助函数 `slot_balance`、`slot_asset_meta`、`slot_allowance`
- `storage.rs` — `StorageCtx` TLS journal
- `helpers/` — ABI 编解码（`decode_u64`、`decode_address`、`encode_u128`、`u128_to_u256` 等）
- `journal_backend.rs` — `JournalBackend`（`StorageBackend` 的 journal 实现）

**移除：**
- `asset.rs` — 全部移到 `call-asset`
- `bridge.rs` — 后续移到 `call-bridge`
- `oracle.rs` — 后续移到 `call-oracle`
- `governance.rs` — 后续移到 `call-governance`
- `validator.rs` — 后续移到 `call-validator`
- `agent.rs` — 后续移到 `call-agent`
- `shielded.rs` — 后续移到 `call-shielded`
- `compliance.rs` — 后续移到 `call-compliance`
- `switch.rs` — 视情况保留或移动

### 步骤 1.3 — `StorageBackend` trait 放在 `call-protocol`

**文件：** `crates/protocol/src/storage_backend.rs`

```rust
use call_primitives::{Address, U256};

/// 可插拔的 EVM 存储槽位读写后端。
///
/// 两种实现：
/// - `EvmStateBackend` — 在 call-asset/call-bridge 等领域 crate 中实现，供共识/RPC/测试使用
/// - `JournalBackend` — 在 call-precompiles 中实现，供 precompile 执行期间使用
pub trait StorageBackend {
    fn load(&self, address: Address, slot: U256) -> U256;
    fn store(&mut self, address: Address, slot: U256, value: U256);
}
```

**注意：** `EvmStateBackend` 不在 protocol 里实现，而在各**领域 crate** 里实现（因为需要 `call-evm`）。protocol 只放纯 trait。

### 步骤 1.4 — `call-asset` 内部结构

**`src/backend.rs`：**

```rust
use call_evm::EvmState;
use call_protocol::storage_backend::StorageBackend;
use call_primitives::{Address, U256};

/// 基于可变 `EvmState` 引用的 StorageBackend。
pub struct EvmStateBackend<'a>(pub &'a mut EvmState);

impl<'a> StorageBackend for EvmStateBackend<'a> {
    fn load(&self, address: Address, slot: U256) -> U256 {
        self.0.get_storage(&address, slot)
    }
    fn store(&mut self, address: Address, slot: U256, value: U256) {
        self.0.set_storage(address, slot, value);
    }
}
```

**`src/lib.rs`（AssetManager 业务逻辑）：**

```rust
use call_precompiles::{slot_allowance, slot_asset_meta, slot_balance, ...};
use call_protocol::storage_backend::StorageBackend;
use call_primitives::{Address, U256};

pub struct AssetManager<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> AssetManager<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn transfer(
        &mut self,
        asset_id: u64,
        from: Address,
        to: Address,
        amount: u128,
    ) -> Result<(), AssetError> {
        self.check_compliance(asset_id, &from)?;
        self.check_compliance(asset_id, &to)?;

        let from_bal = self.read_balance(asset_id, from);
        let to_bal = self.read_balance(asset_id, to);

        let new_from = from_bal.checked_sub(amount)
            .ok_or(AssetError::InsufficientBalance)?;
        let new_to = to_bal.checked_add(amount)
            .ok_or(AssetError::BalanceOverflow)?;

        self.write_balance(asset_id, from, new_from);
        self.write_balance(asset_id, to, new_to);
        Ok(())
    }

    pub fn mint(
        &mut self,
        asset_id: u64,
        caller: Address,
        to: Address,
        amount: u128,
    ) -> Result<(), AssetError> {
        let issuer = self.read_issuer(asset_id);
        if issuer != caller {
            return Err(AssetError::NotIssuer);
        }
        let supply = self.read_supply(asset_id);
        let max = self.read_max_supply(asset_id);
        let new_supply = supply.checked_add(amount)
            .ok_or(AssetError::SupplyOverflow)?;
        if max > 0 && new_supply > max {
            return Err(AssetError::MaxSupplyExceeded);
        }
        self.write_supply(asset_id, new_supply);
        self.add_balance(asset_id, to, amount)?;
        Ok(())
    }

    // ... burn, approve, transfer_from, register, read_balance, read_meta, etc.
}
```

**`src/precompile.rs`（手写 selector match 版本，后续由统一分发框架替代）：**

```rust
use call_precompiles::{JournalBackend, ...};
use call_primitives::Address;
use revm_precompile::{PrecompileError, PrecompileResult, PrecompileOutput};

pub struct AssetPrecompile;

impl StatefulPrecompile for AssetPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        let selector = &calldata[..4];
        match selector {
            TRANSFER_SELECTOR => {
                let (asset_id, to, amount) = decode_asset_addr_amount(calldata)?;
                let from = require_caller(msg_sender)?;
                let mut store = AssetManager::new(JournalBackend);
                store.transfer(asset_id, from, to, amount)
                    .map_err(|e| PrecompileError::Other(e.to_string()))?;
                ok_empty()
            }
            MINT_SELECTOR => { ... }
            BURN_SELECTOR => { ... }
            APPROVE_SELECTOR => { ... }
            TRANSFER_FROM_SELECTOR => { ... }
            REGISTER_SELECTOR => { ... }
            GET_BALANCE_SELECTOR => { ... }
            GET_ASSET_INFO_SELECTOR => { ... }
            BATCH_TRANSFER_SELECTOR => { ... }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}
```

---

### 步骤 1.5 — 统一分发框架（Unified Dispatch）

**参考 docs/precompile.md §10.3**，将手写 `match selector` 替换为基于 `alloy_sol_types::sol!` 的统一分发框架。

**目标：**
- 消除手写 selector 常量和硬编码 ABI 解码
- `view` 和 `mutate` 两种调用模式自动处理 gas、编解码、权限

**`crates/asset/src/precompile.rs`（改造后）：**

```rust
use call_precompiles::dispatch::{dispatch_call, view, mutate};
use alloy_sol_types::sol;

// 由 sol! 宏或手写 Call enum 生成
sol! {
    #[derive(Debug)]
    interface IProtocolAsset {
        function getBalance(uint64 assetId, address account) external view returns (uint128);
        function getAssetInfo(uint64 assetId) external view returns (string, string, uint8, address, uint128, uint8);
        function transfer(uint64 assetId, address to, uint128 amount) external returns (bool);
        function batchTransfer(uint64 assetId, address[] to, uint128[] amounts) external returns (bool);
        function approve(uint64 assetId, address spender, uint128 amount) external returns (bool);
        function transferFrom(uint64 assetId, address from, address to, uint128 amount) external returns (bool);
        function register(string symbol, string name, uint8 decimals, uint128 maxSupply) external returns (uint64);
        function mint(uint64 assetId, address to, uint128 amount) external returns (bool);
        function burn(uint64 assetId, address from, uint128 amount) external returns (bool);
    }
}

pub struct AssetPrecompile {
    storage: AssetManager<JournalBackend>,
}

impl StatefulPrecompile for AssetPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch_call(calldata, IProtocolAssetCall::abi_decode, |call| match call {
            // view: 只读，不检查 msg_sender，自动 ABI 编码返回值
            IProtocolAssetCall::getBalance(c) => {
                view(c, |c| self.storage.get_balance(c.assetId, c.account))
            }
            IProtocolAssetCall::getAssetInfo(c) => {
                view(c, |c| self.storage.get_asset_info(c.assetId))
            }

            // mutate: 写操作，传入 msg_sender 做权限校验，自动扣 gas
            IProtocolAssetCall::transfer(c) => {
                mutate(c, msg_sender, |sender, c| {
                    self.storage.transfer(c.assetId, sender, c.to, c.amount)
                })
            }
            IProtocolAssetCall::batchTransfer(c) => {
                mutate(c, msg_sender, |sender, c| {
                    self.storage.batch_transfer(c.assetId, sender, &c.to, &c.amounts)
                })
            }
            IProtocolAssetCall::approve(c) => {
                mutate(c, msg_sender, |sender, c| {
                    self.storage.approve(c.assetId, sender, c.spender, c.amount)
                })
            }
            IProtocolAssetCall::transferFrom(c) => {
                mutate(c, msg_sender, |sender, c| {
                    self.storage.transfer_from(c.assetId, sender, c.from, c.to, c.amount)
                })
            }
            IProtocolAssetCall::register(c) => {
                mutate(c, msg_sender, |sender, c| {
                    self.storage.register(&c.symbol, &c.name, c.decimals, c.maxSupply, sender)
                })
            }
            IProtocolAssetCall::mint(c) => {
                mutate(c, msg_sender, |sender, c| {
                    self.storage.mint(c.assetId, sender, c.to, c.amount)
                })
            }
            IProtocolAssetCall::burn(c) => {
                mutate(c, msg_sender, |sender, c| {
                    self.storage.burn(c.assetId, sender, c.from, c.amount)
                })
            }
        })
    }
}
```

**`crates/precompiles/src/dispatch.rs`（共享分发框架）：**

```rust
use revm_precompile::{PrecompileError, PrecompileOutput, PrecompileResult};

/// 自动扣除 input gas（每字节固定成本），解码 calldata，路由到对应 handler。
pub fn dispatch_call<T, F>(
    calldata: &[u8],
    decode: impl FnOnce(&[u8]) -> Result<T, PrecompileError>,
    handler: F,
) -> PrecompileResult
where
    F: FnOnce(T) -> PrecompileResult,
{
    // 1. 扣除 input gas
    let input_gas = INPUT_GAS_PER_BYTE * calldata.len() as u64;
    crate::storage::StorageCtx::deduct_gas(input_gas)
        .ok_or(PrecompileError::OutOfGas)?;

    // 2. 解码
    let call = decode(calldata)
        .map_err(|e| PrecompileError::Other(format!("decode error: {:?}", e)))?;

    // 3. 路由
    handler(call)
}

/// 只读调用：自动 ABI 编码返回值。
pub fn view<T, R>(call: T, handler: impl FnOnce(&T) -> R) -> PrecompileResult
where
    R: alloy_sol_types::SolValue,
{
    let result = handler(&call);
    let encoded = result.abi_encode();
    Ok(PrecompileOutput::new(0, encoded.into()))
}

/// 写调用：传入 msg_sender，自动 ABI 编码返回值或错误。
pub fn mutate<T, E>(
    call: T,
    msg_sender: Address,
    handler: impl FnOnce(Address, &T) -> Result<(), E>,
) -> PrecompileResult
where
    E: ToString,
{
    handler(msg_sender, &call)
        .map_err(|e| PrecompileError::Other(e.to_string()))?;
    ok_empty()
}
```

**改造后效果：**
- `precompile.rs` 从 ~300 行降到 ~80 行
- 无手写 selector 常量、无硬编码偏移量
- 新增函数只需在 `sol!` 接口和 `match` arm 各加一行

---

### 步骤 1.6 — 内置 Gas 计量（Built-in Gas Metering）

**参考 docs/precompile.md §10.4**，将 `JournalBackend` 升级为自动追踪 warm/cold sload、sstore 动态 gas，precompile 业务逻辑不再手动扣 gas。

**当前问题：**
每个 precompile 函数内部硬编码 gas：
```rust
fn transfer(&self, ...) -> PrecompileResult {
    const GAS_COST: u64 = 5000;
    StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
    // ... 业务逻辑
}
```

**问题：**
- 读操作和写操作 gas 相同，不区分 warm/cold access
- sstore 从 0→value 和 value→value 的 gas 差异未体现
- 新增函数容易遗漏 gas 扣除

**改造后：**

```rust
// crates/precompiles/src/journal_backend.rs
impl StorageBackend for JournalBackend {
    fn load(&self, address: Address, slot: U256) -> U256 {
        // 自动追踪 warm/cold，扣除对应 gas
        let gas = self.access_tracker.load_gas(address, slot);
        crate::storage::StorageCtx::deduct_gas(gas)
            .expect("gas deducted at storage layer");
        crate::storage::StorageCtx::sload(address, slot).unwrap_or(U256::ZERO)
    }

    fn store(&mut self, address: Address, slot: U256, value: U256) {
        let gas = self.access_tracker.store_gas(address, slot, value);
        crate::storage::StorageCtx::deduct_gas(gas)
            .expect("gas deducted at storage layer");
        crate::storage::StorageCtx::sstore(address, slot, value);
    }
}
```

`dispatch_call` 只扣除 input gas（calldata 长度），所有 storage access gas 由 `JournalBackend` 自动在读写时扣除。

**统一 gas 规则：**

| 操作 | Gas 规则 |
|------|---------|
| `sload` warm | `WARM_STORAGE_READ_COST = 100` |
| `sload` cold | `COLD_SLOAD_COST = 2100` |
| `sstore` 静态 | `SSTORE_STATIC = 5000` |
| `sstore` 动态 (0→非0) | + `SSTORE_INIT = 20000` |
| `sstore` 动态 (非0→0) | 返还 `SSTORE_REFUND = 15000` |
| `sstore` 同值 | `WARM_ACCESS = 100` |

**业务层彻底无 gas：**

```rust
// AssetManager::transfer 里没有任何 gas 相关代码
pub fn transfer(&mut self, asset_id: u64, from: Address, to: Address, amount: u128) -> Result<(), AssetError> {
    // 只有业务逻辑，gas 由 backend 自动处理
    let from_bal = self.read_balance(asset_id, from);
    // ...
    self.write_balance(asset_id, from, new_from); // 自动扣 sstore gas
    Ok(())
}
```

---

### 步骤 1.7 — 更新调用方

原 `protocol/src/evm_instructions.rs` 中所有 asset 相关辅助函数（`seed_balance`、`add_balance_evm`、`deduct_balance_evm`、`seed_allowance`、`read_allowance`、`add_asset_supply_evm`、`seed_asset`、`read_balance`、`read_asset_*` 等）全部删除。所有调用方改用 `call-asset`：

| 文件 | 原用法 | 新用法 |
|------|--------|--------|
| `crates/consensus/src/exec/state_accessors.rs` | `seed_balance`、`seed_asset` | `AssetManager::new(EvmStateBackend).write_balance(...)` |
| `crates/consensus/src/block.rs` | `add_balance_evm` | `AssetManager::new(EvmStateBackend).add_balance(...)` |
| `crates/rpc/src/handlers/state.rs` | `read_balance`、`read_asset_*` | `AssetManager::new(EvmStateBackend).read_balance(...)` |
| `crates/node/src/lib.rs`（创世） | `seed_balance`、`seed_asset` | `AssetManager::new(EvmStateBackend)` |
| `crates/node/src/tests.rs` | `seed_balance` | `AssetManager::new(EvmStateBackend)` |
| `crates/node/tests/e2e/*.rs` | `seed_balance`、`seed_asset` | `AssetManager::new(EvmStateBackend)` |
| `crates/chainspec/src/genesis.rs` | `seed_asset` | `AssetManager::new(EvmStateBackend)` |

**示例：**

```rust
// 改造前（protocol evm_instructions）
use call_protocol::evm_instructions::{seed_balance, seed_asset};
seed_balance(&mut evm, 1, addr, 1000);
seed_asset(&mut evm, 1, "TEST", "Test Token", 18, issuer, 0, 8000, 0);

// 改造后（call-asset）
use call_asset::{AssetManager, EvmStateBackend};
let mut store = AssetManager::new(EvmStateBackend(&mut evm));
store.write_balance(1, addr, 1000);
store.register(1, "TEST", "Test Token", 18, issuer, 0, 8000, 0);
```

---

## 第二至 N 阶段：其他 Precompile（同模式）

每个领域按相同模式拆分为独立 crate：

| 阶段 | Crate | 复杂度 | 说明 |
|------|-------|--------|------|
| 2 | `crates/validator/` | 中 | 质押/解绑逻辑在 precompile 和共识之间共享 |
| 3 | `crates/bridge/` | 中 | Pending deposit 列表压缩逻辑存在重复 |
| 4 | `crates/oracle/` | 中 | 价格更新、TWAP、奖励池 |
| 5 | `crates/governance/` | 高 | 事件发射缺口 — 见下文 |
| 6 | `crates/agent/` | 低 | Agent 元数据读写 |
| 7 | `crates/shielded/` | 低 | Merkle root、commitments、nullifiers |
| 8 | `crates/compliance/` | 低 | 简单的状态读写 |

### 每个新 crate 的目录结构

```
crates/<domain>/
├── Cargo.toml              # 依赖 call-precompiles, call-protocol, call-primitives, call-evm
└── src/
    ├── lib.rs              # XxxStorage<B> 业务逻辑 + 错误类型
    ├── precompile.rs       # XxxPrecompile（selector 分发，薄层）
    └── backend.rs          # EvmStateBackend（可选：如果 logic 简单可内联到 lib.rs）
```

### 共享基础设施的演进

随着更多 precompile 拆分，以下公共代码可以逐步从 `call-precompiles` 下沉到更基础的 crate（如 `call-primitives`）：

- `slot_*` 辅助函数 — 如果 `call-primitives` 提供 `storage_slot` 哈希函数，各领域 crate 可自行定义 slot 常量
- ABI 编解码工具 — 如果多个 precompile 共用，保留在 `call-precompiles`；如果只有一两个用，可内联到各自 crate

**最终状态：** `call-precompiles` 只剩：
- `StorageCtx`（TLS journal）
- `JournalBackend`
- `StatefulPrecompile` trait

### Governance 特例

`GovernanceManager`（当前在 `crates/governance/src/manager.rs`）维护内存事件队列（`events: Vec<GovernanceEvent>`），每出块时 drained 给 WebSocket 广播。

**解决方案：** `call-governance` crate 的 `GovernanceStorage::advance_proposals()` 从 EVM storage 读取提案状态，计算状态转换，并**临时生成事件**（不持久化）。事件只在 `advance()` 执行期间存在并立即消费。

```rust
// crates/governance/src/lib.rs
impl<B: StorageBackend> GovernanceStorage<B> {
    pub fn advance_proposals(&mut self, current_block: u64) -> Vec<GovernanceEvent> {
        let mut events = Vec::new();
        let count = self.read_proposal_count();
        for id in 1..=count {
            let old_status = self.read_status(id);
            let new_status = self.compute_new_status(id, current_block);
            if old_status != new_status {
                self.write_status(id, new_status);
                events.push(GovernanceEvent::StatusChanged { proposal_id: id, new_status });
            }
        }
        events
    }
}
```

这样 `crates/governance/` 这个 crate 可以保留（作为 governance 领域的业务逻辑 crate），但其内部状态机不再维护独立内存，而是从 EVM storage 读取。

---

## 完整依赖图（最终态）

```
                         ┌─────────────────┐
                         │  call-primitives│
                         └────────┬────────┘
                                  │
            ┌─────────────────────┼─────────────────────┐
            │                     │                     │
            ▼                     ▼                     ▼
   ┌─────────────────┐   ┌─────────────────┐   ┌─────────────────┐
   │  call-protocol   │   │  call-evm       │   │  call-precompiles│
   │  (StorageBackend │   │  (EvmState)     │   │  (StorageCtx,   │
   │   trait)         │   │                 │   │   JournalBackend,│
   └────────┬─────────┘   └────────┬────────┘   │   helpers)       │
            │                      │            └─────────────────┘
            │                      │                     ▲
            │                      │                     │
            ▼                      ▼                     │
   ┌──────────────────────────────────────────────────────┐
   │              领域 precompile crates                   │
   │  ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ...    │
   │  │call-asset│ │call-bridge│ │call-oracle│ │call-gov│     │
   │  │ crate  │ │ crate  │ │ crate  │ │ crate  │        │
   │  │ (precompile + Storage + EvmStateBackend)          │
   │  └────────┘ └────────┘ └────────┘ └────────┘         │
   └──────────────────────────────────────────────────────┘
            │                      │
            ▼                      ▼
   ┌─────────────────┐   ┌─────────────────┐
   │  call-consensus  │   │  call-rpc       │
   │  call-node       │   │  call-chainspec │
   └─────────────────┘   └─────────────────┘
```

---

## 验证清单（每阶段）

- [ ] `cargo test -p call-precompiles` 通过
- [ ] `cargo test -p call-asset` 通过
- [ ] `cargo test -p call-protocol` 通过
- [ ] `cargo test -p call-consensus` 通过
- [ ] `cargo test -p call-rpc` 通过
- [ ] `cargo test -p call-node --lib` 通过
- [ ] `cargo test -p call-node --tests` 通过
- [ ] `cargo build` 通过（无未使用 import 警告）
- [ ] 各领域 crate 的 precompile.rs 行数 < 120
- [ ] `protocol/src/evm_instructions.rs` 中对应领域代码已删除
- [ ] 无重复槽位辅助函数

---

## 附录：Asset 函数映射

| 函数 | 原 precompile 位置 | 原 protocol 位置 | 新位置 |
|------|-------------------|-----------------|--------|
| `getBalance` | `AssetPrecompile::get_balance` | `read_balance` | `call-asset::AssetManager::read_balance` |
| `getAssetInfo` | `AssetPrecompile::get_asset_info` | `read_asset_symbol/name/...` | `call-asset::AssetManager::read_meta` |
| `transfer` | `AssetPrecompile::transfer` | `deduct_balance_evm` + `add_balance_evm` | `call-asset::AssetManager::transfer` |
| `batchTransfer` | `AssetPrecompile::batch_transfer` | （内联） | `call-asset::AssetManager::batch_transfer` |
| `approve` | `AssetPrecompile::approve` | `seed_allowance` | `call-asset::AssetManager::approve` |
| `transferFrom` | `AssetPrecompile::transfer_from` | `read_allowance` | `call-asset::AssetManager::transfer_from` |
| `mint` | `AssetPrecompile::mint` | `add_asset_supply_evm` + `add_balance_evm` | `call-asset::AssetManager::mint` |
| `burn` | `AssetPrecompile::burn` | `deduct_balance_evm` + supply 更新 | `call-asset::AssetManager::burn` |
| `register` | `AssetPrecompile::register` | `seed_asset` | `call-asset::AssetManager::register` |
| `checkCompliance` | `AssetPrecompile::check_compliance` | `read_compliance_status` | `call-asset::AssetManager::check_compliance` |
| `seed_balance` | — | `seed_balance` | `call-asset::AssetManager::write_balance` |
| `seed_asset` | — | `seed_asset` | `call-asset::AssetManager::register` / `write_meta` |
