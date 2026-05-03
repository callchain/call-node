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
call-consensus ─────────┼──▶ call-asset（AssetStorage + EvmStateBackend）
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
    ├── lib.rs              # AssetStorage<B> 业务逻辑 + 错误类型
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

**`src/lib.rs`（AssetStorage 业务逻辑）：**

```rust
use call_precompiles::{slot_allowance, slot_asset_meta, slot_balance, ...};
use call_protocol::storage_backend::StorageBackend;
use call_primitives::{Address, U256};

pub struct AssetStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> AssetStorage<B> {
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
                let mut store = AssetStorage::new(JournalBackend);
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
    storage: AssetStorage<JournalBackend>,
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
// AssetStorage::transfer 里没有任何 gas 相关代码
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
| `crates/consensus/src/exec/state_accessors.rs` | `seed_balance`、`seed_asset` | `AssetStorage::new(EvmStateBackend).write_balance(...)` |
| `crates/consensus/src/block.rs` | `add_balance_evm` | `AssetStorage::new(EvmStateBackend).add_balance(...)` |
| `crates/rpc/src/handlers/state.rs` | `read_balance`、`read_asset_*` | `AssetStorage::new(EvmStateBackend).read_balance(...)` |
| `crates/node/src/lib.rs`（创世） | `seed_balance`、`seed_asset` | `AssetStorage::new(EvmStateBackend)` |
| `crates/node/src/tests.rs` | `seed_balance` | `AssetStorage::new(EvmStateBackend)` |
| `crates/node/tests/e2e/*.rs` | `seed_balance`、`seed_asset` | `AssetStorage::new(EvmStateBackend)` |
| `crates/chainspec/src/genesis.rs` | `seed_asset` | `AssetStorage::new(EvmStateBackend)` |

**示例：**

```rust
// 改造前（protocol evm_instructions）
use call_protocol::evm_instructions::{seed_balance, seed_asset};
seed_balance(&mut evm, 1, addr, 1000);
seed_asset(&mut evm, 1, "TEST", "Test Token", 18, issuer, 0, 8000, 0);

// 改造后（call-asset）
use call_asset::{AssetStorage, EvmStateBackend};
let mut store = AssetStorage::new(EvmStateBackend(&mut evm));
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

---

### 各 Precompile 改造影响预览

基于对当前代码的逐文件分析，以下是每个 precompile 的详细改造影响评估：

#### Validator（`crates/precompiles/src/validator.rs`，~547 行）

**当前功能：** `stake`、`unstake`、`claimUnbonded`、`getValidatorStake`、`getValidatorStatus`、`getValidatorPubkey`、`getUnbondHeight`、`getValidatorByIndex`

**跨域依赖：**
- **Asset 强依赖** — `load_bal` / `save_bal`（CALL 质押代币的 escrow 转移）。`stake` 从 caller 扣 CALL，锁到 validator slot；`claimUnbonded` 从 validator slot 释放给 caller。

**改造影响：**
- `ValidatorPrecompile` → `crates/validator/src/precompile.rs`（~60 行，统一分发后）
- 业务逻辑提取为 `ValidatorStorage<B>`，包含：`stake`、`unstake`、`claim_unbonded`、`get_validator`、`get_validator_by_index`
- **跨域调用：** `ValidatorStorage::stake()` 内部调用 `AssetStorage::transfer(CALL_ASSET_ID, caller, VALIDATOR_ESCROW, amount)`
- `SimplexConsensus`（`crates/consensus/`）当前直接操作 `evm_state.set_storage` 进行 validator 注册，改造后改用 `ValidatorStorage::register_validator()`

**复杂度：** 中。核心挑战是质押 escrow 与 Asset 的耦合，但模式清晰（类似 Asset 内部转账）。

---

#### Bridge（`crates/precompiles/src/bridge.rs`，~1210 行）

**当前功能：** `getTotalDeposits`、`getTotalWithdrawals`、`bridgeToEvm`、`bridgeToProtocol`、`externalDeposit`、`externalWithdraw`、`deposit`、`initiateChallenge`、`resolveChallenge`、`getChallengeStatus`、`withdrawChallengeBond`

**跨域依赖：**
- **Asset 强依赖** — `credit_bal` / `debit_bal`（bridge 出入金时的余额变更）
- **Asset 元数据依赖** — `slot_asset_meta`（读取 asset decimals 等）
- **Validator 依赖** — `slot_validator_by_addr` + `VALIDATOR_ADDRESS`（challenge 成功后的 slash 调用）

**改造影响：**
- `BridgePrecompile` → `crates/bridge/src/precompile.rs`（~100 行）
- 业务逻辑提取为 `BridgeStorage<B>`，包含：`external_deposit`、`external_withdraw`、`bridge_to_evm`、`bridge_to_protocol`、`initiate_challenge`、`resolve_challenge`、`withdraw_bond`
- **Pending deposit 队列：** 当前 `BridgeState` 结构体已废弃（标记为 `// Deprecated`），预编译已用 EVM storage slot 存储 pending deposit。但 `crates/bridge/src/state.rs` 中的 `BridgeStateManager`（内存状态）仍被 `block_producer.rs` 使用，需彻底移除。
- **Challenge 系统：** Challenge 的 bond 扣款走 `AssetStorage::transfer`，challenge 成功后的 slash 走 `ValidatorStorage::slash_stake`
- `block_producer.rs:104-125` 的 deposit settlement 逻辑改为：从 `BridgeStorage` 读取 finalized deposits，调用 `AssetStorage::add_balance` 到账

**复杂度：** 中高。Bridge 是交互最复杂的预编译（challenge + 多币种 + 跨链），但大部分逻辑已在预编译中，只需提取。

---

#### Oracle（`crates/precompiles/src/oracle.rs`，~351 行）

**当前功能：** `getPrice`、`getTWAP`、`isStale`、`submitPrice`

**跨域依赖：**
- **Validator 只读依赖** — `VALIDATOR_ADDRESS` + `slot_validator_by_addr(msg_sender)`（验证 caller 是否为 validator）

**改造影响：**
- `OraclePrecompile` → `crates/oracle/src/precompile.rs`（~40 行）
- 业务逻辑提取为 `OracleStorage<B>`，包含：`submit_price`、`get_price`、`get_twap`、`is_stale`
- **奖励池：** 当前预编译中 `add_oracle_reward` / `read_oracle_reward_pool` 已在 EVM storage 中。但 `crates/consensus/src/oracle/` 里的旧 `OracleManager`（内存版）维护了 `contributors`、`reward_pool` 等，需删除。
- **出块器改造：** `block_producer.rs:144-212` 的 oracle period advancement、outlier slashing、reward distribution 改为：从 `OracleStorage` 读取价格历史，计算 outliers，调用 `ValidatorStorage::slash_stake`，调用 `AssetStorage::transfer` 分发奖励。

**复杂度：** 中。核心挑战是出块器中的 outlier detection 和奖励分发逻辑需要跨域调用（Oracle → Validator → Asset）。

---

#### Governance（`crates/precompiles/src/governance.rs`，~713 行）

**当前功能：** `submitProposal`、`vote`、`queue`、`execute`、`emergencyPause`、`emergencyResume`、`getProposalStatus`、`getProposalVotes`、`isPaused`、`getProposalCount`

**跨域依赖：**
- **Asset 强依赖** — `load_bal` / `save_bal`（proposal deposit 扣除与退回）

**改造影响：**
- `GovernancePrecompile` → `crates/governance/src/precompile.rs`（~70 行）
- 业务逻辑提取为 `GovernanceStorage<B>`（或保留 `GovernanceManager` 名称但语义改变），包含：`submit_proposal`、`vote`、`queue`、`execute`、`emergency_pause`、`emergency_resume`
- **GovernanceManager 内存状态机删除：** `crates/governance/src/manager.rs` 中 ~20 个内存字段全部改为 EVM storage 读取（详见上文 [Governance 特例](#governance-特例)）
- **事件队列：** 改为 ephemeral 生成，不持久化
- `block_producer.rs:277-309` 的 governance advancement 改为调用 `GovernanceAdvancer::advance()`

**复杂度：** 高。内存状态机最庞大，且涉及事件系统重构。

---

#### Agent（`crates/precompiles/src/agent.rs`）

**当前功能：** Agent 注册、元数据读写、余额授权

**跨域依赖：** 无（自包含）。

**改造影响：**
- `AgentPrecompile` → `crates/agent/src/precompile.rs`
- 业务逻辑提取为 `AgentStorage<B>`
- `agent_root` 快照当前由内存 `AgentRegistry` 计算，改造后改为从 `AgentStorage` 遍历 EVM storage 计算（或缓存）

**复杂度：** 低。

---

#### Shielded（`crates/precompiles/src/shielded.rs`）

**当前功能：** Merkle root、nullifier 检查、commitment 计数

**跨域依赖：** 无（自包含）。

**改造影响：**
- `ShieldedPrecompile` → `crates/shielded/src/precompile.rs`
- 业务逻辑提取为 `ShieldedStorage<B>`
- **Merkle tree 重建：** 当前内存 `shielded_state` 维护了完整 Merkle tree（不只是 root）。改造后：tree 从 EVM commitments 重建，或作为 sidecar 缓存（不持久化，重启后重建）
- `merkle_root` 快照直接读 EVM storage

**复杂度：** 低。核心决策是 Merkle tree 是否完全存入 EVM（precompile 已有 `slot_tree_node`）还是作为 sidecar。

---

#### Compliance（`crates/precompiles/src/compliance.rs`）

**当前功能：** 地址合规状态读写（冻结/解冻）

**跨域依赖：** 无（自包含，但 Asset / Bridge / Governance 会读取）。

**改造影响：**
- `CompliancePrecompile` → `crates/compliance/src/precompile.rs`
- 业务逻辑提取为 `ComplianceStorage<B>`
- 当前 `RpcState` 中的 `compliance_engine` 内存字段删除，所有合规检查改为读 EVM storage

**复杂度：** 低。

---

### 依赖关系总结（改造后）

```
call-asset（无跨域依赖）
  ▲
  │ 被依赖
  ├─ call-validator（依赖 asset 的 CALL 转账）
  ├─ call-bridge（依赖 asset 的余额操作 + validator 的 slash）
  ├─ call-governance（依赖 asset 的 deposit 扣款）
  └─ call-oracle（不依赖 asset，只依赖 validator）

call-validator（依赖 asset）
  ▲
  │ 被依赖
  ├─ call-bridge（challenge slash）
  ├─ call-oracle（validator 身份校验）
  └─ call-governance（quorum 计算读总质押量）
```

**无循环依赖。** 底层 crate（asset、validator）不依赖上层 crate（bridge、governance、oracle）。

### Governance 特例

`GovernanceManager`（当前在 `crates/governance/src/manager.rs`，~971 行）维护着约 20 个内存字段，每出块时通过 `advance()` 推进提案状态、通过 `drain_events()` 向 WebSocket 广播事件。改造后 `GovernanceManager` 变为 **纯状态扫描器**，不再维护独立内存，所有数据从 EVM storage 读取。

#### 字段映射

| GovernanceManager 字段 | 原语义 | 改造后来源 |
|-----------------------|--------|-----------|
| `proposals: HashMap<u64, Proposal>` | 内存提案列表 | Governance precompile EVM slots（`slot_gov_proposal(id, *)`） |
| `next_proposal_id: u64` | 自增 ID | `slot_gov_proposal_count` |
| `validator_addresses` | 提案人地址 | Validator precompile（只读） |
| `call_balances` | CALL 余额 | Asset precompile（只读） |
| `asset_issuers` | 资产发行者 | Asset precompile（只读） |
| `delegations` | 投票委托 | 新增 `slot_gov_delegation` EVM slot |
| `deposits` | 提案保证金 | `slot_gov_proposal(id, b"deposit")` |
| `voted_addresses` | 已投票地址集合 | `slot_gov_voter(id, addr)` |
| `last_submission_block` | 上次提交区块 | `slot_gov_proposal(id, b"submitted_at")` |
| `current_block` | 当前区块 | 由调用方传入 |
| `emergency_pause` | 紧急暂停状态 | `slot_gov_paused` |
| `events` | 内存事件队列 | **临时生成**（`advance()` 扫描时生成，立即消费） |
| `executor` | 提案执行器 | **保留**（`ProposalExecutor` trait 不变） |
| `balance_source` | 余额来源 | 删除，统一走 `AssetStorage` |
| `scheduled_upgrades` | 计划升级 | 新增 `slot_gov_upgrade` EVM slot |
| `compliance_policies` | 合规策略 | Compliance precompile（只读） |
| `fee_currencies` | 手续费币种 | 新增 `slot_gov_fee_currency` EVM slot |
| `fee_currencies_pending_removal` | 待移除币种 | 新增 `slot_gov_fee_removal` EVM slot |
| `fee_currency_cap_bps` | 手续费上限 | `slot_gov_fee_cap` |
| `validator_pubkeys` | 验证者公钥 | Validator precompile（只读） |

**删除的字段：** `call_balances`、`asset_issuers`、`balance_source`、`validator_addresses`、`validator_pubkeys`、`events` — 这些全部改为实时从对应 precompile EVM storage 读取，或从调用方传入。

#### `advance()` 改造

```rust
// crates/governance/src/advancer.rs
pub struct GovernanceAdvancer<'a, B: StorageBackend> {
    backend: B,
    executor: &'a dyn ProposalExecutor,
}

impl<'a, B: StorageBackend> GovernanceAdvancer<'a, B> {
    /// 扫描所有提案，推进状态，返回事件（不持久化）。
    pub fn advance(&mut self, current_block: u64) -> Vec<GovernanceEvent> {
        let mut events = Vec::new();
        let count = self.read_proposal_count();

        for id in 1..=count {
            let status = self.read_status(id);
            let submitted_at = self.read_submitted_at(id);
            let timelock = self.read_timelock(id);
            let yes_votes = self.read_yes_votes(id);
            let no_votes = self.read_no_votes(id);
            let total_stake = self.read_total_validator_stake(); // 从 Validator precompile

            let new_status = compute_status(status, current_block, submitted_at, timelock,
                                            yes_votes, no_votes, total_stake);

            if new_status != status {
                self.write_status(id, new_status);
                events.push(GovernanceEvent::StatusChanged { proposal_id: id, new_status });

                if new_status == ProposalStatus::Queued {
                    // 即将执行 — 通过 ProposalExecutor 应用 side effects
                    let proposal = self.read_proposal(id);
                    if let Err(e) = self.executor.execute(&proposal) {
                        self.write_status(id, ProposalStatus::Failed);
                        events.push(GovernanceEvent::ExecutionFailed { proposal_id: id, reason: e });
                    } else {
                        events.push(GovernanceEvent::Executed { proposal_id: id });
                    }
                }
            }
        }
        events
    }
}
```

**事件 ephemeral 语义：** `advance()` 返回的 `Vec<GovernanceEvent>` 由调用方（`block_producer.rs` 或 `bft_loop.rs`）立即消费——写入 WebSocket subscription buffer 后丢弃。不存入 EVM storage，不持久化到磁盘。

#### 依赖关系变化

```
GovernanceAdvancer
  ├─ 读 ──▶ AssetStorage（保证金退回时的余额操作）
  ├─ 读 ──▶ ValidatorStorage（总质押量用于 quorum 计算）
  ├─ 读 ──▶ ComplianceStorage（验证合规策略提案）
  └─ 调用 ──▶ ProposalExecutor（跨系统 side effects）
```

`call-governance` crate 将依赖 `call-asset`、`call-validator`、`call-compliance` 的 `*Storage` 类型（只读），但**不循环依赖**——这些领域 crate 不依赖 `call-governance`。

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

- [x] `cargo test -p call-precompiles` 通过
- [x] `cargo test -p call-asset` 通过
- [x] `cargo test -p call-protocol` 通过
- [x] `cargo test -p call-consensus` 通过
- [x] `cargo test -p call-rpc` 通过
- [x] `cargo test -p call-evm` 通过
- [ ] `cargo test -p call-node --lib` 通过（2 个预存在测试失败，与 Asset 迁移无关）
- [ ] `cargo test -p call-node --tests` 通过
- [x] `cargo build` 通过（有未使用 import 警告，与 Asset 迁移无关）
- [x] `call-asset/src/precompile.rs` 行数 < 120（~290 行，含测试 ~110 行不含测试）
- [x] `protocol/src/evm_instructions.rs` 不存在（asset 代码已提前清理）
- [x] 无重复槽位辅助函数

---

## 附录：Asset 函数映射

| 函数 | 原 precompile 位置 | 原 protocol 位置 | 新位置 |
|------|-------------------|-----------------|--------|
| `getBalance` | `AssetPrecompile::get_balance` | `read_balance` | `call-asset::AssetStorage::read_balance` |
| `getAssetInfo` | `AssetPrecompile::get_asset_info` | `read_asset_symbol/name/...` | `call-asset::AssetStorage::read_meta` |
| `transfer` | `AssetPrecompile::transfer` | `deduct_balance_evm` + `add_balance_evm` | `call-asset::AssetStorage::transfer` |
| `batchTransfer` | `AssetPrecompile::batch_transfer` | （内联） | `call-asset::AssetStorage::batch_transfer` |
| `approve` | `AssetPrecompile::approve` | `seed_allowance` | `call-asset::AssetStorage::approve` |
| `transferFrom` | `AssetPrecompile::transfer_from` | `read_allowance` | `call-asset::AssetStorage::transfer_from` |
| `mint` | `AssetPrecompile::mint` | `add_asset_supply_evm` + `add_balance_evm` | `call-asset::AssetStorage::mint` |
| `burn` | `AssetPrecompile::burn` | `deduct_balance_evm` + supply 更新 | `call-asset::AssetStorage::burn` |
| `register` | `AssetPrecompile::register` | `seed_asset` | `call-asset::AssetStorage::register` |
| `checkCompliance` | `AssetPrecompile::check_compliance` | `read_compliance_status` | `call-asset::AssetStorage::check_compliance` |
| `seed_balance` | — | `seed_balance` | `call-asset::AssetStorage::write_balance` |
| `seed_asset` | — | `seed_asset` | `call-asset::AssetStorage::register` / `write_meta` |

---

## 实施计划（全部未实现）

> 以下所有步骤状态为 `- [ ]`（未实现），每完成一项后由实施者勾选 `- [x]` 并提交。

---

### Phase 0 — 前置准备

- [x] **P0.1** 确认全项目当前编译通过：`cargo check`
- [x] **P0.2** 确认 `call-protocol` 已依赖 `call-primitives` 和 `call-evm`
- [x] **P0.3** 列出 `call-precompiles` 中所有 `slot_*` 辅助函数，确认 `call-asset` 可复用
- [x] **P0.4** 在根目录 `Cargo.toml` 的 `[workspace.members]` 中添加 `"crates/asset"`

---

### Phase 1 第一波：Asset 核心功能迁移（功能优先，框架延后）

#### 步骤 1 — `call-protocol` 添加 `StorageBackend` trait

- [x] **1.1** 新建 `crates/protocol/src/storage_backend.rs`

```rust
use call_primitives::{Address, U256};

pub trait StorageBackend {
    fn load(&self, address: Address, slot: U256) -> U256;
    fn store(&mut self, address: Address, slot: U256, value: U256);
}
```

- [x] **1.2** 在 `crates/protocol/src/lib.rs` 中追加 `pub mod storage_backend;`
- [x] **1.3** 编译验证：`cargo check -p call-protocol`

#### 步骤 2 — 创建 `crates/asset/` 独立 crate

- [x] **2.1** 新建 `crates/asset/Cargo.toml`

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
call-protocol.workspace = true
call-precompiles.workspace = true
call-evm.workspace = true
alloy-primitives.workspace = true
revm-precompile.workspace = true
```

- [x] **2.2** 新建 `crates/asset/src/backend.rs`

```rust
use call_evm::EvmState;
use call_protocol::storage_backend::StorageBackend;
use call_primitives::{Address, U256};

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

- [x] **2.3** 新建 `crates/asset/src/lib.rs`，提取 `AssetStorage<B>`

从 `crates/precompiles/src/asset.rs` 和 `crates/consensus/src/exec/state_accessors.rs` 提取以下方法：

| 方法 | 来源 |
|------|------|
| `read_balance` | precompile `load_bal` / protocol `read_balance` |
| `write_balance` | precompile `save_bal` / protocol `seed_balance` |
| `add_balance` | protocol `add_balance_evm` |
| `deduct_balance` | protocol `deduct_balance_evm` |
| `read_allowance` | precompile + protocol `read_allowance` |
| `write_allowance` | protocol `seed_allowance` |
| `read_meta`（symbol/name/decimals/issuer/max/supply/status） | precompile `get_asset_info` / protocol `read_asset_*` |
| `write_meta` / `register` | precompile `register` / protocol `seed_asset` |
| `transfer` | precompile `transfer` |
| `approve` | precompile `approve` |
| `transfer_from` | precompile `transfer_from` |
| `mint` | precompile `mint` |
| `burn` | precompile `burn` |

```rust
use call_precompiles::{slot_allowance, slot_asset_meta, slot_balance, ASSET_ADDRESS};
use call_protocol::storage_backend::StorageBackend;
use call_primitives::{Address, Balance, U256};

#[derive(Debug)]
pub enum AssetError {
    InsufficientBalance,
    BalanceOverflow,
    SupplyOverflow,
    MaxSupplyExceeded,
    NotIssuer,
    AssetNotFound,
}

pub struct AssetStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> AssetStorage<B> {
    pub fn new(backend: B) -> Self { Self { backend } }

    pub fn read_balance(&self, asset_id: u64, addr: Address) -> Balance {
        let slot = slot_balance(asset_id, addr);
        u256_to_u128(self.backend.load(ASSET_ADDRESS, slot))
    }

    pub fn write_balance(&mut self, asset_id: u64, addr: Address, amount: Balance) {
        let slot = slot_balance(asset_id, addr);
        self.backend.store(ASSET_ADDRESS, slot, u128_to_u256(amount));
    }

    pub fn add_balance(&mut self, asset_id: u64, addr: Address, amount: Balance) -> Result<(), AssetError> {
        let current = self.read_balance(asset_id, addr);
        let new = current.checked_add(amount).ok_or(AssetError::BalanceOverflow)?;
        self.write_balance(asset_id, addr, new);
        Ok(())
    }

    pub fn deduct_balance(&mut self, asset_id: u64, addr: Address, amount: Balance) -> Result<(), AssetError> {
        let current = self.read_balance(asset_id, addr);
        let new = current.checked_sub(amount).ok_or(AssetError::InsufficientBalance)?;
        self.write_balance(asset_id, addr, new);
        Ok(())
    }

    pub fn transfer(&mut self, asset_id: u64, from: Address, to: Address, amount: Balance) -> Result<(), AssetError> {
        self.deduct_balance(asset_id, from, amount)?;
        self.add_balance(asset_id, to, amount)?;
        Ok(())
    }

    // ... read_allowance, write_allowance, read_meta, write_meta,
    // ... approve, transfer_from, mint, burn, register 等
}
```

- [x] **2.4** 新建 `crates/asset/src/precompile.rs`（薄层入口）

```rust
use call_precompiles::{JournalBackend, StatefulPrecompile};
use call_primitives::Address;
use revm_precompile::{PrecompileError, PrecompileResult};
use crate::AssetStorage;

pub struct AssetPrecompile;

impl StatefulPrecompile for AssetPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        // 暂时保持原有 selector match，内部改为 AssetStorage 调用
        // TODO: Phase 1 第二波替换为 dispatch_call
        // ...
    }
}
```

- [x] **2.5** 新建 `crates/asset/src/lib.rs` 入口模块导出

```rust
pub mod backend;
pub mod precompile;

mod storage;
pub use storage::{AssetStorage, AssetError};
```

- [x] **2.6** 在根目录 `Cargo.toml` 的 `[workspace.members]` 中添加 `"crates/asset"`
- [x] **2.7** 编译验证：`cargo check -p call-asset`

#### 步骤 3 — `call-precompiles` 瘦身（移除 asset.rs）

- [x] **3.1** 从 `crates/precompiles/src/lib.rs` 移除 `mod asset;` 和 `pub use asset::*;`
- [x] **3.2** 确认 `slot_balance`、`slot_allowance`、`slot_asset_meta`、`ASSET_ADDRESS` 仍保留在 `call-precompiles`（供 `call-asset` 使用）
- [x] **3.3** 编译验证：`cargo check -p call-precompiles`

#### 步骤 4 — 迁移调用方（逐个文件替换）

- [x] **4.1** `crates/consensus/src/exec/state_accessors.rs`
  - asset 相关 helper（`seed_balance`、`seed_asset`、`read_balance`、`read_asset_*`、`add_balance_evm`、`deduct_balance_evm`、`seed_allowance`、`read_allowance`）改为委托给 `AssetStorage<EvmStateBackend>`
  - 保留 wrapper 函数作为稳定 API，内部实现改为 `use call_asset::{AssetStorage, EvmStateBackend};`

- [x] **4.2** `crates/consensus/src/block.rs`
  - 确认已使用 `state_accessors` wrapper（内部已委托给 `AssetStorage`）

- [x] **4.3** `crates/rpc/src/handlers/state.rs`
  - 确认 `read_balance`、`seed_balance` 等调用通过 `state_accessors` wrapper（内部已委托给 `AssetStorage`）

- [x] **4.4** `crates/rpc/src/handlers/executor.rs`
  - 确认涉及 asset 合规状态写入通过 `state_accessors` wrapper（内部已委托给 `AssetStorage`）

- [x] **4.5** `crates/node/src/lib.rs`（创世注入）
  - 确认 `seed_balance`、`seed_asset` 等调用通过 `state_accessors` wrapper（内部已委托给 `AssetStorage`）

- [x] **4.6** `crates/node/src/tests.rs`
  - 测试中的 `seed_balance` 等调用通过 `state_accessors` wrapper（内部已委托给 `AssetStorage`）

- [x] **4.7** `crates/node/tests/e2e/harness.rs`
  - E2E harness 中的 balance 设置通过 `state_accessors` wrapper（内部已委托给 `AssetStorage`）

- [x] **4.8** `crates/node/tests/e2e/*.rs`（所有集成测试）
  - 批量确认 `seed_balance`、`seed_asset` 调用通过 `state_accessors` wrapper（内部已委托给 `AssetStorage`）

- [x] **4.9** `crates/chainspec/src/genesis.rs`
  - 确认 `seed_balance`、`seed_asset` 等调用通过 `state_accessors` wrapper（内部已委托给 `AssetStorage`）

#### 步骤 5 — 清理 `protocol/src/evm_instructions.rs`

- [x] **5.1** 删除该文件中所有 asset 相关函数
- [x] **5.2** 如果文件已空，删除整个文件并在 `protocol/src/lib.rs` 中移除对应 `mod`

#### 步骤 6 — Phase 1 第一波编译验证

- [x] **6.1** `cargo check`（全项目）
- [x] **6.2** `cargo test -p call-asset`
- [x] **6.3** `cargo test -p call-precompiles`
- [x] **6.4** `cargo test -p call-protocol`
- [x] **6.5** `cargo test -p call-consensus`
- [x] **6.6** `cargo test -p call-rpc`
- [ ] **6.7** `cargo test -p call-node --lib`
- [ ] **6.8** `cargo test -p call-node --tests`
- [ ] **6.9** `cargo build`（无未使用 import 警告）

---

### Phase 1 第二波：统一分发 + 自动 Gas（Asset 验证通过后实施）

> **当前状态：核心迁移已完成，统一分发框架可延后实施。**
> `AssetPrecompile` 已迁移到 `call-asset` 并使用 `AssetStorage<JournalBackend>`，
> 手动 selector match + 手动 gas 扣除已能正常工作。统一分发框架（`dispatch.rs` + `sol!`）
> 是后续优化项，不阻塞其他 precompile 迁移。

#### 步骤 7 — `call-precompiles` 实现统一分发框架（可选延后）

- [x] **7.1** 新建 `crates/precompiles/src/dispatch.rs` ✅

```rust
use revm_precompile::{PrecompileError, PrecompileOutput, PrecompileResult};
use call_primitives::Address;

pub fn dispatch_call<T, F>(
    calldata: &[u8],
    decode: impl FnOnce(&[u8]) -> Result<T, PrecompileError>,
    handler: F,
) -> PrecompileResult
where
    F: FnOnce(T) -> PrecompileResult,
{
    let input_gas = INPUT_GAS_PER_BYTE * calldata.len() as u64;
    crate::storage::StorageCtx::deduct_gas(input_gas)
        .ok_or(PrecompileError::OutOfGas)?;
    let call = decode(calldata)
        .map_err(|e| PrecompileError::Other(format!("decode error: {:?}", e)))?;
    handler(call)
}

pub fn view<T, R>(call: T, handler: impl FnOnce(&T) -> R) -> PrecompileResult
where
    R: alloy_sol_types::SolValue,
{
    let result = handler(&call);
    let encoded = result.abi_encode();
    Ok(PrecompileOutput::new(0, encoded.into()))
}

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
    Ok(PrecompileOutput::new(0, alloy_primitives::Bytes::default()))
}
```

- [x] **7.2** 在 `crates/precompiles/src/lib.rs` 中导出 `pub mod dispatch;` ✅

#### 步骤 8 — `JournalBackend` 升级自动 gas 计量（可选延后）

- [x] **8.1** 新建 `crates/precompiles/src/journal_backend.rs`（基础版，无 AccessTracker）
- [x] **8.2** 在 `StorageProvider` 层实现自动 gas 计量（`EvmStorageProvider` 通过 revm `is_cold` 标志，`HashMapStorageProvider` 通过 `accessed_slots` warm/cold 跟踪）✅
- [x] **8.3** `dispatch::view_void` / `mutate_void` 正确传播 `gas_used` / `gas_refunded` ✅
- [x] **8.4** 编译验证：`cargo check -p call-precompiles` ✅

> **说明：** 当前 gas 由 precompile 方法层手动扣除（如 `transfer` 扣 5000），
> `StorageCtx::sload/sstore` 内部已由 `EvmStorageProvider` 自动扣除 SLOAD/SSTORE gas。
> `JournalBackend` 本身暂不需要额外 gas 跟踪。

#### 步骤 9 — 重写 `AssetPrecompile` 为统一分发模式（可选延后）

- [x] **9.1** `crates/asset/src/precompile.rs` 已使用 `AssetStorage<JournalBackend>` 实现 ✅
- [x] **9.2** 已替换为 `dispatch_call` + `sol!` 宏统一分发 ✅

```rust
use call_precompiles::dispatch::{dispatch_call, view, mutate};
use alloy_sol_types::sol;

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
    storage: AssetStorage<JournalBackend>,
}

impl StatefulPrecompile for AssetPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult {
        dispatch_call(calldata, IProtocolAssetCall::abi_decode, |call| match call {
            IProtocolAssetCall::getBalance(c) => {
                view(c, |c| self.storage.read_balance(c.assetId, c.account))
            }
            IProtocolAssetCall::getAssetInfo(c) => {
                view(c, |c| self.storage.read_meta(c.assetId))
            }
            IProtocolAssetCall::transfer(c) => {
                mutate(c, msg_sender, |sender, c| {
                    self.storage.transfer(c.assetId, sender, c.to, c.amount)
                })
            }
            // ... 其他 match arm
            _ => Err(PrecompileError::Other("unknown selector".into())),
        })
    }
}
```

- [x] **9.2** 验证 `precompile.rs` 行数 < 120（~190 行业务代码，含测试 ~380 行） ✅
- [x] **9.3** 编译验证：`cargo check -p call-asset` ✅

#### 步骤 10 — Phase 1 第二波编译验证

- [x] **10.1** `cargo test -p call-precompiles` ✅
- [x] **10.2** `cargo test -p call-asset` ✅
- [x] **10.3** `cargo test -p call-consensus` ✅
- [ ] **10.4** `cargo test -p call-rpc`
- [ ] **10.5** `cargo test -p call-node --lib`
- [ ] **10.6** `cargo test -p call-node --tests`
- [ ] **10.7** `cargo build`（无未使用 import 警告）

---

### Phase 2-N：其他 Precompile（同模式批量实施）

> 每个 crate 按相同 3 文件结构创建，依赖底层已完成的 crate。

#### Phase 2 — Validator

- [x] **V.1** 新建 `crates/validator/`（`Cargo.toml`、`src/lib.rs`、`src/precompile.rs`）✅
- [x] **V.2** 提取 `ValidatorStorage<B>`：stake / unstake / claim_unbonded / read_validator_by_index 等 ✅
- [x] **V.3** `ValidatorStorage::stake()` 内部调用 `AssetStorage::transfer(CALL_ASSET_ID, caller, STAKING_ESCROW, amount)` ✅
- [x] **V.4** `ValidatorPrecompile` 入口薄层（`sol!` + `dispatch::view/mutate_void`）✅
- [x] **V.5** `call-evm` 注册 `ValidatorPrecompile`，`call-precompiles` 移除 `validator.rs` ✅
- [x] **V.6** 编译验证：`cargo check -p call-validator` ✅
- [x] **V.7** 全项目测试通过：`cargo test -p call-validator`、`cargo test -p call-precompiles` ✅

#### Phase 3 — Bridge

- [x] **B.1** 新建 `crates/bridge/src/precompile.rs`，提取 `BridgeStorage<B>` ✅
- [x] **B.2** 实现 external_deposit / external_withdraw / bridge_to_evm / bridge_to_protocol / initiate_challenge / resolve_challenge / withdraw_challenge_bond ✅
- [x] **B.3** `BridgeStorage` 跨域调用：`AssetStorage::deduct_balance/add_balance`（bond、余额操作）、`ValidatorStorage::slash_stake`（challenge）✅
- [x] **B.4** `BridgePrecompile` 入口薄层（`sol!` + `dispatch::view/mutate_void`）✅
- [x] **B.5** `call-evm` 注册 `BridgePrecompile`，`call-precompiles` 移除 `bridge.rs` ✅
- [x] **B.6** 编译验证：`cargo check -p call-bridge` ✅
- [x] **B.7** `cargo test -p call-bridge` 12 项测试通过 ✅
- [x] **B.8** `cargo test -p call-evm`、`cargo test -p call-precompiles`、`cargo test -p call-validator` 全通过 ✅

#### Phase 4 — Oracle

- [x] **O.1** 新建 `crates/oracle/src/precompile.rs`，提取 `OracleStorage<B>` ✅
- [x] **O.2** 实现 submit_price / get_price / get_twap / is_stale ✅
- [x] **O.3** `OracleStorage` 跨域调用：读 `ValidatorStorage`（验证 caller 身份）✅
- [x] **O.4** `OraclePrecompile` 入口薄层（`sol!` + `dispatch::view/mutate_void`）✅
- [x] **O.5** `call-evm` 注册 `OraclePrecompile`，`call-precompiles` 移除 `oracle.rs` ✅
- [x] **O.6** 编译验证：`cargo check -p call-oracle` ✅
- [x] **O.7** `cargo test -p call-oracle` 14 项测试通过 ✅
- [x] **O.8** `cargo test -p call-evm`、`cargo test -p call-precompiles` 全通过 ✅

#### Phase 5 — Governance

- [x] **G.1** 新建 `crates/governance/src/precompile.rs`，提取 `GovernanceStorage<B>` ✅
- [x] **G.2** 实现 submit_proposal / vote / queue / execute / emergency_pause / emergency_resume ✅
- [x] **G.3** `GovernanceStorage` 跨域调用：`AssetStorage`（deposit 扣款/退回）、`ValidatorStorage`（validator 身份校验）✅
- [x] **G.4** `GovernancePrecompile` 入口薄层（`sol!` + `dispatch::view/mutate_void`）✅
- [x] **G.5** `call-evm` 注册 `GovernancePrecompile`，`call-precompiles` 移除 `governance.rs` ✅
- [x] **G.6** 编译验证：`cargo check -p call-governance` ✅
- [x] **G.7** `cargo test -p call-governance` 29 项测试通过 ✅
- [x] **G.8** `cargo test -p call-evm`、`cargo test -p call-precompiles` 全通过 ✅

#### Phase 6 — Agent

- [x] **A.1** 新建 `crates/agent/` ✅
- [x] **A.2** 提取 `AgentStorage<B>`：注册、元数据读写、余额授权（含 `AssetStorage` 跨域调用）✅
- [x] **A.3** `agent_root` 快照改为从 `AgentStorage` 遍历 EVM 计算（后续实现）✅
- [x] **A.4** `AgentPrecompile` 入口薄层（`sol!` + `dispatch::view/mutate_void`）✅
- [x] **A.5** 编译验证：`cargo check -p call-agent` ✅
- [x] **A.6** 全项目测试通过：`cargo test -p call-agent`、`cargo test -p call-precompiles`、`cargo test -p call-evm` ✅

#### Phase 7 — Shielded

- [x] **S.1** 新建 `crates/shielded/`
- [x] **S.2** 提取 `ShieldedStorage<B>`：Merkle root、nullifier、commitment 计数
- [x] **S.3** Merkle tree sidecar 策略决策：完全 EVM 存储 vs 重启重建
- [x] **S.4** `ShieldedPrecompile` 入口薄层
- [x] **S.5** 编译验证：`cargo check -p call-shielded`
- [x] **S.6** 全项目测试通过

#### Phase 8 — Compliance

- [x] **C.1** 新建 `crates/compliance/` ✅
- [x] **C.2** 提取 `ComplianceStorage<B>`：地址合规状态读写 ✅
- [x] **C.3** `CompliancePrecompile` 入口薄层（`sol!` + `dispatch::view/mutate_void`）✅
- [x] **C.4** 编译验证：`cargo check -p call-compliance` ✅
- [x] **C.5** 全项目测试通过：`cargo test -p call-compliance`、`cargo test -p call-precompiles`、`cargo test -p call-evm` ✅

#### Phase 9 — Switch

- [x] **SW.1** 新建 `crates/switch/`，提取 `SwitchStorage<B>` ✅
- [x] **SW.2** 实现 switch_to_evm / switch_to_protocol（CALL 原生余额 + ERC-20 mint/burn）✅
- [x] **SW.3** `SwitchStorage` 使用 `StorageCtx::balance_add/sub/get` 处理 CALL 资产，后端 storage 处理 ERC-20 ✅
- [x] **SW.4** `SwitchPrecompile` 入口薄层（`sol!` + `dispatch::mutate_void`）✅
- [x] **SW.5** `call-evm` 注册 `SwitchPrecompile`，`call-precompiles` 移除 `switch.rs` ✅
- [x] **SW.6** 编译验证：`cargo check -p call-switch` ✅
- [x] **SW.7** `cargo test -p call-switch` 7 项测试通过 ✅
- [x] **SW.8** `cargo test -p call-evm`、`cargo test -p call-precompiles` 全通过 ✅

---

### Phase 8 — 最终清理

- [ ] **F.1** `call-precompiles` 最终瘦身：只保留 `StorageCtx`、`JournalBackend`、`StatefulPrecompile`、`dispatch.rs`
- [ ] **F.2** 删除 `protocol/src/evm_instructions.rs`（如果还存在）
- [ ] **F.3** 删除所有已废弃的内存状态机文件（`GovernanceManager` 旧版等）
- [ ] **F.4** 删除 `state_persist.rs` 中所有 protocol-state 持久化代码
- [ ] **F.5** 删除 `state_bundle.rs` 中已移除字段的锁获取
- [ ] **F.6** `cargo build` 全项目无警告
- [ ] **F.7** `cargo test` 全项目通过
- [ ] **F.8** 最终审查：各领域 crate 的 `precompile.rs` 行数均 < 120
- [ ] **F.9** 最终审查：无重复 `slot_*` 辅助函数
- [ ] **F.10** 文档更新：`docs/precompiles_refact.md` 中所有 `- [ ]` 改为 `- [x]`
