# Call-Node Protocol 预编译迁移方案

## 背景

当前 call-node 采用 **dual-track 架构**：
- **ProtocolTransaction**：结构化指令（Transfer/Mint/ShieldedTransfer/GovernanceVote 等），由 Rust 原生执行
- **EvmTransaction**：标准以太坊 RLP 交易，由 revm 执行

这导致：
1. MetaMask / Ethers.js 无法发送 ProtocolTransaction，生态隔离
2. 协议功能（隐私转账、治理投票、质押）无法被 Solidity 合约调用
3. 两种交易类型、两种 nonce 空间、两种手续费模型，维护成本高

本文档描述**将核心协议功能迁移为 EVM 预编译**的方案：彻底删除 ProtocolTransaction，所有协议功能通过预编译地址暴露给 EVM，实现单轨执行。

---

## 目标

1. Solidity 合约和 EOA 都能调用协议功能（Transfer、Shielded、Governance、Validator）
2. **彻底删除 ProtocolTransaction**：统一为 EvmTransaction 单轨执行
3. MetaMask 可直接发起隐私转账、治理投票、质押等操作
4. 保持现有共识逻辑不变（block header roots、snapshot/rollback）
5. 移除 dual-track 架构：不再区分 protocol tx 和 evm tx

---

## 非目标

- 保留 ProtocolTransaction 格式（本方案彻底删除）
- 将 AccountState 合并进 EVM state（保持独立，通过预编译桥接）
- 重写 ShieldedPool 的密码学逻辑（保持 Rust 实现，只改变调用入口）

---

## 架构设计

### 整体数据流

```
+---------------+      +------------------+      +------------------+
|  MetaMask/    |  ->  |  EvmTransaction  |  ->  |  revm            |
|  Solidity     |      |  (RLP, to=0x201) |      |  (识别预编译)     |
+---------------+      +------------------+      +------------------+
                                                          |
                                                          v
+---------------+      +------------------+      +------------------+
|  AccountState |  <-  |  Rust 预编译函数  |  <-  |  selector + args |
|  ShieldedState|      |  (读写协议状态)   |      |  ABI 解码         |
+---------------+      +------------------+      +------------------+
```

### 预编译地址分配

| 地址 | 模块 | 功能 | 当前状态 |
|------|------|------|---------|
| `0x101` | **Oracle** | getPrice, getTWAP, isStale, submitPrice | ✅ 已有（读），新增 submitPrice（写）|
| `0x103` | **Bridge** | getTotalDeposits, getTotalWithdrawals, externalBridgeDeposit, externalBridgeWithdraw, challengeBridgeDeposit | ✅ 已有（读），新增外部跨链（写）|
| `0x201` | **Asset** | getBalance, getAssetInfo, transfer, batchTransfer, approve, transferFrom, registerAsset, mint, burn | ✅ 新增，读写（合并原 0x102 Balance + 原 Transfer + 原 Asset）|
| `0x202` | **Shielded** | shieldedDeposit, shieldedWithdraw, shieldedTransfer | 🆕 新增，读写 |
| `0x203` | **Governance** | submitProposal, vote, queue, execute, emergencyPause, emergencyResume | 🆕 新增，读写 |
| `0x204` | **Validator** | stake, unstake, claimUnbonded | 🆕 新增，读写 |
| `0x205` | **Compliance** | updateCompliance, checkCompliance | 🆕 新增，读写 |
| `0x207` | **Switch** | switchToEvm, switchToProtocol | 🆕 新增，读写 |
| `0x209` | **Agent** | registerAgent, grantAgentBalance, revokeAgentBalance | 🆕 新增，读写 |

> **已废弃地址**：`0x102`（原 Balance，功能并入 `0x201`）、`0x208`（原 ExternalBridge，并入 `0x103`）。

---

## 状态共享层

预编译运行在 revm 内部，但需要读写 revm 外部的协议状态。由于 `Block::execute` 已经持有所有状态的写锁（`StateWriteBundle`），预编译**不能**再获取自己的锁，否则会导致死锁。

因此采用 **Thread-Local Scoped References** 架构：

### 新增文件：`crates/precompiles/src/state_hook.rs`

```rust
use std::cell::RefCell;
use call_protocol::AccountState;
use call_shielded::ShieldedState;
use call_protocol::registry::AssetRegistry;
use call_protocol::compliance::ComplianceEngine;
use call_governance::GovernanceManager;
use call_oracle::OracleManager;

/// Scoped references stored in thread-local storage.
/// Only valid while a `StateHookGuard` is alive in the same thread.
#[derive(Clone, Copy)]
struct ExecutionStateRef {
    account: *mut AccountState,
    registry: *mut AssetRegistry,
    compliance: *mut ComplianceEngine,
    shielded_state: *mut ShieldedState,
    oracle: *mut OracleManager,
    governance: *mut GovernanceManager,
}

thread_local! {
    static TL_STATE: RefCell<Option<ExecutionStateRef>> = RefCell::new(None);
}

/// Guard that injects raw pointers to protocol state into TLS.
///
/// # Safety
/// The caller must ensure all references outlive this guard.
/// This is naturally guaranteed when called from `Block::execute`.
pub struct StateHookGuard;

impl StateHookGuard {
    pub fn new(
        account: &mut AccountState,
        registry: &mut AssetRegistry,
        compliance: &mut ComplianceEngine,
        shielded_state: &mut ShieldedState,
        oracle: Option<&mut OracleManager>,
        governance: Option<&mut GovernanceManager>,
    ) -> Self {
        let refs = ExecutionStateRef {
            account,
            registry,
            compliance,
            shielded_state,
            oracle: oracle.map_or(std::ptr::null_mut(), |r| r),
            governance: governance.map_or(std::ptr::null_mut(), |r| r),
        };
        TL_STATE.with(|t| *t.borrow_mut() = Some(refs));
        Self
    }

    /// Create from raw pointers (useful when borrow-checker prevents normal construction).
    pub unsafe fn from_raw(
        account: *mut AccountState,
        registry: *mut AssetRegistry,
        compliance: *mut ComplianceEngine,
        shielded_state: *mut ShieldedState,
        oracle: *mut OracleManager,
        governance: *mut GovernanceManager,
    ) -> Self { ... }
}

impl Drop for StateHookGuard {
    fn drop(&mut self) {
        TL_STATE.with(|t| *t.borrow_mut() = None);
    }
}

/// Safe accessor used by precompiles.
pub fn with_account_state<F, R>(f: F) -> Option<R>
where F: FnOnce(&mut AccountState) -> R {
    TL_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref().map(|r| unsafe { f(&mut *r.account) })
    })
}
```

### Block::execute 注入点

**文件：** `crates/consensus/src/block.rs`

```rust
pub fn execute(...) -> Result<BlockExecutionResult, ConsensusError> {
    // 执行前：注入协议状态引用到 TLS
    let _state_hook = unsafe {
        call_precompiles::state_hook::StateHookGuard::from_raw(
            &mut state.account,
            &mut state.registry,
            &mut state.compliance,
            &mut state.shielded_state,
            subsystems.oracle.as_deref_mut(),
            subsystems.governance.as_deref_mut(),
        )
    };

    // ... 现有执行逻辑 ...

    // Drop _state_hook 自动清空 TLS，防止状态泄露到下一个 block
}
```

### 外部预编译注册

对于可能产生循环依赖的 crate（如 `call-consensus`、`call-agent`），使用 `OnceLock<fn(...)>` 外部注册：

```rust
// crates/precompiles/src/lib.rs
static VALIDATOR_PRECOMPILE_FN: OnceLock<fn(&[u8], u64) -> PrecompileResult> = OnceLock::new();
static AGENT_PRECOMPILE_FN: OnceLock<fn(&[u8], u64) -> PrecompileResult> = OnceLock::new();
static BRIDGE_EXT_PRECOMPILE_FN: OnceLock<fn(&[u8], u64) -> PrecompileResult> = OnceLock::new();
```

各 crate 在启动时注册自己的实现：
```rust
// crates/consensus/src/validator_precompile.rs
pub fn register_validator_precompile() {
    let _ = VALIDATOR_PRECOMPILE_FN.set(validator_precompile_fn);
}
```

> **注意**：当前 block 执行是单线程串行，TLS 方案无并发竞争。所有状态访问通过 `with_*` 安全函数进行，Guard 的 `Drop` 确保即使 panic 也能清空 TLS。

---

## 预编译详细设计

### 1. Asset 预编译（`0x201`）

**文件：** `crates/precompiles/src/asset.rs`

> 统一资产操作：查询、转账、授权、发行。合并原 `0x102` Balance + 原 Transfer + 原 Asset。

#### ABI

```solidity
interface IProtocolAsset {
    // ── 只读 ──
    // selector: keccak256("getBalance(uint64,address)")[:4]
    function getBalance(uint64 assetId, address account) external view returns (uint128);

    // selector: keccak256("getAssetInfo(uint64)")[:4]
    function getAssetInfo(uint64 assetId) external view returns (string memory symbol, string memory name, uint8 decimals, address issuer, address erc20Address);

    // ── 转账 ──
    // selector: keccak256("transfer(uint64,address,uint128)")[:4]
    function transfer(uint64 assetId, address to, uint128 amount) external returns (bool);

    // selector: keccak256("batchTransfer(uint64,address[],uint128[])")[:4]
    function batchTransfer(uint64 assetId, address[] calldata to, uint128[] calldata amounts) external returns (bool);

    // selector: keccak256("approve(uint64,address,uint128)")[:4]
    function approve(uint64 assetId, address spender, uint128 amount) external returns (bool);

    // selector: keccak256("transferFrom(uint64,address,address,uint128)")[:4]
    function transferFrom(uint64 assetId, address from, address to, uint128 amount) external returns (bool);

    // ── 发行 ──
    // selector: keccak256("registerAsset(string,string,uint8,uint128)")[:4]
    // 自动部署 WrappedToken ERC-20 合约
    function registerAsset(string calldata symbol, string calldata name, uint8 decimals, uint128 maxSupply) external returns (uint64 assetId, address erc20Address);

    // selector: keccak256("mint(uint64,address,uint128)")[:4]
    // 仅限 asset issuer
    function mint(uint64 assetId, address to, uint128 amount) external returns (bool);

    // selector: keccak256("burn(uint64,address,uint128)")[:4]
    // 仅限 asset issuer
    function burn(uint64 assetId, address from, uint128 amount) external returns (bool);
}
```

#### Rust 实现概要

```rust
pub fn asset_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let selector = &input[..4];
    let sender = /* 从 revm context 获取 caller */;

    let account_guard = LIVE_ACCOUNT_STATE.get()
        .ok_or(PrecompileError::Other("account state not available".into()))?;
    let mut account = account_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    let registry_guard = LIVE_REGISTRY.get()
        .ok_or(PrecompileError::Other("registry not available".into()))?;
    let registry = registry_guard.read().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    let compliance_guard = LIVE_COMPLIANCE.get()
        .ok_or(PrecompileError::Other("compliance not available".into()))?;
    let compliance = compliance_guard.read().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    match selector {
        // ── 只读 ──
        GET_BALANCE_SELECTOR => {
            const GAS_COST: u64 = 800;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let (asset_id, addr) = decode_get_balance_args(&input[4..]);
            let balance = account.get_balance(asset_id, &addr);
            let mut output = [0u8; 32];
            output[16..].copy_from_slice(&balance.to_be_bytes());
            return Ok(PrecompileOutput { bytes: Bytes::from(output.to_vec()), gas_used: GAS_COST, .. });
        }
        GET_ASSET_INFO_SELECTOR => {
            const GAS_COST: u64 = 1_000;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let asset_id = decode_asset_id(&input[4..]);
            let asset = registry.get_asset(asset_id).ok_or(PrecompileError::Other("asset not found".into()))?;
            let output = encode_asset_info(&asset);
            return Ok(PrecompileOutput { bytes: Bytes::from(output), gas_used: GAS_COST, .. });
        }
        // ── 转账 ──
        TRANSFER_SELECTOR => {
            const GAS_COST: u64 = 5_000;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let (asset_id, to, amount) = decode_transfer_args(&input[4..]);
            let policy_id = registry.get_asset(asset_id).map(|a| a.compliance_policy).unwrap_or(0);
            compliance.check_compliance_by_policy_id(&sender, policy_id)?;
            compliance.check_compliance_by_policy_id(&to, policy_id)?;
            account.transfer(asset_id, sender, to, amount)?;
            Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
        }
        APPROVE_SELECTOR => {
            const GAS_COST: u64 = 4_000;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let (asset_id, spender, amount) = decode_approve_args(&input[4..]);
            account.allowances.set_allowance(asset_id, sender, spender, amount);
            Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
        }
        TRANSFER_FROM_SELECTOR => {
            const GAS_COST: u64 = 5_500;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let (asset_id, from, to, amount) = decode_transfer_from_args(&input[4..]);
            let policy_id = registry.get_asset(asset_id).map(|a| a.compliance_policy).unwrap_or(0);
            compliance.check_compliance_by_policy_id(&from, policy_id)?;
            compliance.check_compliance_by_policy_id(&to, policy_id)?;
            account.allowances.spend_allowance(asset_id, from, sender, amount)?;
            account.transfer(asset_id, from, to, amount)?;
            Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
        }
        &[0x5f, 0x91, 0x61, 0xbb] => {
            const GAS_COST_PER: u64 = 5_000;
            let asset_id = decode_u64(input, 4)?;
            let recipients = decode_address_array(input, 36)?;
            let amounts = decode_u128_array(input, 68)?;
            if recipients.len() != amounts.len() {
                return Err(PrecompileError::Other("mismatch".into()));
            }
            let total_gas = GAS_COST_PER * recipients.len() as u64;
            if gas_limit < total_gas { return Err(PrecompileError::OutOfGas); }
            let from = require_caller()?;
            check_compliance(asset_id, &from)?;
            for to in &recipients { check_compliance(asset_id, to)?; }
            state_hook::with_account_state(|acc| {
                for (to, amount) in recipients.iter().zip(amounts.iter()) {
                    acc.transfer(asset_id, from, *to, *amount)
                        .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                }
                Ok::<_, PrecompileError>(())
            }).ok_or_else(|| PrecompileError::Other("account state not available".into()))?;
            Ok(PrecompileOutput { bytes: Bytes::new(), gas_used: total_gas, .. })
        }
        // ── 发行 ──
        REGISTER_ASSET_SELECTOR => {
            const GAS_COST: u64 = 50_000;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let (symbol, name, decimals, max_supply) = decode_register_asset_args(&input[4..]);
            let asset_id = registry.register_asset(symbol, name, decimals, max_supply, sender)?;
            // 自动部署 WrappedToken ERC-20
            let (contract_addr, _) = executor.deploy_erc20_template(
                SYSTEM_DEPLOYER, evm_state, &name, &symbol, decimals,
                BRIDGE_ADDRESS, sender, U256::from(max_supply), U256::from(asset_id),
            )?;
            registry.set_evm_contract_address(asset_id, contract_addr)?;
            let mut output = vec![0u8; 64];
            output[24..32].copy_from_slice(&asset_id.to_be_bytes());
            output[44..64].copy_from_slice(contract_addr.as_slice());
            Ok(PrecompileOutput { bytes: Bytes::from(output), gas_used: GAS_COST, .. })
        }
        MINT_SELECTOR => {
            const GAS_COST: u64 = 6_000;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let (asset_id, to, amount) = decode_mint_args(&input[4..]);
            let asset = registry.get_asset(asset_id).ok_or(PrecompileError::Other("asset not found".into()))?;
            if asset.issuer != sender { return Err(PrecompileError::Other("unauthorized".into())); }
            account.mint(asset_id, &sender, to, amount)?;
            Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
        }
        BURN_SELECTOR => {
            const GAS_COST: u64 = 5_000;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let (asset_id, from, amount) = decode_burn_args(&input[4..]);
            let asset = registry.get_asset(asset_id).ok_or(PrecompileError::Other("asset not found".into()))?;
            if asset.issuer != sender { return Err(PrecompileError::Other("unauthorized".into())); }
            account.burn(asset_id, from, amount)?;
            Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
        }
        _ => Err(PrecompileError::Other("unknown selector".into())),
    }
}
```

---

### 2. Switch 预编译（`0x207`）

**文件：** `crates/precompiles/src/switch.rs`

> 同一链内 protocol 层资产与 EVM wrapped ERC-20 之间的双向切换。

#### ABI

```solidity
interface IProtocolSwitch {
    // selector: keccak256("switchToEvm(uint64,address,uint128)")[:4]
    // protocol 资产 -> EVM ERC-20（mint wrapped token）
    function switchToEvm(uint64 assetId, address to, uint128 amount) external returns (bool);

    // selector: keccak256("switchToProtocol(uint64,address,uint128)")[:4]
    // EVM ERC-20 -> protocol 资产（burn wrapped token）
    function switchToProtocol(uint64 assetId, address to, uint128 amount) external returns (bool);
}
```

#### Rust 实现概要

```rust
pub fn switch_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 8_000;
    if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }

    let sender = /* 从 revm context 获取 caller */;
    let selector = &input[..4];

    let account_guard = LIVE_ACCOUNT_STATE.get()
        .ok_or(PrecompileError::Other("account state not available".into()))?;
    let mut account = account_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    let registry_guard = LIVE_REGISTRY.get()
        .ok_or(PrecompileError::Other("registry not available".into()))?;
    let registry = registry_guard.read().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    let Some(contract_addr) = registry.get_evm_contract_address(asset_id) else {
        return Err(PrecompileError::Other("asset has no wrapped token".into()));
    };

    match selector {
        SWITCH_TO_EVM_SELECTOR => {
            let (asset_id, to, amount) = decode_switch_to_evm_args(&input[4..]);
            // 1. 扣除 protocol 层余额
            account.deduct_balance(asset_id, sender, amount)
                .map_err(|e| PrecompileError::Other(format!("deduct failed: {e}")))?;
            // 2. EVM 层 mint wrapped token（通过 EvmExecutor 调用合约 mint）
            evm_executor.evm_call_mint(SYSTEM_CALLER, contract_addr, evm_state, to, U256::from(amount))
                .map_err(|e| PrecompileError::Other(format!("evm mint failed: {e}")))?;
        }
        SWITCH_TO_PROTOCOL_SELECTOR => {
            let (asset_id, to, amount) = decode_switch_to_protocol_args(&input[4..]);
            // 1. EVM 层 burn wrapped token（通过 EvmExecutor 调用合约 burn）
            evm_executor.evm_call_burn(SYSTEM_CALLER, contract_addr, evm_state, U256::from(amount))
                .map_err(|e| PrecompileError::Other(format!("evm burn failed: {e}")))?;
            // 2. 增加 protocol 层余额
            account.credit_balance(asset_id, to, amount)
                .map_err(|e| PrecompileError::Other(format!("credit failed: {e}")))?;
        }
        _ => return Err(PrecompileError::Other("unknown selector".into())),
    }

    Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
}
```

> **注意**：`switchToProtocol` 需要预编译内部触发 EVM `CALL`（burn 合约），这是少数需要"预编译回调 EVM"的场景。实现时需确保 gas 计算正确，避免重入。

---

### 3. Oracle 预编译（`0x101`）

**文件：** `crates/precompiles/src/oracle.rs`

> 合并只读查询与验证者提交价格。原有 `LIVE_ORACLE` 全局句柄复用。

#### ABI

```solidity
interface IProtocolOracle {
    // ── 只读 ──
    // selector: keccak256("getPrice(uint64)")[:4]
    function getPrice(uint64 assetId) external view returns (uint128);

    // selector: keccak256("getTWAP(uint64,uint64)")[:4]
    function getTWAP(uint64 assetId, uint64 currentTimestamp) external view returns (uint128);

    // selector: keccak256("isStale(uint64,uint64)")[:4]
    function isStale(uint64 assetId, uint64 currentTimestamp) external view returns (bool);

    // ── 写（仅限验证者）──
    // selector: keccak256("submitPrice(uint64,uint128,uint64,uint64,bytes,bytes[])")[:4]
    function submitPrice(
        uint64 assetId,
        uint128 price,
        uint64 blockNumber,
        uint64 timestamp,
        bytes calldata signature,
        bytes[] calldata sources
    ) external returns (bool);
}
```

#### Rust 实现概要（submitPrice 新增分支）

```rust
pub fn oracle_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    // 现有 getPrice / getTWAP / isStale 分支保持不变 ...

    // 新增 submitPrice 分支
    SUBMIT_PRICE_SELECTOR => {
        const GAS_COST: u64 = 3_000;
        if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }

        let sender = /* 从 revm context 获取 caller */;
        let (asset_id, price, block_number, timestamp, signature, sources) = decode_submit_price_args(&input[4..]);

        let Some(oracle_guard) = get_live_oracle() else {
            return Err(PrecompileError::Other("oracle not initialized".into()));
        };
        let mut oracle = oracle_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

        let validator_id = oracle.validator_id_by_address(sender)
            .ok_or(PrecompileError::Other("sender not a registered validator".into()))?;

        let submission = OracleSubmission {
            validator_id,
            pair: PricePair::new(asset_id, 0),
            price,
            block_number,
            timestamp,
            signature: signature.try_into().map_err(|_| PrecompileError::Other("invalid signature length".into()))?,
            sources,
        };

        oracle.submit_price(submission)
            .map_err(|e| PrecompileError::Other(format!("oracle submit failed: {e}")))?;

        Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
    }
    // ...
}
```

---

### 4. Agent 预编译（`0x209`）

**文件：** `crates/precompiles/src/agent.rs`

> Agent 注册、授权余额、撤销余额。

#### ABI

```solidity
interface IProtocolAgent {
    // selector: keccak256("registerAgent(bytes,string,string)")[:4]
    // 注册新 agent，sender 为 owner
    function registerAgent(bytes calldata pubkey, string calldata name, string calldata url) external returns (uint64 agentId);

    // selector: keccak256("grantAgentBalance(uint64,uint64,uint128)")[:4]
    // owner 给 agent 授权某资产额度
    function grantAgentBalance(uint64 agentId, uint64 assetId, uint128 amount) external returns (bool);

    // selector: keccak256("revokeAgentBalance(uint64,uint64)")[:4]
    // owner 撤销 agent 的某资产额度
    function revokeAgentBalance(uint64 agentId, uint64 assetId) external returns (bool);
}
```

#### Rust 实现概要

```rust
pub fn agent_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 6_000;
    if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }

    let sender = /* 从 revm context 获取 caller */;
    let selector = &input[..4];

    let agent_registry_guard = LIVE_AGENT_REGISTRY.get()
        .ok_or(PrecompileError::Other("agent registry not available".into()))?;
    let mut agent_registry = agent_registry_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    let agent_balances_guard = LIVE_AGENT_BALANCES.get()
        .ok_or(PrecompileError::Other("agent balances not available".into()))?;
    let mut agent_balances = agent_balances_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    let account_guard = LIVE_ACCOUNT_STATE.get()
        .ok_or(PrecompileError::Other("account state not available".into()))?;
    let mut account = account_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    match selector {
        REGISTER_AGENT_SELECTOR => {
            let (pubkey, name, url) = decode_register_args(&input[4..]);
            let agent_id = agent_registry.register(sender, pubkey, name, url)
                .map_err(|e| PrecompileError::Other(format!("register failed: {e}")))?;
            let mut output = [0u8; 32];
            output[24..].copy_from_slice(&agent_id.to_be_bytes());
            return Ok(PrecompileOutput { bytes: Bytes::from(output.to_vec()), gas_used: GAS_COST, .. });
        }
        GRANT_AGENT_BALANCE_SELECTOR => {
            let (agent_id, asset_id, amount) = decode_grant_args(&input[4..]);
            let agent = agent_registry.get(agent_id)
                .ok_or(PrecompileError::Other("agent not found".into()))?;
            if agent.owner != sender {
                return Err(PrecompileError::Other("only owner can grant".into()));
            }
            account.deduct_balance(asset_id, sender, amount)
                .map_err(|e| PrecompileError::Other(format!("deduct failed: {e}")))?;
            agent_balances.grant(agent_id, asset_id, amount);
        }
        REVOKE_AGENT_BALANCE_SELECTOR => {
            let (agent_id, asset_id) = decode_revoke_args(&input[4..]);
            let agent = agent_registry.get(agent_id)
                .ok_or(PrecompileError::Other("agent not found".into()))?;
            if agent.owner != sender {
                return Err(PrecompileError::Other("only owner can revoke".into()));
            }
            agent_balances.revoke(agent_id, asset_id);
        }
        _ => return Err(PrecompileError::Other("unknown selector".into())),
    }

    Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
}
```

---

### 5. Shielded 预编译（`0x202`）

**文件：** `crates/precompiles/src/shielded.rs`

#### ABI

```solidity
interface IProtocolShielded {
    // selector: keccak256("shieldedDeposit(uint64,uint128,bytes32,bytes)")[:4]
    function shieldedDeposit(uint64 assetId, uint128 amount, bytes32 commitment, bytes calldata encryptedNote) external returns (bool);

    // selector: keccak256("shieldedWithdraw(uint64,address,uint128,bytes,bytes32)")[:4]
    function shieldedWithdraw(uint64 assetId, address target, uint128 amount, bytes calldata proof, bytes32 nullifier) external returns (bool);

    // selector: keccak256("shieldedTransfer(uint64,bytes,bytes32[],bytes32[],bytes[])")[:4]
    function shieldedTransfer(uint64 assetId, bytes calldata proof, bytes32[] calldata nullifiers, bytes32[] calldata commitments, bytes[] calldata encryptedNotes) external returns (bool);
}
```

#### 特殊说明

- `shieldedTransfer` 内部调用 `call_shielded::verify_zk_proof()` 和 `process_transfer()`
- Groth16 证明验证是计算密集型，gas 定价 50,000
- 预编译内部直接操作 `ShieldedState.merkle_tree` 和 `nullifier_set`

---

### 6. Validator 预编译（`0x204`）

**文件：** `crates/precompiles/src/validator.rs`

#### ABI

```solidity
interface IProtocolValidator {
    // selector: keccak256("stake(bytes32,uint128)")[:4]
    function stake(bytes32 ed25519Pubkey, uint128 amount) external returns (bool);

    // selector: keccak256("unstake(uint32)")[:4]
    function unstake(uint32 validatorId) external returns (bool);

    // selector: keccak256("claimUnbonded(uint32)")[:4]
    function claimUnbonded(uint32 validatorId) external returns (bool);
}
```

---

### 7. Governance 预编译（`0x203`）

**文件：** `crates/precompiles/src/governance.rs`

#### ABI

```solidity
interface IProtocolGovernance {
    // selector: keccak256("submitProposal(uint8,string,string,bytes)")[:4]
    function submitProposal(uint8 proposalType, string calldata title, string calldata description, bytes calldata executionData) external returns (uint64 proposalId);

    // selector: keccak256("vote(uint64,uint8)")[:4]
    function vote(uint64 proposalId, uint8 vote) external returns (bool);

    // selector: keccak256("queue(uint64)")[:4]
    function queue(uint64 proposalId) external returns (bool);

    // selector: keccak256("execute(uint64)")[:4]
    function execute(uint64 proposalId) external returns (bool);

    // selector: keccak256("emergencyPause(string)")[:4]
    // 仅限验证者
    function emergencyPause(string calldata reason) external returns (bool);

    // selector: keccak256("emergencyResume()")[:4]
    function emergencyResume() external returns (bool);
}
```

---

### 8. Bridge 预编译（`0x103`）

**文件：** `crates/precompiles/src/bridge.rs`

> 合并原只读统计与原 ExternalBridge 跨链操作。

#### ABI

```solidity
interface IProtocolBridge {
    // ── 只读统计 ──
    // selector: keccak256("getTotalDeposits()")[:4]
    function getTotalDeposits() external view returns (uint256);

    // selector: keccak256("getTotalWithdrawals()")[:4]
    function getTotalWithdrawals() external view returns (uint256);

    // ── 外部跨链 ──
    // selector: keccak256("externalBridgeDeposit(bytes32,uint8,uint64,bytes,address,uint64,uint128,bytes)")[:4]
    // 验证者提交跨链存款证明
    function externalBridgeDeposit(
        bytes32 sourceTxHash,
        uint8 sourceChain,
        uint64 sourceBlockNumber,
        bytes calldata externalSender,
        address recipient,
        uint64 assetId,
        uint128 amount,
        bytes calldata validatorSignatures
    ) external returns (bool);

    // selector: keccak256("externalBridgeWithdraw(uint8,bytes,uint64,uint128)")[:4]
    // 用户发起提款到外部链
    function externalBridgeWithdraw(
        uint8 targetChain,
        bytes calldata targetAddress,
        uint64 assetId,
        uint128 amount
    ) external returns (bool);

    // selector: keccak256("challengeBridgeDeposit(bytes32,bytes)")[:4]
    // 任何人可挑战欺诈存款
    function challengeBridgeDeposit(bytes32 sourceTxHash, bytes calldata proof) external returns (bool);
}
```

#### Rust 实现概要

```rust
pub fn bridge_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    let sender = /* 从 revm context 获取 caller */;
    let selector = &input[..4];

    match selector {
        // ── 只读 ──
        GET_TOTAL_DEPOSITS_SELECTOR => {
            const GAS_COST: u64 = 800;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let Some(bridge_guard) = get_live_bridge() else {
                return Err(PrecompileError::Other("bridge not initialized".into()));
            };
            let bridge = bridge_guard.read().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;
            let mut output = [0u8; 32];
            output[16..].copy_from_slice(&bridge.total_deposits.to_be_bytes());
            return Ok(PrecompileOutput { bytes: Bytes::from(output.to_vec()), gas_used: GAS_COST, .. });
        }
        GET_TOTAL_WITHDRAWALS_SELECTOR => {
            const GAS_COST: u64 = 800;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let Some(bridge_guard) = get_live_bridge() else {
                return Err(PrecompileError::Other("bridge not initialized".into()));
            };
            let bridge = bridge_guard.read().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;
            let mut output = [0u8; 32];
            output[16..].copy_from_slice(&bridge.total_withdrawals.to_be_bytes());
            return Ok(PrecompileOutput { bytes: Bytes::from(output.to_vec()), gas_used: GAS_COST, .. });
        }
        // ── 外部跨链 ──
        EXTERNAL_BRIDGE_DEPOSIT_SELECTOR => {
            const GAS_COST: u64 = 10_000;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let args = decode_external_deposit_args(&input[4..]);

            let account_guard = LIVE_ACCOUNT_STATE.get()
                .ok_or(PrecompileError::Other("account state not available".into()))?;
            let mut account = account_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

            let bridge_guard = LIVE_BRIDGE_STATE.get()
                .ok_or(PrecompileError::Other("bridge state not available".into()))?;
            let mut bridge = bridge_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

            bridge.add_pending_op(BridgeOp::DepositToEvm { ... }, current_height);
            account.mint(args.asset_id, &SYSTEM_ISSUER, args.recipient, args.amount)?;
            Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
        }
        EXTERNAL_BRIDGE_WITHDRAW_SELECTOR => {
            const GAS_COST: u64 = 8_000;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let args = decode_external_withdraw_args(&input[4..]);

            let account_guard = LIVE_ACCOUNT_STATE.get()
                .ok_or(PrecompileError::Other("account state not available".into()))?;
            let mut account = account_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

            let bridge_guard = LIVE_BRIDGE_STATE.get()
                .ok_or(PrecompileError::Other("bridge state not available".into()))?;
            let mut bridge = bridge_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

            account.burn(args.asset_id, sender, args.amount)?;
            bridge.add_pending_op(BridgeOp::WithdrawToProtocol { ... }, current_height);
            Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
        }
        CHALLENGE_BRIDGE_DEPOSIT_SELECTOR => {
            const GAS_COST: u64 = 6_000;
            if gas_limit < GAS_COST { return Err(PrecompileError::OutOfGas); }
            let (source_tx_hash, proof) = decode_challenge_args(&input[4..]);

            let bridge_guard = LIVE_BRIDGE_STATE.get()
                .ok_or(PrecompileError::Other("bridge state not available".into()))?;
            let mut bridge = bridge_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

            bridge.challenge_deposit(source_tx_hash, proof)
                .map_err(|e| PrecompileError::Other(format!("challenge failed: {e}")))?;
            Ok(PrecompileOutput { bytes: Bytes::from([0x01; 32].to_vec()), gas_used: GAS_COST, .. })
        }
        _ => Err(PrecompileError::Other("unknown selector".into())),
    }
}
```

---

### 9. Compliance 预编译（`0x205`）

**文件：** `crates/precompiles/src/compliance.rs`

#### ABI

```solidity
interface IProtocolCompliance {
    // selector: keccak256("updateCompliance(uint64,address,uint8)")[:4]
    // 仅限 asset issuer
    function updateCompliance(uint64 assetId, address target, uint8 status) external returns (bool);

    // selector: keccak256("checkCompliance(uint64,address)")[:4]
    function checkCompliance(uint64 assetId, address target) external view returns (uint8);
}
```

---

## 执行路径改造

### 彻底取消 ProtocolTransaction

本方案**彻底删除** ProtocolTransaction，统一为 EvmTransaction 单轨执行。

**文件改动**：
- `crates/consensus/src/block.rs`：删除 `protocol_txs` 字段，block 只包含 `evm_txs`
- `crates/node/src/mempool.rs`：不再区分 protocol tx 和 evm tx，统一为 EVM RLP 交易
- `crates/rpc/src/standard.rs`：`eth_sendRawTransaction` 接收所有交易，`call_submit` 移除 ProtocolTransaction 支持
- `crates/protocol/src/instructions/`：删除 `execute_protocol_instructions` 和相关指令执行代码

**优点**：
- 单轨执行，MetaMask / 所有以太坊钱包完全兼容
- 一种 nonce 空间、一种 gas 模型、一种交易格式
- Solidity 合约可直接调用所有协议功能

**缺点**：
- ShieldedTransfer 等复杂指令的 ABI 编码对用户不友好，需提供前端 SDK 封装
- 现有 ProtocolTransaction 客户端/SDK 需要重写为 EVM 交易构造
- 需要重新设计 `call_submit` RPC（或完全依赖 `eth_sendRawTransaction`）

---

## Gas 定价

| 预编译 | Gas | 对比 EVM 等价操作 |
|--------|-----|------------------|
| getBalance | 800 | ERC-20 balanceOf ~2,100 |
| getAssetInfo | 1,000 | 只读查询 |
| transfer | 5,000 | ERC-20 transfer ~25,000 |
| batchTransfer (per recipient) | 5,000 | 循环 ERC-20 ~25,000 each |
| approve | 3,000 | ERC-20 approve ~20,000 |
| transferFrom | 6,000 | ERC-20 transferFrom ~28,000 |
| registerAsset | 50,000 | 自动部署合约 |
| mint | 10,000 | ERC-20 mint ~35,000 |
| burn | 8,000 | ERC-20 burn ~25,000 |
| switchToEvm | 30,000 | protocol 资产切换到 EVM |
| switchToProtocol | 30,000 | EVM 资产切换回 protocol |
| getPrice | 1,000 | 只读查询 |
| getTWAP | 1,500 | 只读查询 |
| isStale | 800 | 只读查询 |
| submitPrice | 5,000 | 预言机验证者提交价格 |
| registerAgent | 10,000 | Agent 注册 |
| grantAgentBalance | 10,000 | Agent 授权余额 |
| revokeAgentBalance | 10,000 | Agent 撤销余额 |
| shieldedDeposit | 50,000 | 不可能在 Solidity 实现 |
| shieldedWithdraw | 50,000 | 不可能在 Solidity 实现 |
| shieldedTransfer | 100,000 | 不可能在 Solidity 实现 |
| stake | 20,000 | 不可能在 Solidity 实现 |
| unstake | 20,000 | 不可能在 Solidity 实现 |
| claimUnbonded | 20,000 | 不可能在 Solidity 实现 |
| submitProposal | 50,000 | Governor Bravo ~80,000 |
| vote | 10,000 | Governor Bravo ~50,000 |
| queue | 15,000 | 治理队列 |
| execute | 30,000 | 治理执行 |
| emergencyPause | 20,000 | 紧急暂停 |
| emergencyResume | 20,000 | 紧急恢复 |
| getTotalDeposits | 800 | 只读查询 |
| getTotalWithdrawals | 800 | 只读查询 |
| externalBridgeDeposit | 50,000 | 外部跨链存款（多签验证） |
| externalBridgeWithdraw | 30,000 | 外部跨链提款 |
| challengeBridgeDeposit | 20,000 | 挑战欺诈存款 |
| updateCompliance | 10,000 | 合规状态更新 |
| checkCompliance | 1,000 | 只读查询 |

---

## 注册与集成

### `crates/precompiles/src/lib.rs`

```rust
pub const ORACLE_ADDRESS: Address = address!("0000000000000000000000000000000000000101");
pub const BRIDGE_ADDRESS: Address = address!("0000000000000000000000000000000000000103");
pub const ASSET_ADDRESS: Address = address!("0000000000000000000000000000000000000201");
pub const SHIELDED_ADDRESS: Address = address!("0000000000000000000000000000000000000202");
pub const GOVERNANCE_ADDRESS: Address = address!("0000000000000000000000000000000000000203");
pub const VALIDATOR_ADDRESS: Address = address!("0000000000000000000000000000000000000204");
pub const COMPLIANCE_ADDRESS: Address = address!("0000000000000000000000000000000000000205");
pub const SWITCH_ADDRESS: Address = address!("0000000000000000000000000000000000000207");
pub const AGENT_ADDRESS: Address = address!("0000000000000000000000000000000000000209");

fn build_precompiles_for_spec(spec: SpecId) -> Precompiles {
    let mut precompiles = revm_precompile::Precompiles::new(
        PrecompileSpecId::from_spec_id(spec)
    ).clone();

    // 0x101 Oracle（读+写）
    precompiles.extend([
        Precompile::new(PrecompileId::Custom("call_oracle".into()), ORACLE_ADDRESS, oracle_precompile_fn),
    ]);

    // 0x103 Bridge（读统计+外部跨链）
    precompiles.extend([
        Precompile::new(PrecompileId::Custom("call_bridge".into()), BRIDGE_ADDRESS, bridge_precompile_fn),
    ]);

    // 0x201~0x209 协议功能预编译
    precompiles.extend([
        Precompile::new(PrecompileId::Custom("call_asset".into()), ASSET_ADDRESS, asset_precompile_fn),
        Precompile::new(PrecompileId::Custom("call_shielded".into()), SHIELDED_ADDRESS, shielded_precompile_fn),
        Precompile::new(PrecompileId::Custom("call_governance".into()), GOVERNANCE_ADDRESS, governance_precompile_fn),
        Precompile::new(PrecompileId::Custom("call_validator".into()), VALIDATOR_ADDRESS, validator_precompile_fn),
        Precompile::new(PrecompileId::Custom("call_compliance".into()), COMPLIANCE_ADDRESS, compliance_precompile_fn),
        Precompile::new(PrecompileId::Custom("call_switch".into()), SWITCH_ADDRESS, switch_precompile_fn),
        Precompile::new(PrecompileId::Custom("call_agent".into()), AGENT_ADDRESS, agent_precompile_fn),
    ]);

    precompiles
}
```

---

## 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| 全局 RwLock 死锁 | block 执行 panic | 当前单线程执行无并发；未来并行需改通道通信 |
| Snapshot 内存膨胀 | clone AccountState 代价高 | 监控 100k+ 账户场景；必要时改用 Cow/Copy-on-Write |
| 预编译失败 revert | 协议层状态已部分修改 | 预编译内部先做所有验证再写状态；或依赖 revm state revert |
| Switch 回调 EVM | 重入风险、gas 计算错误 | switchToProtocol 先 burn 再 credit；确保 gas limit 正确传递 |
| 全局 RwLock 数量增加 | 更多潜在的 lock 竞争 | 当前单线程无竞争；未来并行需统一改为通道通信 |
| Gas 定价不合理 | 被攻击者滥用 | 初期用保守定价，主网后根据实际 profiling 调整 |
| ABI 不兼容 Solidity | dApp 无法调用 | 提供官方 Solidity interface + Foundry 测试套件 |
| 共识分裂 | 不同节点预编译结果不一致 | 确保只读操作 deterministic；写操作走统一 state 路径 |

---

## 回滚方案

如果 testnet 上发现问题：

1. 预编译地址保留但返回 `not supported`，回退到纯 EVM 行为
2. 需要临时恢复 ProtocolTransaction 支持时，基于 git 历史恢复 `protocol_txs` 相关代码
3. 核心改动（预编译本身）与 ProtocolTransaction 删除解耦：即使回滚，预编译仍可保留只读功能

---

## 实现优先级

| 阶段 | 内容 | 预计工作量 | 验证标准 |
|------|------|----------|---------|
| P0 | 状态共享层 `state_hook.rs` + Block::execute 注入 | 1 天 | 现有测试通过 |
| P0 | Asset 预编译（getBalance/transfer/approve/registerAsset） | 2 天 | MetaMask 可发转账、注册资产 |
| P1 | Switch 预编译 | 1 天 | protocol↔EVM 切换测试通过 |
| P1 | Shielded 预编译 | 2 天 | 现有 shielded 测试通过 |
| P2 | Validator 预编译 | 1 天 | 质押测试通过 |
| P2 | Governance 预编译 | 1 天 | 治理测试通过 |
| P3 | Oracle(submitPrice) + Bridge(外部跨链) 预编译 | 2 天 | 预言机/桥接测试通过 |
| P3 | Compliance + Agent 预编译 | 1 天 | 合规/Agent 测试通过 |
| P4 | Gas 定价调优 + benchmark | 1 天 | 与原生执行对比 < 10% 开销 |
| P5 | Solidity interface 合约 + dApp 示例 | 2 天 | Hardhat/Foundry 可交互 |

**总计**：~3 周（1 名 Rust 工程师全职）

---

## 相关文件索引

| 文件 | 当前角色 | 改动内容 |
|------|---------|---------|
| `crates/precompiles/src/state_hook.rs` | 新建 | 全局状态共享句柄 |
| `crates/precompiles/src/asset.rs` | 新建 | Asset 预编译（合并 Balance+Transfer+Asset） |
| `crates/precompiles/src/switch.rs` | 新建 | Switch 预编译 |
| `crates/precompiles/src/oracle.rs` | 已有 | 新增 submitPrice 分支 |
| `crates/precompiles/src/agent.rs` | 新建 | Agent 预编译 |
| `crates/precompiles/src/shielded.rs` | 新建 | Shielded 预编译 |
| `crates/precompiles/src/validator.rs` | 新建 | Validator 预编译 |
| `crates/precompiles/src/governance.rs` | 新建 | Governance 预编译 |
| `crates/precompiles/src/bridge.rs` | 已有 | 新增外部跨链存取/挑战分支 |
| `crates/precompiles/src/compliance.rs` | 新建 | Compliance 预编译 |
| `crates/precompiles/src/lib.rs` | 预编译注册 | 注册 0x101/0x103/0x201~0x209 |
| `crates/protocol/src/instructions/exec.rs` | 指令执行 | 删除 |
| `crates/consensus/src/block.rs` | Block 执行 | 注入/回收全局状态，删除 protocol_txs |
| `docs/precompile.md` | 本文档 | 方案说明 |
