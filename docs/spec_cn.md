# Callchain Specification

## 版本信息

| 项目 | 值 |
|------|------|
| 版本 | 0.1.0-draft |
| 日期 | 2026-04-13 |
| 状态 | 草稿 |
| 语言 | Rust |

---

## 1. 概述

Callchain 是一个高性能 Layer-1 区块链，采用**单一 EVM 执行域架构**：所有交易均为标准 EVM 交易，协议层功能（资产转账、桥接、Agent 支付、Shielded 隐私等）通过预编译合约地址（`0x101`–`0x209`）暴露给 EVM。协议状态与 EVM 状态通过内部桥接机制实现资产在两层之间的无缝流转。

### 1.1 设计原则

- **资产一等公民**：稳定币等资产在协议层拥有原生余额映射，享受确定性执行和固定费用
- **开放发行**：任何人都能在链上注册资产，无需许可
- **EVM 兼容**：完全兼容以太坊，现有 DeFi 生态可无缝迁移
- **单一执行域**：所有交易均为 EVM 交易，协议功能通过预编译地址暴露
- **合规框架**：协议级合规策略引擎，发行方自主选择策略

### 1.2 核心架构

```
                    Callchain L1
              Simplex BFT 共识（单一验证者集）
                         │
                         ▼
                  EVM 执行域
            （标准以太坊交易）
                         │
          ┌──────────────┼──────────────┐
          ▼              ▼              ▼
   预编译合约        EVM 合约        系统交易
   (0x101-0x209)   (DeFi/ERC-20)   (奖励/费用结算)
          │              │
          ▼              ▼
   ProtocolBalances   ERC-20 Storage
   (协议级余额映射)   (合约独立余额)
          │              │
          └──────────┬───┘
                     ▼
            Internal Bridge
           (协议级内部桥接)
           lock-and-release 机制
```

---

## 2. 共识层

### 2.1 算法

采用 **Commonware Simplex BFT** 共识算法。

Simplex 是一种低延迟拜占庭容错共识协议，由 Commonware 提供生产级 Rust 实现（`commonware-consensus` crate）。其核心设计是一个简化的 BFT 状态机，每轮由一个提议者打包区块，验证者投票，达到 2/3 多数即最终确认。

**核心特性：**

- BFT 容错：容忍 1/3 拜占庭验证者
- 通信复杂度：O(n) — 每轮仅需线性数量的消息交换
- 最终性：单轮确认，亚秒级最终性（~500ms，2 轮）
- 区块时间：250ms
- 优雅降级：网络分区时自动暂停出块，分区恢复后立即继续

**选择 Simplex 的理由：**

| 维度 | Simplex | Tendermint 系 (Malachite) | HotStuff 系 |
|------|---------|---------------------------|-------------|
| 通信复杂度 | O(n) | O(n²) | O(n) |
| 状态机复杂度 | 最低 | 中等 | 高 |
| Rust 实现成熟度 | commonware-consensus 可直接使用 | malachite-bft 可用 | 无成熟开源 Rust 实现 |
| 子集轮换 | 原生支持 | 需额外实现 | 原生支持 |
| 审计面 | 最小 | 中等 | 大 |
| 生产验证 | Tempo 链验证中 | Arc 链验证中 | PlasmaBFT（闭源） |

**关键设计决策：** 216 验证者下，Simplex 的 O(n) = 216 条消息/轮，而 Tendermint 的 O(n²) ≈ 46K 条消息/轮。通信量差距 200 倍，直接决定延迟上限。

### 2.2 依赖

```toml
[dependencies]
commonware-consensus = "2026.3.0"
commonware-cryptography = "2026.3.0"
commonware-p2p = "2026.3.0"
commonware-runtime = "2026.3.0"
commonware-codec = "2026.3.0"

# Reth 全量集成（EVM + 节点框架 + RPC + 存储）
# 锁定 commit 与 Alloy 1.8.2 兼容，避免 crates.io 版本不一致
reth-chainspec = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-consensus = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-consensus-common = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-db = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-db-api = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-e2e-test-utils = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-engine-local = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-engine-tree = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-errors = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-ethereum = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-ethereum-consensus = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-ethereum-engine-primitives = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-ethereum-primitives = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-evm = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-evm-ethereum = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-api = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-builder = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-core = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-ethereum = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-node-metrics = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-payload-builder = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-provider = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-revm = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-rpc = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-rpc-builder = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-rpc-eth-api = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-rpc-eth-types = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-storage-api = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-tracing = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-transaction-pool = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-trie = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-trie-common = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
reth-trie-db = { git = "https://github.com/paradigmxyz/reth", rev = "a550b7a" }
```

### 2.3 验证者

| 参数 | 值 |
|------|------|
| 验证者数量 | 100-216 |
| 每轮子集大小 | 21 |
| 轮换机制 | 轮次随机选择 |
| 最小质押 | 动态调整（保持验证者数稳定） |
| 出块奖励 | 50% 费用分配 + 0% 通胀 |
| 惩罚 | 双签惩罚 + 离线惩罚 |

### 2.4 区块结构

```rust
struct Block {
    header: BlockHeader,
    evm_txs: Vec<EvmTx>,                     // EVM 交易（RLP 编码原始字节）
    system_txs: Vec<SystemTx>,               // 系统交易
    bridge_operations: Vec<BridgeOp>,         // 桥接操作
}

struct BlockHeader {
    parent_hash: Hash,
    height: u64,
    timestamp_millis: u64,          // 亚秒级时间戳
    payment_root: Hash,             // 协议余额 Merkle 根
    evm_state_root: Hash,           // EVM 状态根
    bridge_root: Hash,              // 桥接状态 Merkle 根
    receipt_root: Hash,             // 交易收据 Merkle 根
    proposer: ValidatorId,
    signature: Signature,
}
```

### 2.5 区块执行顺序

每个区块按以下顺序处理：

```
1. 执行 EVM 交易（evm_txs）
   - 标准以太坊交易通过 revm 执行
   - 协议操作通过调用预编译地址（0x101–0x209）触发
2. 执行桥接操作（bridge_operations）
   - 处理 EVM → Protocol 的提取请求
   - 处理 Protocol → EVM 的存款请求
3. 执行系统交易（system_txs）
   - 验证者奖励分配
   - 费用结算
   - 合规策略更新
4. 计算最终状态根，打包区块头
```

---

## 3. 协议支付层

### 3.1 资产注册表

任何人均可注册资产。注册后自动获得协议层余额映射。

```rust
struct Asset {
    id: AssetId,                    // 协议分配的唯一 ID (u64)
    name: String,                   // 显示名称，如 "USDC"
    symbol: String,                 // 交易符号，如 "USDC"
    decimals: u8,
    issuer: Address,                // 发行方地址
    total_supply: u128,             // 协议层总供应
    policy: CompliancePolicy,       // 合规策略
    registered_at: u64,             // 注册时间戳
    evm_contract: Address,          // 对应的 ERC-20 合约地址
    bridge_reserve: u128,           // 桥接池中的余额
    status: AssetStatus,
}

enum AssetStatus {
    Active,                         // 正常
    Frozen,                         // 暂停（发行方可触发）
    Delisted,                       // 下架
}
```

### 3.2 注册流程

```rust
fn register_asset(
    caller: Address,
    name: String,
    symbol: String,
    decimals: u8,
    initial_supply: u128,
    policy: CompliancePolicy,
    registration_fee: Balance,      // 经济防垃圾费
) -> Result<AssetId>;
```

注册时：
1. 扣除注册费（防止垃圾注册）
2. 分配唯一 AssetId
3. 创建协议层余额映射，`balances[issuer] = initial_supply`
4. 在 EVM 层自动部署对应的 ERC-20 合约
5. ERC-20 合约初始供应为 0（所有资产在协议层）
6. 桥接合约获得该代币的 mint/burn 权限

### 3.3 余额管理

```rust
/// 协议层余额映射
/// asset_id → (address → balance)
type ProtocolBalances = HashMap<AssetId, HashMap<Address, u128>>;

/// 允许度映射（用于 approve/transferFrom）
/// (asset_id, owner, spender) → amount
type Allowances = HashMap<(AssetId, Address, Address), u128>;
```

### 3.4 合规策略

```rust
enum CompliancePolicy {
    /// 无限制，任何人都能收发
    None,
    /// OFAC 制裁地址黑名单
    OfacBlacklist { blacklist_hash: Hash },
    /// 需要 KYC 证明（链下验证，链上标记）
    KycRequired,
    /// 仅白名单地址
    Whitelist { registry: Address },
    /// 自定义逻辑（通过预编译回调）
    Custom { handler: Address },
}
```

协议层在每次转账前执行合规检查：

```rust
fn check_compliance(
    state: &ProtocolState,
    asset: &Asset,
    from: Address,
    to: Address,
) -> Result<()> {
    match asset.policy {
        CompliancePolicy::None => Ok(()),
        CompliancePolicy::OfacBlacklist { .. } => {
            ensure!(!is_sanctioned(from), "Sender is sanctioned");
            ensure!(!is_sanctioned(to), "Receiver is sanctioned");
            Ok(())
        }
        CompliancePolicy::KycRequired => {
            ensure!(is_kyc_verified(to), "Receiver not KYC verified");
            Ok(())
        }
        CompliancePolicy::Whitelist { registry } => {
            ensure!(is_whitelisted(registry, to), "Receiver not whitelisted");
            Ok(())
        }
        CompliancePolicy::Custom { handler } => {
            // 自定义合规策略通过调用 EVM 合约实现
            evm_call(handler, abi_encode("checkCompliance(address,address)", from, to))?;
            Ok(())
        }
    }
}
```

### 3.5 协议预编译调用模型 (Protocol Precompile Model)

所有协议层功能（资产转账、桥接、Agent 支付、Shielded 隐私等）均通过 EVM 预编译地址暴露。用户发送标准 EVM 交易，目标地址为预编译地址，数据为 ABI 编码的操作参数。

**预编译地址范围：** `0x101` – `0x209`

```rust
/// 预编译地址分配
const PRECOMPILE_ASSET_REGISTRY: Address = address!(0x101);  // 资产注册
const PRECOMPILE_TRANSFER: Address     = address!(0x102);  // 单笔转账
const PRECOMPILE_BATCH_TRANSFER: Address = address!(0x103); // 批量转账
const PRECOMPILE_APPROVE: Address      = address!(0x104);  // 授权
const PRECOMPILE_TRANSFER_FROM: Address = address!(0x105); // 代授权转账
const PRECOMPILE_MINT: Address         = address!(0x106);  // 发行方增发
const PRECOMPILE_BURN: Address         = address!(0x107);  // 发行方销毁
const PRECOMPILE_BRIDGE_DEPOSIT: Address = address!(0x108); // 协议层 → EVM 层桥接
const PRECOMPILE_BRIDGE_WITHDRAW: Address = address!(0x109); // EVM 层 → 协议层桥接
const PRECOMPILE_AGENT_PAY: Address    = address!(0x10A);  // Agent 代付
const PRECOMPILE_AGENT_BATCH_PAY: Address = address!(0x10B); // Agent 批量支付
const PRECOMPILE_AGENT_CALL: Address   = address!(0x10C);  // Agent 调用 EVM 合约
const PRECOMPILE_SHIELDED_DEPOSIT: Address = address!(0x10D); // 存入 Shielded Pool
const PRECOMPILE_SHIELDED_TRANSFER: Address = address!(0x10E); // 隐私转账
const PRECOMPILE_SHIELDED_WITHDRAW: Address = address!(0x10F); // 从 Shielded Pool 提取
const PRECOMPILE_UPDATE_COMPLIANCE: Address = address!(0x110); // 更新合规状态
// ... 预留至 0x209
```

**预编译执行上下文：**

预编译合约通过 Thread-Local 状态共享（`StateHookGuard`）访问协议层状态，在 revm 执行框架内运行：

```rust
/// 预编译执行入口
fn execute_precompile(
    address: Address,
    input: &Bytes,
    gas_limit: u64,
    evm_context: &mut Context,
) -> PrecompileResult {
    // 1. 通过 StateHookGuard 获取协议状态引用
    let state = StateHookGuard::current();

    // 2. 解析 ABI 编码的输入
    let decoded = abi_decode(address, input)?;

    // 3. 执行协议操作
    let result = match address {
        PRECOMPILE_TRANSFER => execute_transfer(state, decoded)?,
        PRECOMPILE_BATCH_TRANSFER => execute_batch_transfer(state, decoded)?,
        PRECOMPILE_BRIDGE_DEPOSIT => execute_bridge_deposit(state, decoded)?,
        PRECOMPILE_AGENT_PAY => execute_agent_pay(state, decoded)?,
        PRECOMPILE_SHIELDED_TRANSFER => execute_shielded_transfer(state, decoded)?,
        // ... 其他预编译
        _ => return Err(PrecompileError::NotFound),
    };

    // 4. 返回 ABI 编码的输出
    Ok(PrecompileOutput::new(gas_used, abi_encode(result)))
}
```

**典型用例：**

```solidity
// 用例 1：发工资 — 单笔 EVM 交易批量支付给 100 人
// 调用预编译 0x103 (BatchTransfer)
bytes memory data = abi.encode(
    USDC_ASSET_ID,           // uint64 asset_id
    recipients,              // address[] to
    amounts,                 // uint128[] amounts
    "PAYROLL-MAR-2026"       // string reference
);
address(PRECOMPILE_BATCH_TRANSFER).call(data);

// 用例 2：DeFi 入场 — 桥接 + ERC-20 approve 一步完成
// 交易 1：调用预编译 0x108 (BridgeDeposit) 将 USDC 桥到 EVM 层
bytes memory bridgeData = abi.encode(USDC_ASSET_ID, user_address, 1000_000000);
address(PRECOMPILE_BRIDGE_DEPOSIT).call(bridgeData);

// 交易 2：调用标准 ERC-20 approve（EVM 合约层）
usdcToken.approve(dexRouter, type(uint256).max);

// 用例 3：Agent 自动付款 — 调用预编译 0x10A (AgentPay)
bytes memory agentData = abi.encode(
    agent_id,                // uint64 agent_id
    USDC_ASSET_ID,           // uint64 asset_id
    vendor,                  // address to
    100_000000               // uint128 amount
);
address(PRECOMPILE_AGENT_PAY).call(agentData);
```

**设计理由：**

1. **单一执行域**：所有交易均为标准 EVM 交易，无需自定义交易格式
2. **工具兼容**：MetaMask、Foundry、Hardhat 等以太坊工具可直接使用
3. **原子性**：单笔 EVM 交易内可组合多个预编译调用（通过合约或 multicall）
4. **费用统一**：统一使用 EVM gas 模型，无需独立的费用计算
5. **状态隔离**：预编译通过 `StateHookGuard` 访问协议状态，与 EVM 状态互不干扰

**支付备注（PaymentMemo）：**

预编译调用支持通过 ABI 编码附加备注信息：

```solidity
struct PaymentMemo {
    string message;      // 最大 256 字节
    string reference;    // 最大 128 字节
    bytes metadata;      // 最大 1024 字节
}
```

备注数据写入交易收据，每个额外字节增加 ~1 gas。

### 3.6 预编译执行语义

预编译合约在 revm 执行框架内运行，使用快照机制保证协议状态操作的原子性。

```rust
/// 预编译执行入口（由 revm 调用）
fn execute_precompile(
    address: Address,
    input: &Bytes,
    gas_limit: u64,
    evm_context: &mut Context,
) -> PrecompileResult {
    // 1. 通过 StateHookGuard 获取协议状态的可变引用
    let state = StateHookGuard::current();

    // 2. 保存协议状态快照（用于回滚）
    let state_snapshot = state.take_snapshot();

    // 3. 解析 ABI 编码输入
    let decoded = match abi_decode(address, input) {
        Ok(d) => d,
        Err(e) => return Err(PrecompileError::from(e)),
    };

    // 4. 执行对应协议操作
    let result = match address {
        PRECOMPILE_TRANSFER => {
            let (asset_id, to, amount) = decoded.as_transfer()?;
            check_compliance(state, asset_id, msg_sender, to)?;
            transfer_balance(state, asset_id, msg_sender, to, amount)?;
        }
        PRECOMPILE_BATCH_TRANSFER => {
            let (asset_id, payments) = decoded.as_batch_transfer()?;
            for payment in payments {
                check_compliance(state, asset_id, msg_sender, payment.to)?;
                transfer_balance(state, asset_id, msg_sender, payment.to, payment.amount)?;
            }
        }
        PRECOMPILE_APPROVE => {
            let (asset_id, spender, amount) = decoded.as_approve()?;
            set_allowance(state, asset_id, msg_sender, spender, amount)?;
        }
        PRECOMPILE_TRANSFER_FROM => {
            let (asset_id, from, to, amount) = decoded.as_transfer_from()?;
            spend_allowance(state, asset_id, from, msg_sender, to, amount)?;
        }
        PRECOMPILE_MINT => {
            let (asset_id, to, amount) = decoded.as_mint()?;
            let asset = state.get_asset(asset_id)?;
            ensure!(asset.issuer == msg_sender, "Only issuer can mint");
            mint_balance(state, asset_id, to, amount)?;
        }
        PRECOMPILE_BURN => {
            let (asset_id, from, amount) = decoded.as_burn()?;
            let asset = state.get_asset(asset_id)?;
            ensure!(asset.issuer == msg_sender, "Only issuer can burn");
            burn_balance(state, asset_id, from, amount)?;
        }
        PRECOMPILE_AGENT_PAY => {
            let (agent_id, asset_id, to, amount) = decoded.as_agent_pay()?;
            execute_agent_pay(state, agent_id, asset_id, to, amount)?;
        }
        PRECOMPILE_AGENT_BATCH_PAY => {
            let (agent_id, asset_id, payments) = decoded.as_agent_batch_pay()?;
            execute_agent_batch_pay(state, agent_id, asset_id, payments)?;
        }
        PRECOMPILE_AGENT_CALL => {
            let (agent_id, asset_id, contract, data, value) = decoded.as_agent_call()?;
            execute_agent_call(state, agent_id, asset_id, contract, data, value)?;
        }
        PRECOMPILE_BRIDGE_DEPOSIT => {
            let (asset_id, to, amount) = decoded.as_bridge_deposit()?;
            execute_bridge_deposit(state, asset_id, msg_sender, to, amount)?;
        }
        PRECOMPILE_UPDATE_COMPLIANCE => {
            let (asset_id, target, status) = decoded.as_update_compliance()?;
            let asset = state.get_asset(asset_id)?;
            ensure!(asset.issuer == msg_sender, "Only issuer can update compliance");
            update_compliance_status(state, target, asset_id, status)?;
        }
        PRECOMPILE_SHIELDED_TRANSFER => {
            let (asset_id, commitments, nullifiers, proof) = decoded.as_shielded_transfer()?;
            verify_zk_proof(proof)?;
            for nf in &nullifiers {
                ensure!(!state.is_nullifier_spent(asset_id, *nf), "Nullifier already spent");
            }
            let asset = state.get_asset(asset_id)?;
            verify_shielded_balance(&asset, &nullifiers, &commitments, proof)?;
            for nf in nullifiers {
                state.mark_nullifier_spent(asset_id, nf);
            }
            for cm in commitments {
                state.append_commitment(asset_id, cm);
            }
        }
        PRECOMPILE_SHIELDED_DEPOSIT => {
            let (asset_id, from, commitment, amount) = decoded.as_shielded_deposit()?;
            let balance = state.get_owner_balance(from, asset_id)?;
            ensure!(balance >= amount, "Insufficient transparent balance");
            state.deduct_owner_balance(from, asset_id, amount)?;
            state.append_commitment(asset_id, commitment);
        }
        PRECOMPILE_SHIELDED_WITHDRAW => {
            let (asset_id, to, nullifier, proof) = decoded.as_shielded_withdraw()?;
            ensure!(!state.is_nullifier_spent(asset_id, nullifier), "Nullifier already spent");
            verify_zk_proof(proof)?;
            let amount = extract_amount_from_proof(proof)?;
            state.mark_nullifier_spent(asset_id, nullifier);
            state.credit_owner_balance(to, asset_id, amount)?;
        }
        _ => return Err(PrecompileError::NotFound),
    };

    // 5. 计算 gas 消耗
    let gas_used = calculate_precompile_gas(address, &decoded);
    ensure!(gas_used <= gas_limit, "Out of gas");

    // 6. 提交状态变更（revm 的 Journal 机制保证 EVM 状态原子性）
    // 协议状态变更通过 StateHookGuard 直接写入，失败时由快照回滚
    Ok(PrecompileOutput::new(gas_used, abi_encode(result)))
}

/// 快照回滚机制
impl ProtocolState {
    fn take_snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            balances: self.balances.clone(),
            allowances: self.allowances.clone(),
            agent_balances: self.agent_balances.clone(),
            shielded_tree: self.shielded_tree.clone(),
            nullifier_set: self.nullifier_set.clone(),
        }
    }

    fn restore_snapshot(&mut self, snapshot: StateSnapshot) {
        self.balances = snapshot.balances;
        self.allowances = snapshot.allowances;
        self.agent_balances = snapshot.agent_balances;
        self.shielded_tree = snapshot.shielded_tree;
        self.nullifier_set = snapshot.nullifier_set;
    }
}
```

**原子性保证：** 预编译执行使用两层保障：
1. **EVM 层**：revm 的 Journal 机制保证单交易原子性（EVM 状态回滚）
2. **协议层**：预编译内部保存状态快照，任何协议操作失败时回滚到快照状态

预编译失败时返回 `PrecompileError`，revm 将该错误转换为 EVM revert，整笔交易回滚（已消耗的 gas 不退还）。

**Thread-Local 状态共享：** `StateHookGuard` 使用线程本地存储（TLS）在 revm 执行期间共享协议状态引用，避免跨层状态拷贝：

```rust
/// 状态钩子守卫 — 在 EVM 交易执行期间提供协议状态访问
thread_local! {
    static PROTOCOL_STATE: RefCell<Option<Rc<RefCell<ProtocolState>>>> = const { RefCell::new(None) };
}

struct StateHookGuard;

impl StateHookGuard {
    fn install(state: Rc<RefCell<ProtocolState>>) -> Self {
        PROTOCOL_STATE.with(|s| *s.borrow_mut() = Some(state));
        StateHookGuard
    }

    fn current() -> Rc<RefCell<ProtocolState>> {
        PROTOCOL_STATE.with(|s| {
            s.borrow().as_ref().expect("StateHookGuard not installed").clone()
        })
    }
}

impl Drop for StateHookGuard {
    fn drop(&mut self) {
        PROTOCOL_STATE.with(|s| *s.borrow_mut() = None);
    }
}
```

### 3.7 预编译 Gas 模型

预编译调用使用标准 EVM gas 计费模型。每个预编译有固定的基础 gas 消耗，加上与输入数据大小相关的动态 gas。

**预编译 Gas 定价（固定 + 动态）：**

| 预编译操作 | 基础 Gas | 动态 Gas | 说明 |
|-----------|---------|---------|------|
| Transfer (0x102) | 10,000 | 16 × calldata 字节 | 单笔转账 |
| BatchTransfer (0x103) | 10,000 | 1,000 × 收款人数 | 批量转账 |
| Approve (0x104) | 5,000 | 16 × calldata 字节 | 授权 |
| TransferFrom (0x105) | 8,000 | 16 × calldata 字节 | 代授权转账 |
| Mint (0x106) | 10,000 | 16 × calldata 字节 | 发行方增发 |
| Burn (0x107) | 8,000 | 16 × calldata 字节 | 发行方销毁 |
| BridgeDeposit (0x108) | 15,000 | 16 × calldata 字节 | 协议层 → EVM 层 |
| AgentPay (0x10A) | 8,000 | 16 × calldata 字节 | Agent 代付 |
| AgentBatchPay (0x10B) | 8,000 | 800 × 收款人数 | Agent 批量支付 |
| ShieldedTransfer (0x10E) | 50,000 | 16 × calldata 字节 | 隐私转账（含 ZK 验证） |
| ShieldedDeposit (0x10D) | 20,000 | 16 × calldata 字节 | 存入 Shielded Pool |
| ShieldedWithdraw (0x10F) | 25,000 | 16 × calldata 字节 | 从 Shielded Pool 提取 |

**费用计算示例（假设 base_fee = 1 wei/gas，priority_fee = 0）：**

| 操作 | Gas 计算 | 总 Gas |
|------|---------|--------|
| 单笔转账 | 10,000 + 16 × 128 | 12,048 gas |
| 批量支付 100 人 | 10,000 + 1,000 × 100 | 110,000 gas |
| 100 笔独立转账 | 100 × 12,048 | 1,204,800 gas |
| Shielded 转账 | 50,000 + 16 × 512 | 58,192 gas |
| Agent 桥接 + 调用 | 15,000 + 16 × 128 + EVM 调用 gas | ~50,000 gas |

100 人批量支付使用预编译批量操作节省 **~91%** gas（对比 100 笔独立转账）。

**Gas 计算逻辑：**

```rust
fn calculate_precompile_gas(address: Address, input_len: usize) -> u64 {
    let base_gas = match address {
        PRECOMPILE_TRANSFER => 10_000,
        PRECOMPILE_BATCH_TRANSFER => 10_000,
        PRECOMPILE_APPROVE => 5_000,
        PRECOMPILE_TRANSFER_FROM => 8_000,
        PRECOMPILE_MINT => 10_000,
        PRECOMPILE_BURN => 8_000,
        PRECOMPILE_BRIDGE_DEPOSIT => 15_000,
        PRECOMPILE_AGENT_PAY => 8_000,
        PRECOMPILE_AGENT_BATCH_PAY => 8_000,
        PRECOMPILE_SHIELDED_TRANSFER => 50_000,
        PRECOMPILE_SHIELDED_DEPOSIT => 20_000,
        PRECOMPILE_SHIELDED_WITHDRAW => 25_000,
        _ => 0,
    };

    let dynamic_gas = (input_len as u64).saturating_mul(16);
    base_gas.saturating_add(dynamic_gas)
}
```

### 3.8 Shielded Pool（隐私屏蔽池）

Shielded Pool 提供协议级隐私转账能力，隐藏发送方、接收方和金额。基于 zk-SNARK 实现，采用 Zcash 的 Note Commitment Tree + Nullifier 模型。

#### 3.8.1 核心数据结构

```rust
/// 隐私笔记（Note）— 代表 Shielded Pool 中的一笔资产
struct Note {
    value: u128,              // 金额（加密状态）
    asset_id: AssetId,
    rcm: Scalar,              // 随机数
    recipient_view_key: PublicKey,  // 接收方视图密钥
}

/// 笔记承诺 — 放入全局 Merkle Tree
struct NoteCommitment(pub Hash);

/// 空值揭露器 — 防双花
struct Nullifier(pub Hash);

/// 视图密钥 — 选择性披露
struct ViewingKey {
    incoming_view_key: PublicKey,  // 查看进账
    full_view_key: PublicKey,      // 查看所有相关交易
}

/// ZK 证明 — 证明交易合法但不泄露细节
struct ZkProof {
    proof_data: Vec<u8>,         // Groth16 / Halo2 证明
    public_inputs: PublicInputs,  // 公开的 nullifiers + commitments
}
```

#### 3.8.2 三种操作

```
存入（ShieldedDeposit）：
  透明地址 → Shielded Pool
  1. 从发送者透明余额扣除
  2. 生成 Note 和 Commitment
  3. Commitment 加入 Merkle Tree

  外部可见：某地址存入了资产到池
  外部不可见：存入了多少、最终归属谁

隐私转账（ShieldedTransfer）：
  Shielded Pool → Shielded Pool
  1. 选择已有的 Notes 作为输入
  2. 创建新的 Notes 作为输出
  3. 生成 ZK 证明：
     - 输入的 Notes 确实存在（Merkle proof）
     - 输入的 Notes 未被花费（nullifier 未在集合中）
     - 输出总额 <= 输入总额（不造币）
     - 发送方拥有输入 Notes 的支出密钥
  4. 公开 nullifiers（标记输入已花费）
  5. 新 commitments 加入 Merkle Tree

  外部可见：有人进行了隐私转账
  外部不可见：谁转的、转给谁、转多少

提取（ShieldedWithdraw）：
  Shielded Pool → 透明地址
  1. 消费 Shielded Note
  2. 生成 ZK 证明
  3. 金额公开恢复到透明余额

  外部可见：有人从池中提取了 X 金额到地址 A
  外部不可见：提取者原始身份
```

#### 3.8.3 ZK 电路

```rust
/// ShieldedTransfer 的 ZK 电路（证明陈述）
///
/// 公共输入（公开）：
///   - nullifiers[]          （已花费的输入）
///   - commitments[]         （新输出）
///   - asset_id
///
/// 私有输入（保密）：
///   - notes[]               （被消费的 Notes）
///   - new_notes[]           （新创建的 Notes）
///   - spending_key          （发送方私钥）
///   - merkle_path[]         （Merkle 证明路径）
///
/// 约束：
///   1. 每个 note 的 nullifier 正确派生
///   2. 每个 note 在 Merkle Tree 中（路径有效）
///   3. 发送方拥有 notes 的支出权
///   4. sum(new_notes.value) <= sum(notes.value)
///   5. 所有值在有效范围内（无溢出/下溢）
```

#### 3.8.4 Merkle Tree 状态

```rust
/// 每个资产一个独立的 Merkle Tree
/// 或共享一棵树但 asset_id 编码在 Note 中
struct ShieldedState {
    merkle_tree: SparseMerkleTree<NoteCommitment>,
    nullifier_set: HashSet<Nullifier>,  // 已花费的 nullifier
    note_registry: HashMap<NoteCommitment, EncryptedNote>,  // 链上存储加密 Note
}
```

Merkle Tree 使用 **Incremental Merkle Tree**（增量默克尔树），深度 32，支持约 42 亿个叶子节点。每次新 commitment 只需 O(log n) 更新。

#### 3.8.5 合规与审计

Shielded Pool 内置合规支持，不是"不可追踪的暗网工具"：

```rust
enum ShieldedComplianceMode {
    /// 完全隐私，不强制任何披露
    Unrestricted,
    /// 接收方必须持有有效 KYC 标记
    KycRequired,
    /// 资产发行方可通过 viewing key 审计
    IssuerAuditable,
    /// 仅允许在白名单地址间进行隐私转账
    WhitelistedOnly,
}
```

**审计流程：**
1. 用户生成 viewing key 给审计方
2. 审计方用 viewing key 解密相关交易
3. 验证合规性，不暴露给公众

#### 3.8.6 证明系统选择

| 方案 | 证明大小 | 验证时间 | 可信设置 | 推荐度 |
|------|---------|---------|---------|--------|
| Groth16 | ~200B | ~3ms | 需要（per circuit） | 当前首选，性能最优 |
| Halo2 | ~1KB | ~10ms | 不需要 | 未来迁移目标 |
| Plonk | ~1KB | ~8ms | 需要（universal） | 备选 |

初始采用 **Groth16**（验证快、证明小），计划迁移到 **Halo2**（无需可信设置）。

#### 3.8.7 性能影响

| 指标 | 值 |
|------|------|
| 证明生成时间 | 1-5 秒（客户端本地） |
| 证明验证时间 | ~3ms/笔（链上） |
| 证明数据大小 | ~200 字节 |
| Nullifier 检查 | O(1) via HashSet |
| Merkle Tree 更新 | O(log n)，深度 32 |

每轮 21 个验证者子集，假设 100 笔 Shielded 交易：
- 总验证时间：100 × 3ms = 300ms
- 在 250ms 区块时间内可能成为瓶颈
- **解决方案**：每区块 Shielded 交易上限 50 笔，超出排入下一区块

### 3.9 智能账户 (Smart Accounts)

Callchain 协议层原生支持三种身份认证方案，通过 `AuthScheme` 统一抽象。身份认证（谁有权签名）与 Gas 支付（谁来付费）是**两个独立维度**，可以自由组合。所有认证方案均通过标准 EVM 交易签名或授权机制实现。

```
AuthScheme（谁签名）          Gas 支付（谁付费）
├── SingleSig                ├── SelfPay（标准 EVM gas）
├── MultiSig                 ├── 代付合约（EIP-7702）
└── SessionKey               └── 第三方代付（EIP-4337 Paymaster）
```

#### 3.9.1 AuthScheme 定义

```rust
/// 身份认证方案 — 替代传统单一签名
enum AuthScheme {
    /// 单签：标准 secp256k1 签名
    SingleSig {
        signature: Signature,
    },

    /// 多签：m-of-n 门限签名
    MultiSig {
        signatures: Vec<Signature>,
    },

    /// Session Key：临时密钥签名（用于 dApp 授权、Agent 轻量交互）
    SessionKey {
        key: Address,
        signature: Signature,
    },
}
```

#### 3.9.2 多签账户（m-of-n）

多签账户允许将账户控制权分散给多个密钥。注册多签配置后，该账户发起的每笔交易都需要至少 `threshold` 个签名者签名。

```rust
/// 多签配置 — 绑定到账户地址
struct MultiSigConfig {
    signers: Vec<Address>,          // n 个签名者
    threshold: u8,                  // 至少 m 个签名（m ≤ n）
    version: u64,                   // 版本号（用于配置轮换）
}
```

**注册与变更：**

```rust
fn register_multi_sig(
    new_address: Address,           // 新多签地址
    config: MultiSigConfig,
    // 注意：注册时需要所有 signers 签名，防止恶意注册
) -> Result<()> {
    ensure!(config.threshold <= config.signers.len() as u8,
            "Threshold exceeds signers");
    ensure!(config.threshold >= 1, "Threshold must be at least 1");
    ensure!(config.signers.len() >= 2 && config.signers.len() <= 10,
            "Signer count must be 2-10");

    // 验证所有 signer 都签署了注册同意
    for signer in &config.signers {
        ensure!(signer_signed_registration(signer, new_address),
                "Signer did not consent");
    }

    MultiSigConfigs::insert(new_address, config);
    Ok(())
}

fn update_multi_sig(
    account: Address,
    new_config: MultiSigConfig,
    // 需要旧配置的 threshold 个签名授权变更
    authorizations: Vec<Signature>,
) -> Result<()> {
    let old_config = MultiSigConfigs::get(account).ok_or("Not multi-sig")?;
    ensure!(authorizations.len() >= old_config.threshold as usize,
            "Insufficient authorizations");

    // 验证签名来自旧 signers
    let valid_count = verify_signatures_from_signers(
        &authorizations, &old_config.signers
    )?;
    ensure!(valid_count >= old_config.threshold as usize,
            "Invalid authorizations");

    MultiSigConfigs::insert(account, new_config);
    Ok(())
}
```

**验证逻辑：**

```rust
fn verify_multisig(account: Address, auth: &AuthScheme) -> Result<()> {
    let config = MultiSigConfigs::get(account)
        .ok_or("No multi-sig config")?;

    let AuthScheme::MultiSig { signatures } = auth
        else { return Err("Expected MultiSig auth"); };

    ensure!(signatures.len() >= config.threshold as usize,
            "Not enough signatures");

    // 统计有多少签名来自合法 signers
    let valid_count = signatures.iter()
        .filter(|sig| {
            let signer = recover_signer(&sig.hash());
            config.signers.contains(&signer)
        })
        .count();

    ensure!(valid_count >= config.threshold as usize,
            "Invalid threshold");

    // 额外检查：去重，防止同一 signer 多签
    ensure!(signatures.len() <= config.signers.len(),
            "Duplicate signer detected");

    Ok(())
}
```

**典型用例：**

| 场景 | 配置 | 说明 |
|------|------|------|
| 团队金库 | 3-of-5 | 5 个核心成员，任意 3 人可操作 |
| DAO 多签 | 5-of-9 | 9 个理事，5 人多数可决策 |
| 家庭账户 | 2-of-3 | 本人 + 配偶 + 律师，任意 2 人 |
| 企业审批 | 2-of-4 | CEO + CFO + 董事 + 法务，任意 2 人 |

#### 3.9.3 社交恢复钱包（Social Recovery）

用户丢失私钥时，通过预设的 Guardian（守护者）网络恢复账户控制权，无需依赖助记词备份。

```rust
/// 社交恢复配置
struct SocialRecoveryConfig {
    guardians: Vec<Address>,            // 3-10 个守护者
    threshold: u8,                      // 至少几个同意（建议 threshold = ceil(guardians/2 + 1)）
    recovery_delay_secs: u64,           // 延迟生效时间（24-72 小时）
    pending_recovery: Option<RecoveryRequest>,
}

/// 待恢复请求
struct RecoveryRequest {
    new_key: Address,                   // 新主密钥
    approved_by: Vec<Address>,          // 已同意的 Guardian
    initiated_at: u64,                  // 发起时间戳
    initiator_signature: Option<Signature>, // 旧密钥持有者签名（如果旧密钥还在）
}
```

**恢复流程：**

```
1. 发起恢复请求
   → 用户（用旧密钥或其他方式）提交 RecoveryRequest
   → 指定新密钥 new_key
   → 请求进入链上状态

2. Guardian 投票
   → Guardian 逐一签名确认
   → 每确认一个，加入 approved_by
   → 达到 threshold 个 Guardian 同意 → 进入延迟期

3. 延迟期（24-72 小时）
   → 如果用户旧密钥还在，可以单方面取消恢复
   → 防止 Guardian 串谋攻击
   → 延迟期从 threshold 达成时开始计算

4. 延迟结束 → 自动生效
   → 旧密钥失效
   → 新密钥成为账户主密钥
   → SocialRecoveryConfig 清除 pending_recovery
```

```rust
fn initiate_recovery(
    account: Address,
    new_key: Address,
    old_key_signature: Option<Signature>,  // 可选，证明是本人操作
) -> Result<()> {
    let config = SocialRecoveryConfigs::get(account)
        .ok_or("No recovery config")?;

    ensure!(config.guardians.len() >= 3, "Need at least 3 guardians");
    ensure!(new_key != Address::zero(), "Invalid new key");

    let request = RecoveryRequest {
        new_key,
        approved_by: vec![],
        initiated_at: current_timestamp(),
        initiator_signature: old_key_signature,
    };
    config.pending_recovery = Some(request);
    SocialRecoveryConfigs::insert(account, config);
    emit_event("RecoveryInitiated", account, new_key);
    Ok(())
}

fn guardian_approve(
    account: Address,
    guardian: Address,
    guardian_signature: Signature,
) -> Result<()> {
    let config = SocialRecoveryConfigs::get_mut(account)
        .ok_or("No recovery config")?;
    let request = config.pending_recovery.as_mut()
        .ok_or("No pending recovery")?;

    // 验证 guardian 身份
    ensure!(config.guardians.contains(&guardian), "Not a guardian");

    // 验证 guardian 签名
    verify_signature(&guardian, &guarder_signature, request.new_key)?;

    // 防止重复
    ensure!(!request.approved_by.contains(&guardian), "Already approved");

    request.approved_by.push(guardian);

    // 达到阈值，设置延迟生效
    if request.approved_by.len() >= config.threshold as usize {
        let effective_time = current_timestamp() + config.recovery_delay_secs;
        emit_event("RecoveryApproved", account, effective_time);
    }

    Ok(())
}

fn finalize_recovery(account: Address) -> Result<()> {
    let config = SocialRecoveryConfigs::get_mut(account)
        .ok_or("No recovery config")?;
    let request = config.pending_recovery.take()
        .ok_or("No pending recovery")?;

    // 检查 Guardian 阈值
    ensure!(request.approved_by.len() >= config.threshold as usize,
            "Insufficient guardian approvals");

    // 检查延迟期已过
    let elapsed = current_timestamp() - request.initiated_at;
    ensure!(elapsed >= config.recovery_delay_secs,
            "Recovery delay not met");

    // 替换账户主密钥
    AccountKeys::insert(account, request.new_key);

    // 清除 pending 状态
    config.pending_recovery = None;
    SocialRecoveryConfigs::insert(account, config);

    emit_event("RecoveryFinalized", account, request.new_key);
    Ok(())
}

fn cancel_recovery(account: Address, owner_signature: Signature) -> Result<()> {
    // 账户所有者可以在延迟期内随时取消
    verify_signature(&account, &owner_signature, "cancel_recovery")?;

    let config = SocialRecoveryConfigs::get_mut(account)
        .ok_or("No recovery config")?;
    config.pending_recovery = None;
    SocialRecoveryConfigs::insert(account, config);

    emit_event("RecoveryCancelled", account);
    Ok(())
}
```

**Guardian 类型建议：**

| Guardian 类型 | 示例 | 推荐数量 |
|--------------|------|---------|
| 自有设备 | 备用手机、硬件钱包、笔记本电脑 | 1-2 |
| 信任的人 | 配偶、家人、密友 | 1-2 |
| 专业机构 | 律师事务所、银行、托管服务 | 0-1 |
| 时间锁 | 基于时间的自动恢复（备选） | 0-1 |

推荐配置：5 个 Guardian，阈值 3，延迟 48 小时。

#### 3.9.4 Session Key（会话密钥）

Session Key 是临时密钥，用于授权 dApp 或 Agent 在限定范围内操作，无需用户每次签名。

```rust
/// Session Key 配置
struct SessionKeyConfig {
    key: Address,                       // Session Key 公钥地址
    permissions: SessionPermissions,
    expires_at: u64,                    // 过期时间戳
    created_at: u64,
}

/// Session Key 权限
struct SessionPermissions {
    /// 允许的预编译地址（空 = 所有预编译）
    allowed_precompiles: Vec<Address>,
    /// 单笔最大金额（0 = 无限制）
    max_per_tx: u128,
    /// 日累计金额上限（0 = 无限制）
    max_daily: u128,
    /// 允许交互的目标地址（空 = 任意）
    allowed_targets: Vec<Address>,
    /// 允许操作的资产（空 = 全部）
    allowed_assets: Vec<AssetId>,
}
```

**创建与撤销：**

```rust
fn create_session_key(
    account: Address,
    session_key: Address,
    permissions: SessionPermissions,
    duration_secs: u64,
    signature: Signature,           // 账户所有者签名
) -> Result<()> {
    verify_signature(&account, &signature, session_key)?;

    let config = SessionKeyConfig {
        key: session_key,
        permissions,
        expires_at: current_timestamp() + duration_secs,
        created_at: current_timestamp(),
    };
    SessionKeys::insert(account, session_key, config);
    Ok(())
}

fn revoke_session_key(
    account: Address,
    session_key: Address,
    signature: Signature,
) -> Result<()> {
    verify_signature(&account, &signature, session_key)?;
    SessionKeys::remove(account, session_key);
    Ok(())
}
```

**验证逻辑：**

```rust
fn verify_session_key(account: Address, auth: &AuthScheme) -> Result<()> {
    let AuthScheme::SessionKey { key, signature } = auth
        else { return Err("Expected SessionKey auth"); };

    // 1. 验证 Session Key 签名
    verify_signature(key, signature, &tx_hash)?;

    // 2. 获取配置
    let config = SessionKeys::get(account, *key)
        .ok_or("Session key not found")?;

    // 3. 验证未过期
    ensure!(current_timestamp() < config.expires_at,
            "Session key expired");

    // 4. 验证权限
    let perms = &config.permissions;
    if !perms.allowed_precompiles.is_empty() {
        let precompile_calls = decode_precompile_calls(&tx.data)?;
        for call in &precompile_calls {
            ensure!(perms.allowed_precompiles.contains(&call.precompile_address),
                    "Precompile not allowed");
        }
    }
    if perms.max_per_tx > 0 {
        let total_amount = decode_precompile_calls(&tx.data)?
            .iter()
            .filter_map(|c| c.amount())
            .max()
            .unwrap_or(0);
        ensure!(total_amount <= perms.max_per_tx,
                "Exceeds per-tx limit");
    }
    if perms.max_daily > 0 {
        let today = current_timestamp() / 86400;
        let daily_spent = SessionKeyDailyUsage::get(account, *key, today);
        let tx_amount = decode_precompile_calls(&tx.data)?
            .iter()
            .filter_map(|c| c.amount())
            .sum::<u128>();
        ensure!(daily_spent + tx_amount <= perms.max_daily,
                "Exceeds daily limit");
        SessionKeyDailyUsage::insert(account, *key, today, daily_spent + tx_amount);
    }
    if !perms.allowed_targets.is_empty() {
        if let Some(to) = tx.to {
            ensure!(perms.allowed_targets.contains(&to),
                    "Target not allowed");
        }
    }
    if !perms.allowed_assets.is_empty() {
        for asset_id in decode_precompile_calls(&tx.data)?
            .iter()
            .filter_map(|c| c.asset_id()) {
            ensure!(perms.allowed_assets.contains(&asset_id),
                    "Asset not allowed");
        }
    }

    Ok(())
}
```

**典型用例：**

| 场景 | 权限设置 | 有效期 |
|------|---------|--------|
| dApp 游戏 | 仅 Transfer 预编译，单笔 < 10 CALL，日累计 < 100 CALL | 24 小时 |
| Agent 自动支付 | AgentPay + AgentCall 预编译，单笔 < 50 USDC | 7 天 |
| DeFi 策略机器人 | 仅 Approve + TransferFrom 预编译，目标 = 指定 DEX | 1 小时 |
| 钱包预览模式 | 仅 view 操作（无需签名） | 永久 |

#### 3.9.5 统一认证流程

```rust
/// 协议层身份认证入口 — 替代原有单一签名验证
fn verify_auth(account: Address, auth: &AuthScheme) -> Result<()> {
    match auth {
        AuthScheme::SingleSig { signature } => {
            verify_single_sig(account, signature)?;
        }
        AuthScheme::MultiSig { signatures } => {
            verify_multisig(account, signatures)?;
        }
        AuthScheme::SessionKey { key, signature } => {
            verify_session_key(account, auth)?;
        }
    }
    Ok(())
}

fn verify_single_sig(account: Address, signature: &Signature) -> Result<()> {
    // 检查是否有社交恢复的待生效请求
    if let Some(config) = SocialRecoveryConfigs::get(account) {
        if let Some(request) = &config.pending_recovery {
            if request.approved_by.len() >= config.threshold as usize {
                let elapsed = current_timestamp() - request.initiated_at;
                if elapsed >= config.recovery_delay_secs {
                    // 恢复已生效，旧密钥不再有效
                    return Err("Account key has been recovered");
                }
            }
        }
    }

    verify_signature(&account, signature, &tx_hash)?;
    Ok(())
}
```

#### 3.9.6 存储结构

```
callchain/
├── accounts/
│   ├── {address}/
│   │   ├── key                     # 主密钥（SingleSig）
│   │   ├── multi_sig_config        # 多签配置（如果设置）
│   │   ├── social_recovery_config  # 社交恢复配置（如果设置）
│   │   └── session_keys/           # Session Keys（0-N 个）
│   │       └── {session_key}/      # 每个 Session Key
│   │           ├── config          # 权限 + 过期时间
│   │           └── daily_usage     # 日累计使用量
└── ...
```

存储估算：
- 多签账户：~200 字节（10 个 signer × 20 字节 + 元数据）
- 社交恢复：~300 字节（5 个 Guardian + 待恢复状态）
- Session Key：~100 字节/个（权限 + 过期时间）

#### 3.9.7 与 Agent 模型的协同

Session Key 和 Agent 账户是不同抽象层：

| 特性 | Session Key | Agent 账户 |
|------|------------|-----------|
| 生命周期 | 临时（小时/天） | 长期（月/年） |
| 权限粒度 | 指令级别、金额限制 | 资产级别、对手方限制 |
| 资金归属 | 用户主账户 | 独立子账户（从 Owner 划拨） |
| 费用支付 | 通常 SelfPay | 通常 AuthorizedSponsor（Owner 代付） |
| 适用场景 | dApp 交互、临时授权 | AI Agent 长期自动支付 |

可以组合使用：Agent 交易本身可以用 Session Key 发起（短期授权），同时由 Owner 代付 Gas。

---

## 4. EVM 智能合约层

### 4.1 执行引擎

基于 **Reth + Revm**，作为库嵌入到同一进程中。

- 完全兼容以太坊 EVM
- 支持所有标准 Ethereum JSON-RPC 方法
- 兼容 Foundry / Hardhat 开发工具链

### 4.2 资产对应的 ERC-20 合约

每个注册的协议资产在 EVM 层都有对应的 ERC-20 合约。该合约维护**独立的 EVM 层余额**，与协议层余额通过内部桥接转换。

```solidity
/// 协议资产对应的 ERC-20 合约
/// 维护独立的 EVM 层余额，通过桥接与协议层转换
contract AssetToken is IERC20 {
    uint8 public immutable decimals;
    string public name;
    string public symbol;
    uint256 public totalSupply;

    // EVM 层独立余额
    mapping(address => uint256) private _balances;
    mapping(address => mapping(address => uint256)) private _allowances;

    // 只有桥接合约可以铸造/销毁
    modifier onlyBridge() {
        require(msg.sender == BRIDGE_CONTRACT, "Only bridge");
        _;
    }

    function balanceOf(address account) external view returns (uint256) {
        return _balances[account];
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _balances[msg.sender] -= amount;
        _balances[to] += amount;
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        _allowances[msg.sender][spender] = amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        _allowances[from][msg.sender] -= amount;
        _balances[from] -= amount;
        _balances[to] += amount;
        return true;
    }

    // 桥接接口
    function bridgeMint(address to, uint256 amount) external onlyBridge {
        _balances[to] += amount;
        totalSupply += amount;
    }

    function bridgeBurn(address from, uint256 amount) external onlyBridge {
        _balances[from] -= amount;
        totalSupply -= amount;
    }
}
```

### 4.3 独立 ERC-20 合约

除协议资产对应的 ERC-20 合约外，EVM 层还支持：
- 任意标准 ERC-20 合约部署
- 非协议资产的代币（meme coin、治理代币等）
- 标准 DeFi 合约（DEX、Lending、...）无需任何适配
- EVM gas 使用动态定价

**关键：EVM 层是一个功能完整的以太坊链，协议资产通过标准 ERC-20 接口参与 DeFi。**

---

## 5. 内部桥接

### 5.1 设计

协议层余额和 EVM 层余额是**两份独立的账本**，通过内部桥接合约进行转换。

```
协议层                    EVM 层
─────────                ─────────
Asset: USDX              ERC-20: USDX
balances[A] = 100        balanceOf(A) = 0
                         (独立余额)

A 桥接到 EVM 层：
  balances[A] = 0        balanceOf(A) = 100
                         (锁仓释放)

A 桥回协议层：
  balances[A] = 100      balanceOf(A) = 0
```

### 5.2 桥接操作

```rust
enum BridgeOp {
    /// 协议层 → EVM 层
    /// 从协议余额中扣除，在 EVM 层铸造
    DepositToEvm {
        asset_id: AssetId,
        from: Address,
        to: Address,
        amount: u128,
    },
    /// EVM 层 → 协议层
    /// 在 EVM 层销毁，恢复到协议余额
    WithdrawToProtocol {
        asset_id: AssetId,
        from: Address,
        to: Address,
        amount: u128,
    },
}
```

### 5.3 桥接执行

**Protocol → EVM（Deposit）：**

```rust
fn execute_deposit(op: &BridgeOp) -> Result<()> {
    let asset = get_asset(op.asset_id)?;

    // 1. 协议层扣除余额
    let balance = protocol_balances
        .get_mut(&op.asset_id)
        .ok_or(AssetNotFound)?;
    let user_balance = balance.get_mut(&op.from).ok_or(InsufficientBalance)?;
    ensure!(*user_balance >= op.amount, InsufficientBalance);
    *user_balance -= op.amount;

    // 2. EVM 层铸造
    evm_call(
        asset.evm_contract,
        encode("bridgeMint(address,uint256)", op.to, op.amount),
    )?;

    Ok(())
}
```

**EVM → Protocol（Withdraw）：**

```rust
fn execute_withdraw(op: &BridgeOp) -> Result<()> {
    let asset = get_asset(op.asset_id)?;

    // 1. EVM 层销毁
    evm_call(
        asset.evm_contract,
        encode("bridgeBurn(address,uint256)", op.from, op.amount),
    )?;

    // 2. 协议层增加余额
    let balance = protocol_balances
        .entry(op.asset_id)
        .or_default();
    *balance.entry(op.to).or_insert(0) += op.amount;

    Ok(())
}
```

### 5.4 桥接时机

```
每个区块的执行顺序中，桥接操作在第二步执行：

1. EVM 交易执行
   → 用户可能在 EVM 层触发 bridge_withdraw
   → 这些请求被加入待处理桥接队列
   → 协议操作通过预编译调用在同一交易中完成

2. 桥接操作执行
   → 处理待处理队列中的桥接请求
   → 保证在同一区块内完成

3. 系统交易执行
```

这意味着 **Protocol → EVM 和 EVM → Protocol 的转换在同一个区块内完成**，无需等待。

### 5.5 用户体验

```
场景：用户 A 想参与 EVM 层 DeFi

钱包自动处理：
  1. 检测到 A 的 USDX 在协议层
  2. 用户发起 swap 操作
  3. 钱包自动附加 bridge_deposit 操作
  4. 同一个交易中完成：桥接 → swap
  5. 用户无需感知"我在哪一层"

前端展示统一余额：
  Total USDX: 100
  ├── Protocol: 30 (快速支付可用)
  └── EVM: 70 (DeFi 可用)
```

---

## 5.6 外部跨链桥接（External Bridge）

Callchain 通过**验证者共识子集签名**实现跨链桥接，不依赖第三方桥或 BLS 聚合签名。验证者使用已有的 secp256k1 共识密钥，利用每轮 21 个验证者子集中的 2/3（即 14 个签名）完成桥接验证。

### 5.6.1 信任模型

```
每轮 21 个验证者（从 100-216 中随机抽取）
    │
    ├── 2/3 阈值 = 14 个签名
    ├── 验证者使用 secp256k1 密钥签名
    ├── 签名在 Callchain 协议层验证（存款方向）
    └── 签名在以太坊合约验证（提款方向）

安全性：
  桥接安全 ≤ 共识安全
  如果 14/21 验证者串谋 → 链本身也不安全
```

### 5.6.2 桥接数据结构

```rust
/// 外部链标识
enum ExternalChain {
    EthereumMainnet,     // 以太坊主网
    Arbitrum,            // Arbitrum
    // 可扩展
}

/// 外部桥接操作
enum ExternalBridgeOp {
    /// 外部链 → Callchain（存款）
    Deposit {
        source_chain: ExternalChain,
        source_tx_hash: Hash,
        source_block_number: u64,
        sender: Vec<u8>,              // 外部链地址（原始字节）
        recipient: Address,           // Callchain 接收地址
        asset_id: AssetId,
        amount: u128,
        signatures: Vec<Signature>,   // 14+ 个验证者签名
    },
    /// Callchain → 外部链（提款）
    Withdraw {
        target_chain: ExternalChain,
        target_address: Vec<u8>,      // 外部链接收地址
        asset_id: AssetId,
        sender: Address,
        amount: u128,
    },
}
```

### 5.6.3 存款流程（外部链 → Callchain）

```
用户在以太坊操作：
  1. 调用以太坊桥接合约 deposit(assetId, amount, callchainRecipient)
  2. 以太坊合约锁定资产，触发 DepositInitiated 事件

Callchain 验证者操作：
  3. 每个验证者运行桥接监听进程，检测以太坊 DepositInitiated 事件
  4. 等待 min_confirmations 个区块确认（以太坊默认 12 个）
  5. 确认后，每个验证者用 secp256k1 私钥对 deposit_hash 签名
  6. 签名通过 P2P 网络传播，收集到 14 个签名后聚合
  7. 任意节点提交 ExternalBridgeDeposit 交易到 Callchain
     （包含 14 个签名 + deposit 数据）
  8. Callchain 协议层验证 14 个签名，验证通过后铸造对应资产
```

### 5.6.4 提款流程（Callchain → 外部链）

```
用户在 Callchain 操作：
  1. 发起 ExternalBridgeWithdraw 指令
  2. Callchain 销毁对应资产
  3. 提款请求加入待处理队列

验证者操作：
  4. 验证者区块打包时收集提款请求
  5. 每个验证者对提款数据签名
  6. 收集到 14 个签名后，聚合提交到以太坊桥接合约
  7. 以太坊合约验证 14 个签名，验证通过后释放资产
```

### 5.6.5 桥接签名验证

```rust
/// 验证桥接签名集合
fn verify_bridge_signatures(
    message_hash: Hash,
    signatures: &[Signature],
    min_signatures: u8,
) -> Result<()> {
    ensure!(signatures.len() >= min_signatures as usize,
            "Insufficient bridge signatures");

    // 获取当前活跃验证者
    let validators = get_active_validators();
    let required = validators.len() * 2 / 3 + 1;  // 2/3 + 1

    let mut unique_validators = HashSet::new();

    for sig in signatures {
        let signer = recover_secp256k1_signer(message_hash, sig)?;
        if validators.contains(&signer) {
            unique_validators.insert(signer);
        }
    }

    ensure!(unique_validators.len() >= required,
            "Not enough valid validator signatures");

    Ok(())
}

/// 验证者桥接签名服务
fn sign_bridge_event(
    private_key: &SecretKey,
    event: &BridgeEvent,
) -> Signature {
    let deposit_hash = keccak256(&abi_encode(
        event.source_chain,
        event.source_tx_hash,
        event.asset_id,
        event.sender,
        event.recipient,
        event.amount,
    ));
    sign_secp256k1(private_key, &deposit_hash)
}
```

### 5.6.6 以太坊端桥接合约

```solidity
contract CallchainBridge {
    /// 验证者公钥注册
    mapping(address => bool) public validators;
    uint256 public validatorCount;

    /// 存款：用户从以太坊存入到 Callchain
    function deposit(
        uint256 assetId,
        uint256 amount,
        bytes32 callchainRecipient
    ) external {
        require(amount > 0, "Invalid amount");
        IERC20(assetId).transferFrom(msg.sender, address(this), amount);
        emit DepositInitiated(assetId, amount, callchainRecipient, msg.sender);
    }

    /// 提款：从 Callchain 提取到以太坊
    /// 需要 14+ 个验证者签名
    function withdraw(
        uint256 assetId,
        uint256 amount,
        address recipient,
        bytes32 sourceTxHash,
        bytes[] calldata signatures  // 14+ 个 secp256k1 签名
    ) external {
        // 1. 防重放
        require(!processedWithdraws[sourceTxHash], "Already processed");
        processedWithdraws[sourceTxHash] = true;

        // 2. 构建签名消息
        bytes32 messageHash = keccak256(abi.encode(
            assetId, amount, recipient, sourceTxHash, address(this)
        ));

        // 3. 验证签名
        uint256 validCount;
        for (uint256 i = 0; i < signatures.length; i++) {
            address signer = recoverSigner(messageHash, signatures[i]);
            require(validators[signer], "Invalid validator signature");
            validCount++;
        }
        require(validCount >= getQuorum(), "Insufficient signatures");

        // 4. 释放资产
        IERC20(assetId).transfer(recipient, amount);
    }

    function getQuorum() public view returns (uint256) {
        return (validatorCount * 2) / 3 + 1;
    }
}
```

### 5.6.7 桥接安全限制

```rust
struct BridgeConfig {
    /// 单笔最大桥接金额
    max_per_tx: u128,

    /// 每日总桥接上限（按资产）
    daily_limit_per_asset: u128,

    /// 以太坊最小确认数
    eth_min_confirmations: u32,       // 默认 12

    /// 桥接费用（覆盖以太坊 gas 成本）
    bridge_fee: u128,

    /// 白名单资产（仅允许注册的外部资产桥接）
    allowed_assets: Vec<AssetId>,

    /// 签名收集超时（防止桥接卡住）
    signature_timeout_secs: u64,      // 默认 300 秒
}
```

| 参数 | 默认值 | 说明 |
|------|--------|------|
| max_per_tx | 1,000,000 USDC | 防鲸鱼攻击 |
| daily_limit_per_asset | 10,000,000 USDC | 防大额资金流动冲击 |
| eth_min_confirmations | 12 | 以太坊标准确认数 |
| signature_timeout_secs | 300 | 签名收集超时，自动重试 |

---

## 6. Agent 支付 (Agent Payments)

### 6.1 Agent 账户模型

Agent 账户不独立存余额，而是从 owner 账户授权划拨。Owner 将资金存入 Agent 子账户，Agent 只能在权限范围内使用。

```rust
struct AgentRegistration {
    agent_id: u64,                    // 协议分配的唯一 Agent ID
    owner: Address,                   // 资金所有者（Owner）
    agent_public_key: PublicKey,      // Agent 操作密钥（secp256k1）
    name: String,                     // 显示名称
    url: Option<String>,              // 公开信息 URL
    metadata_hash: Option<Hash>,      // Agent 代码/配置哈希
    domain_proof: Option<DomainProof>, // 域名所有权证明（可选）
    registered_at: u64,
}

enum DomainProof {
    /// 在指定域名的 .well-known/callchain-agent 放置验证文件
    DnsTxt { domain: String, txt_value: String },
    /// 通过 HTTP 访问域名的验证端点
    HttpFile { url: String, expected_content: String },
}
```

### 6.2 Agent 权限与费用配置

```rust
struct AgentPermissions {
    allowed_assets: Vec<AssetId>,    // 允许操作的资产列表（空=全部）
    daily_limit: u128,               // 日支出上限（0=无限制）
    per_tx_limit: u128,             // 单笔上限（0=无限制）
    allowed_counterparties: Vec<Address>, // 白名单（空=任意）
    allowed_protocols: Vec<Address>,     // 允许交互的 EVM 合约（空=任意）
    expires_at: u64,                     // 过期时间戳（0=永不过期）
}

struct AgentFeeConfig {
    fee_payer: FeePayer,
    owner_max_daily_fee: u128,      // Owner 每天最多替 Agent 付多少
    owner_max_total_fee: u128,       // Owner 累计最多替 Agent 付多少
    require_owner_signature_above: u128,  // 超过此金额需要 Owner 二次签名
}

enum FeePayer {
    /// Agent 子账户余额支付费用
    SelfPay,
    /// Owner 主账户代付费用
    OwnerPays,
    /// 第三方代付（平台/协议补贴）
    ThirdParty { payer: Address },
}
```

**Owner 代付是 Agent 支付的推荐模式。** Agent 只提交操作指令（调用预编译地址），Gas 通过 EIP-7702 授权或代付合约从 Owner 账户扣除。Owner 一次签名授权，Agent 后续交易无需 Owner 再签名。Agent 可在单笔 EVM 交易中通过 multicall 组合多个预编译调用。

### 6.3 Agent 资金授权

```rust
enum AgentFundingAction {
    /// Owner 创建并授权 Agent
    Grant {
        owner: Address,
        agent_public_key: PublicKey,
        name: String,
        url: Option<String>,
        domain_proof: Option<DomainProof>,
        amount: u128,                // 初始授权金额
        asset_id: AssetId,
        permissions: AgentPermissions,
        fee_config: AgentFeeConfig,
    },
    /// 追加资金
    TopUp {
        agent_id: u64,
        amount: u128,
        asset_id: AssetId,
    },
    /// 立即撤销，剩余资金返回 Owner
    Revoke {
        agent_id: u64,
    },
    /// 更新权限或费用配置
    UpdateConfig {
        agent_id: u64,
        new_permissions: Option<AgentPermissions>,
        new_fee_config: Option<AgentFeeConfig>,
    },
}
```

**Revoke 和 UpdateConfig 由 Owner 直接发起，不需要 Agent 配合，立即生效。**

### 6.4 Agent 余额管理

```rust
/// Agent 子账户余额
/// (owner_address, agent_id, asset_id) → balance
type AgentBalances = HashMap<(Address, u64, AssetId), u128>;

/// Agent nonce（防重放）
/// (owner_address, agent_id) → nonce
type AgentNonces = HashMap<(Address, u64), u64>;
```

### 6.5 Agent 指令

Agent 操作通过调用预编译地址实现：

```solidity
// Agent 相关预编译地址：
// 0x10A — AgentPay
// 0x10B — AgentBatchPay
// 0x10C — AgentCall
// 0x10D — AgentBridgeDeposit
//
// Agent 可以在单笔 EVM 交易中组合多个预编译调用（通过合约或 multicall）：
//
// bytes memory bridgeData = abi.encode(agent_id, asset_id, to, amount);
// address(PRECOMPILE_AGENT_BRIDGE_DEPOSIT).call(bridgeData);
//
// bytes memory callData = abi.encode(agent_id, asset_id, contract, data, value);
// address(PRECOMPILE_AGENT_CALL).call(callData);
```

Agent 交易签名与封装：

### 6.6 Agent 交易签名与验证

```rust
struct SignedAgentTx {
    evm_tx: EvmTx,                    // 标准 EVM 交易（调用预编译）
    owner_signature: Option<Signature>,  // 大额交易需要 Owner 二次确认
}
```

**预编译层验证流程：**

```rust
fn verify_agent_tx(tx: &SignedAgentTx) -> Result<()> {
    let agent = get_agent_from_evm_tx(&tx.evm_tx)?;

    // 1. 验证 Agent 签名（EVM 交易签名）
    ensure!(
        recover_evm_signer(&tx.evm_tx) == agent.agent_public_key.to_address(),
        "Invalid agent signature"
    );

    // 2. 验证 nonce 防重放（使用 EVM nonce）
    let current_nonce = get_agent_nonce(agent.owner, agent.agent_id);
    ensure!(tx.evm_tx.nonce == current_nonce, "Invalid nonce");

    // 3. 验证预编译调用的权限
    let precompile_calls = decode_precompile_calls(&tx.evm_tx.data)?;
    for call in &precompile_calls {
        let perms = &agent.permissions;
        if let Some(asset_id) = call.asset_id() {
            ensure!(perms.is_allowed(asset_id), "Asset not allowed");
        }
        if let Some(counterparty) = call.counterparty() {
            ensure!(perms.is_allowed_counterparty(counterparty), "Counterparty not allowed");
        }
        if let Some(amount) = call.amount() {
            if perms.per_tx_limit > 0 {
                ensure!(amount <= perms.per_tx_limit, "Exceeds per-tx limit");
            }
        }
    }

    // 4. 验证 Agent 未过期
    if agent.permissions.expires_at > 0 {
        ensure!(current_timestamp() < agent.permissions.expires_at, "Agent expired");
    }

    // 5. 大额交易需要 Owner 二次签名
    let total_amount = precompile_calls.iter().filter_map(|c| c.amount()).sum::<u128>();
    if let Some(threshold) = agent.fee_config.require_owner_signature_above {
        if total_amount > threshold {
            ensure!(
                tx.owner_signature.is_some()
                    && tx.owner_signature.unwrap().verify(&agent.owner, &tx.evm_tx.hash()),
                "Owner signature required"
            );
        }
    }

    Ok(())
}
```

### 6.7 Agent 交易执行与费用处理

Agent 交易通过 EVM 预编译执行引擎处理：

```rust
fn execute_agent_tx(tx: &SignedAgentTx) -> Result<()> {
    let agent = get_agent_from_evm_tx(&tx.evm_tx)?;

    verify_agent_tx(tx)?;

    // EVM gas 已由 revm 在执行期间扣除
    // 预编译内部通过 StateHookGuard 访问协议状态

    // 提取预编译调用并执行（原子性由 revm Journal 保证）
    let precompile_calls = decode_precompile_calls(&tx.evm_tx.data)?;
    for call in &precompile_calls {
        execute_precompile_call(call, &agent.address)?;
    }

    // 更新 nonce
    increment_agent_nonce(agent.owner, agent.agent_id);

    Ok(())
}
```

### 6.8 Agent 费用模型

Agent 支付享受专属 Gas 折扣（详见 §12.2 费用表），所有 Agent 预编译调用的基础费用为普通用户的 50%。Agent 的 Gas 通过标准 EVM gas 模型支付，通常使用代付合约或 EIP-7702 授权由 Owner 代付，Agent 本身无需持有 CALL。

### 6.9 Agent 身份验证层级

Agent 身份验证分三层：

| 层级 | 内容 | 保证 |
|------|------|------|
| 1. 密码学认证 | Agent 密钥签名 + nonce 防重放 | "是 Agent 自己在操作" |
| 2. 注册声明 | 名称 + 域名验证 + 代码哈希 | "Agent 是谁运营的" |
| 3. 信任验证 | 行为历史 + 审计证明 + 社区声誉 | "我能信它吗" |

协议层强制第 1 层，提供第 2 层的基础设施，第 3 层交给生态（审计机构、钱包 UI、社区）。

---

## 7. 发行方管理

### 7.1 发行方权限

发行方对其注册的资产拥有以下权限：

```rust
enum IssuerAction {
    /// 增发（受合规策略约束）
    Mint { to: Address, amount: u128 },
    /// 销毁
    Burn { from: Address, amount: u128 },
    /// 冻结特定地址
    FreezeAddress { target: Address },
    /// 解冻特定地址
    UnfreezeAddress { target: Address },
    /// 更新合规策略
    UpdatePolicy { new_policy: CompliancePolicy },
    /// 转移发行权限
    TransferOwnership { new_issuer: Address },
}
```

### 7.2 限制

发行方**不能**：
- 修改其他发行方的资产
- 绕过协议层合规策略
- 改变协议支付的费用模型
- 修改桥接规则
- 直接修改用户余额（只能通过 mint/burn）

---

## 8. 网络层

### 8.1 协议

使用 **commonware-p2p** 作为 P2P 网络层，与 Simplex 共识（commonware-consensus）无缝集成。

- 共识消息：gossipsub 低延迟模式
- 交易传播：gossipsub
- 请求-响应：commonware-p2p 请求-响应模式

### 8.2 交易类型传播

```
EvmTx               → gossipsub, 标准优先级
BridgeOp            → 打包在区块中，不单独传播
SystemTx            → 仅验证者生成
```

---

## 9. 序列化格式

Callchain 使用**统一 RLP 序列化**，P2P 传播、区块编码、存储层编码均使用同一种格式，避免多格式转换的复杂度。

### 9.1 P2P 网络与区块编码（RLP）

P2P 消息传播、区块/交易编码使用 **alloy-rlp**（RLP，Recursive Length Prefix），与以太坊生态一致。

```rust
use alloy_rlp::{RlpEncodable, RlpDecodable};

/// P2P 网络消息
struct NetworkMessage {
    data: Vec<u8>,  // RLP 编码的交易或区块
    checksum: u32,
}

impl alloy_rlp::Encodable for EvmTx { ... }
impl alloy_rlp::Decodable for EvmTx { ... }

impl alloy_rlp::Encodable for Block { ... }
impl alloy_rlp::Decodable for Block { ... }
```

**为什么选 RLP 而非 Borsh/SCALE/rkyv：**
| 维度 | RLP | Borsh | SCALE | rkyv |
|------|-----|-------|-------|------|
| 以太坊兼容 | ✅ 原生 | ❌ 需转换 | ❌ 需转换 | ❌ 需转换 |
| 工具生态 | 丰富（alloy-rs） | 中等 | 仅 Polkadot | 小 |
| 存储读写性能 | 与 Reth 原生一致 | 需要适配层 | 需要适配层 | 零拷贝读，写慢 |
| 与 Reth 集成 | ✅ 零适配 | 需要适配层 | 需要适配层 | 需要适配层 |

### 9.2 JSON-RPC 与配置（Serde JSON）

RPC 接口、配置文件、Genesis 使用 **serde** 序列化。

```rust
use serde::{Serialize, Deserialize};

/// Genesis 配置（TOML/JSON 解析）
#[derive(Serialize, Deserialize)]
struct GenesisConfig {
    chain_id: u64,
    initial_validators: Vec<ValidatorInfo>,
    initial_allocations: Vec<Allocation>,
}

/// RPC 响应
#[derive(Serialize, Deserialize)]
struct RpcResponse<T> {
    jsonrpc: String,
    result: Option<T>,
    error: Option<RpcError>,
    id: u64,
}
```

### 9.3 存储层编码

存储层（reth-db / MDBX）使用 RLP 编码，与协议层保持一致：

```rust
/// 存储键值对使用 RLP 编码
/// 键：固定长度前缀 + 变量部分
/// 值：RLP 编码的结构体

trait StorageCodec: Sized {
    fn encode_to_buf(&self, buf: &mut Vec<u8>);
    fn decode_from_buf(buf: &[u8]) -> Result<Self>;
}
```

### 9.4 各类型序列化策略

| 数据类型 | P2P 传播 | 存储 | RPC 输出 |
|----------|---------|------|---------|
| EvmTx | RLP | StorageCodec | JSON |
| SignedAgentTx | RLP | StorageCodec | JSON |
| Block | RLP | StorageCodec | JSON |
| ShieldedProof | RLP | 压缩二进制 | JSON（base64） |
| StateSnapshot | RLP | StorageCodec | JSON |
| ZkProof | RLP | 压缩二进制 | JSON（base64） |

### 9.5 地址格式

```
地址 = 20 字节（160 位），hex 编码，0x 前缀

示例：0x742d35Cc6634C0532925a3b844Bc9e7595f2bD18

编码规则：
  - 内部表示：[u8; 20]
  - 显示格式：0x + 40 字符 hex（小写）
  - 校验：可选 EIP-55 混合大小写校验和
  - 序列化：RLP 编码为 20 字节原始值，JSON 编码为 hex 字符串
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable)]
#[repr(transparent)]
pub struct Address([u8; 20]);

impl Address {
    pub fn zero() -> Self { Self([0; 20]) }
    pub fn to_checksum(&self) -> String { /* EIP-55 */ }
}

impl Serialize for Address { /* JSON: hex string */ }
impl<'de> Deserialize<'de> for Address { /* JSON: from hex string */ }
```

### 9.6 交易序列化示例

```rust
// RLP 编码（P2P 传播）
let tx = EvmTx {
    nonce: 42,
    gas_price: 10_000_000_000,
    gas_limit: 21_000,
    to: Some(PRECOMPILE_TRANSFER),  // 0x102
    value: U256::ZERO,
    data: Bytes::from(abi_encode_transfer(USDC_ID, recipient, amount)),
    signature: sig,
};
let rlp_bytes = alloy_rlp::encode(&tx);  // → Vec<u8>
let decoded = EvmTx::decode(&mut &rlp_bytes[..])?;

// JSON 编码（RPC 输出）
let json = serde_json::to_string(&tx)?;
// → {"nonce":"0x2a","gasPrice":"0x2540be400","to":"0x000...0102",...}
```

---

## 10. 存储层

### 10.1 引擎

使用 **reth-db (MDBX)** 作为持久化存储引擎。

### 10.2 状态结构

```
callchain/
├── protocol/
│   ├── assets/{asset_id}/          # 资产元数据
│   ├── balances/{asset_id}/{addr}  # 协议层余额
│   └── allowances/{asset_id}/{owner}/{spender}  # 允许度
├── shielded/
│   ├── merkle_tree/{asset_id}/     # 每个资产的 Merkle Tree
│   ├── nullifiers/{asset_id}/{nf}  # 已花费的 nullifier（防双花）
│   ├── commitments/{asset_id}/{cm} # 笔记承诺（加密 Note）
│   └── viewing_keys/{addr}/        # 用户视图密钥映射
├── agent/
│   ├── registrations/{agent_id}/   # Agent 注册信息
│   ├── balances/{owner}/{agent_id}/{asset_id}  # Agent 子账户余额
│   └── nonces/{owner}/{agent_id}   # Agent nonce
├── evm/
│   ├── accounts/{addr}/            # EVM 账户
│   ├── contracts/{addr}/           # 合约代码
│   └── storage/{addr}/{slot}       # 合约存储
├── bridge/
│   └── pending_ops/                # 待处理桥接操作
├── consensus/
│   ├── blocks/{height}             # 区块数据
│   └── state/{height}              # 状态快照
└── metadata/
    ├── chain_id                    # 链 ID
    ├── validators                  # 当前验证者集
    ├── compliance                  # 合规策略注册表
    └── agents/                     # Agent 注册表
        └── {agent_id}/             # Agent 身份与权限
```

### 10.3 数据 Prune 策略

区块链节点数据会随时间无限增长。Callchain 采用**分层 prune 策略**：保留当前状态 + 最近 N 个区块，历史中间状态可安全丢弃。

#### 10.3.1 数据分类

```
节点数据 = 当前状态 + 历史区块 + 中间状态 + 索引 + 归档数据

必须保留（不可 prune）：
  ✓ 当前协议层余额
  ✓ 当前 EVM 状态
  ✓ 当前 Shielded Pool 状态（nullifier 集合、Merkle 根）
  ✓ 当前 Agent 注册与余额
  ✓ 区块头（用于链验证）
  ✓ 最近 N 个区块的完整数据

可以 prune：
  ✗ 历史状态的中间版本
  ✗ 已确认交易的执行痕迹（traces）
  ✗ 过期的交易索引（> N 区块之前）
  ✗ 已归档的合约中间存储
```

#### 10.3.2 分层 Prune 配置

```rust
struct PruneConfig {
    // 快照：每 E 个区块生成一个完整状态快照
    snapshot_interval: u64,        // 默认 100_000 区块（约 7 小时）
    snapshot_keep: u64,            // 默认 3（保留最近 3 个快照）

    // Prune：每 P 个区块触发一次 prune
    prune_interval: u64,           // 默认 10_000 区块

    // 热数据：保留最近的完整状态
    keep_recent: u64,              // 默认 50_000 区块完整状态

    // 区块头：永远保留（用于轻客户端验证）
    // 区块体：超过 keep_recent 后只保留区块头，丢弃交易详情
    keep_block_body: u64,          // 默认 100_000 区块

    // 收据/日志：保留更长时间，用于区块浏览器查询
    keep_receipt: u64,             // 默认 1_000_000 区块

    // 节点模式
    node_mode: NodeMode,
}

enum NodeMode {
    /// 验证者：保留完整状态 + 最近 10 万区块
    Validator,
    /// 全节点：prune 历史中间状态，保留当前状态
    Full,
    /// 轻节点：只保留区块头，状态按需查询
    Light,
    /// 归档节点：保留所有历史数据（由社区/分析机构运行）
    Archive,
}
```

#### 10.3.3 各层 Prune 规则

**协议层（Protocol）：**
```
- 当前余额：始终保留（增量更新，覆盖旧值）
- 允许度：始终保留
- 资产注册表：始终保留
- 历史交易痕迹：超过 keep_recent prune
```

**EVM 层：**
```
- 当前 EVM 状态：始终保留
- 合约代码：始终保留
- 历史存储版本：超过 keep_recent prune
- 交易痕迹（traces）：超过 prune 策略丢弃
```

**Shielded Pool：**
```
Nullifier 集合（不可 prune）：
  - 必须完整保留，否则无法检测双花
  - 使用 BitSet 压缩存储，~1 bit / 已花费 nullifier
  - 预计 1 亿笔交易 ~ 12.5 MB

Merkle Tree（部分 prune）：
  - 根哈希：始终保留
  - 分支节点：保留到深度允许快速验证的范围
  - 加密 Note 数据：超过 keep_recent 可 prune 到归档节点
  - Viewing key 映射：始终保留

commitment 树增长估算：
  每笔 Shielded 交易 ~2 个 commitment
  10 万笔/天 × 365 天 = 3650 万 commitment
  每个 commitment ~32 字节 = ~1.2 GB/年（可接受）
```

**Agent 层：**
```
- 当前注册信息：始终保留
- 当前余额：始终保留
- 当前 nonce：始终保留
- 历史交易：超过 keep_recent prune
```

**共识层：**
```
- 区块头：始终保留
- 区块体（交易详情）：超过 keep_block_body prune
- 状态快照：保留最近 snapshot_keep 个
- 验证者历史集：超过 keep_recent prune
```

#### 10.3.4 状态快照生成与验证

```rust
struct StateSnapshot {
    height: u64,
    protocol_root: Hash,         // 协议余额 Merkle 根
    evm_root: Hash,              // EVM 状态根
    shielded_root: Hash,         // Shielded Merkle 根
    agent_root: Hash,            // Agent 状态根
    consensus_root: Hash,        // 验证者集哈希
    total_size: u64,             // 快照大小（字节）
    validator_signatures: Vec<ValidatorSignature>,  // 2/3 签名
}

// 快照生成（每 snapshot_interval 个区块）
fn generate_snapshot(state: &State, height: u64) -> StateSnapshot {
    StateSnapshot {
        height,
        protocol_root: compute_protocol_root(&state.protocol),
        evm_root: compute_evm_root(&state.evm),
        shielded_root: compute_shielded_root(&state.shielded),
        agent_root: compute_agent_root(&state.agent),
        consensus_root: compute_consensus_root(&state.validators),
        total_size: estimate_state_size(state),
        validator_signatures: collect_validator_signatures(state),
    }
}

// 快照验证
fn verify_snapshot(snapshot: &StateSnapshot) -> Result<()> {
    // 需要 2/3 验证者签名
    let valid_sigs = snapshot.validator_signatures
        .iter()
        .filter(|sig| verify_validator_sig(sig, &snapshot))
        .count();
    ensure!(valid_sigs >= quorum(), "Ins validator signatures");
    Ok(())
}
```

#### 10.3.5 快速同步流程

新节点使用快照 + 增量同步，无需从创世区块重放：

```
1. 从网络获取最近的状态快照（从其他节点或 P2P 快照市场）
2. 验证快照：
   - 检查 2/3 验证者签名
   - 验证 Merkle 根一致性
3. 恢复快照状态到本地存储
4. 从快照高度开始，逐区块同步后续数据
5. 达到最新高度后，开始参与共识/验证

预计时间：
  - 快照下载：~30 秒（100MB @ 100Mbps）
  - 快照验证：~5 秒
  - 状态恢复：~30 秒
  - 增量同步：~2-3 分钟（取决于落后区块数）
  - 总计：< 5 分钟
```

#### 10.3.6 Prune 触发与清理

```rust
// 定期 prune 检查
fn maybe_prune(state: &State, config: &PruneConfig) -> Result<()> {
    let current_height = state.current_height();

    if current_height % config.prune_interval != 0 {
        return Ok(());
    }

    let prune_boundary = current_height.saturating_sub(config.keep_recent);

    // 1. Prune 历史交易痕迹
    prune_execution_traces(prune_boundary)?;

    // 2. Prune 过期收据/日志
    prune_receipts(current_height.saturating_sub(config.keep_receipt))?;

    // 3. Prune 过期区块体
    prune_block_bodies(current_height.saturating_sub(config.keep_block_body))?;

    // 4. 清理过期快照（保留最近 N 个）
    prune_old_snapshots(config.snapshot_keep)?;

    // 5. Compact 数据库（释放物理磁盘空间）
    compact_database()?;

    Ok(())
}
```

#### 10.3.7 存储增长估算

| 节点模式 | 年化增长 | 1 年后总大小 | 说明 |
|----------|---------|------------|------|
| Validator | ~2 GB/月 | ~50 GB | 热数据 + 最近 5 万区块 |
| Full | ~500 MB/月 | ~20 GB | 当前状态 + 区块头 |
| Light | ~100 MB/月 | ~5 GB | 仅区块头 |
| Archive | ~50 GB/月 | ~600 GB | 保留所有历史 |

#### 10.3.8 CLI 配置示例

```bash
# 验证者节点（默认）
calld run --mode validator

# 全节点（prune 历史，保留当前状态）
calld run --mode full \
  --prune.keep-recent 50000 \
  --prune.keep-receipt 1000000 \
  --prune.keep-block-body 100000

# 轻节点（仅区块头，适合嵌入式设备）
calld run --mode light

# 归档节点（保留所有历史，用于分析/浏览器）
calld run --mode archive

# 自定义快照策略
calld run \
  --snapshot.interval 100000 \
  --snapshot.keep 3
```

---

## 11. RPC 接口

### 11.1 标准以太坊 JSON-RPC

完全支持以太坊 JSON-RPC 2.0 规范：
- `eth_call`, `eth_sendRawTransaction`
- `eth_getBalance`, `eth_getTransactionReceipt`
- `eth_blockNumber`, `eth_getLogs`
- ... 所有标准方法

### 11.2 Callchain 扩展方法

```json
// 协议层资产查询
{
    "method": "call_assetInfo",
    "params": [42],
    "id": 1
}
→ { asset_id, name, symbol, decimals, issuer, total_supply, policy }

// 协议层余额查询
{
    "method": "call_protocolBalance",
    "params": [42, "0x..."],
    "id": 1
}
→ { balance: "1000000000000000000" }

// 发起协议支付交易
{
    "method": "call_sendPayment",
    "params": [{ asset_id, from, to, amount, signature }],
    "id": 1
}
→ { tx_hash }

// 资产注册
{
    "method": "call_registerAsset",
    "params": [{ name, symbol, decimals, policy, signature }],
    "id": 1
}
→ { asset_id, evm_contract }

// 合规策略查询
{
    "method": "call_compliancePolicy",
    "params": [42],
    "id": 1
}
→ { policy: "OfacBlacklist", details: {...} }

// 统一余额查询
{
    "method": "call_totalBalance",
    "params": [42, "0x..."],
    "id": 1
}
→ { protocol: "30", evm: "70", total: "100" }

// Agent 注册
{
    "method": "call_agentRegister",
    "params": [{ name, agent_public_key, url, permissions, fee_config, signature }],
    "id": 1
}
→ { agent_id: 42 }

// Agent 信息查询
{
    "method": "call_agentInfo",
    "params": [42],
    "id": 1
}
→ { agent_id, owner, name, url, permissions, fee_config, registered_at }

// Agent 余额查询
{
    "method": "call_agentBalance",
    "params": ["0x...", 42, 1],
    "id": 1
}
→ { balance: "500000000", asset_id: 1 }

// Agent 交易历史
{
    "method": "call_agentHistory",
    "params": [42, { from_block: 0, to_block: "latest" }],
    "id": 1
}
→ { transactions: [...], total_spent: "...", days_active: 365 }

// Agent 资金授权
{
    "method": "call_agentGrant",
    "params": [{ agent_id, amount, asset_id, signature }],
    "id": 1
}
→ { tx_hash }

// Agent 撤销
{
    "method": "call_agentRevoke",
    "params": [{ agent_id, signature }],
    "id": 1
}
→ { tx_hash }

// Shielded Pool：生成存款证明（客户端调用，链下）
{
    "method": "call_shieldedDepositProve",
    "params": [{ asset_id, amount, recipient }],
    "id": 1
}
→ { commitment, encrypted_note }

// Shielded Pool：生成隐私转账证明（客户端调用，链下）
{
    "method": "call_shieldedTransferProve",
    "params": [{ asset_id, notes_to_spend, recipients, viewing_key }],
    "id": 1
}
→ { nullifiers, commitments, proof }

// Shielded Pool：余额查询（需要 viewing key）
{
    "method": "call_shieldedBalance",
    "params": [{ asset_id, viewing_key }],
    "id": 1
}
→ { total: "1000", notes: [{ commitment, encrypted_amount }] }

// Shielded Pool：Merkle Tree 状态
{
    "method": "call_shieldedTreeState",
    "params": [42],
    "id": 1
}
→ { depth: 32, leaf_count: 15234, root: "0x..." }
```

### 11.3 WebSocket

支持 WebSocket 订阅：
- `call_newPaymentBlock` — 新区块
- `call_paymentReceived` — 收到协议支付
- `call_bridgeCompleted` — 桥接完成
- `call_assetRegistered` — 新资产注册
- `call_agentExecuted` — Agent 交易执行
- `call_agentRevoked` — Agent 被撤销
- `call_shieldedDeposit` — 收到 Shielded 存款（需要 viewing key）
- `call_shieldedWithdrawal` — Shielded 提取到透明地址

---

## 12. 经济模型

### 12.1 CALL 代币

CALL 是 Callchain 的原生代币，承担 Gas、质押、治理三重功能。

| 属性 | 值 |
|------|------|
| 总供应量 | 1,000,000,000 CALL（10 亿，固定） |
| 最小单位 | 1 wei = 10⁻¹⁸ CALL |
| 增发 | 无，固定供应 |
| 通缩机制 | 50% 交易费销毁 |

**初始分配：**

| 类别 | 比例 | 数量 | 锁仓 |
|------|------|------|------|
| 验证者奖励 | 70% | 350M | 线性释放 8 年 |
| 生态基金 | 20% | 100M | 多签管理，社区治理 |
| 社区空投 | 10% | 50M | 主网上线释放 30%，剩余 24 个月线性 |
| 历史已分配 | - | 500M | 创世前已完成分配 |

### 12.2 Gas 支付（EIP-1559 动态费率）

所有协议层交易的 Gas 以 CALL 支付。采用**类 EIP-1559 动态费率**：每条指令定义固定的 gas unit，网络基础费率每区块自动调整，拥堵时涨价、空闲时降价。

#### 12.2.1 指令 Gas Unit 表

| 指令类型 | Gas Unit | 说明 |
|----------|---------|------|
| Transfer | 10,000 gas | 标准转账 |
| Transfer（含 Memo） | 10,000 + memo_bytes × 1 gas | 带备注转账 |
| Approve / Mint / Burn | 5,000 gas | 授权/铸造/销毁 |
| BatchTransfer 内每笔 | 1,000 gas | 批量支付每收款人 |
| BatchTransfer Memo | memo_bytes × 1 gas | 批量备注附加费用 |
| BridgeDeposit | 10,000 gas | 内部桥接存款 |
| ShieldedDeposit / Withdraw | 20,000 gas | 含 ZK 证明验证 |
| ShieldedTransfer | 50,000 gas | 含 ZK 证明验证 |
| Agent 指令 | 上述 × 0.5 | Agent 专属折扣 |
| ExternalBridgeDeposit | 30,000 gas | 外部桥接（含签名验证） |

#### 12.2.2 费用计算公式

```
总费用 = base_fee × 总 gas unit + priority_fee

其中：
  base_fee：每区块动态调整的基础费率（wei/gas unit）
  total_gas = 首条指令 gas + (N-1) × 后续指令 gas × 边际折扣系数
  priority_fee：用户自选优先级小费（全部给验证者）
```

**边际折扣系数：**
```
第 1 条指令：1.0 × gas（全价）
第 2-10 条指令：0.5 × gas（50% 折扣）
第 11+ 条指令：0.25 × gas（75% 折扣）
```

#### 12.2.3 Base Fee 动态调整

```rust
/// 每区块 base fee 调整参数
struct FeeParams {
    base_fee: u128,               // 当前基础费率（wei/gas）
    target_gas_per_block: u64,   // 目标 gas 使用量（区块）
    max_gas_per_block: u64,      // 最大 gas 上限
    adjustment_coefficient: u128, // 调整系数（1/8 = 12.5%）
}

/// 每区块更新 base fee
fn update_base_fee(current_base_fee: u128, block_gas_used: u64, params: &FeeParams) -> u128 {
    let target = params.target_gas_per_block;
    if block_gas_used == target {
        return current_base_fee;  // 刚好达到目标，不变
    }

    let adjustment = current_base_fee
        * (block_gas_used as i128 - target as i128).abs() as u128
        / target as u128
        / 8;  // 最大调整 12.5%

    if block_gas_used > target {
        // 拥堵：涨价，最大 +12.5%
        current_base_fee.saturating_add(adjustment)
    } else {
        // 空闲：降价，最大 -12.5%
        current_base_fee.saturating_sub(adjustment)
    }
}
```

**Base Fee 调整示例（假设初始 base_fee = 1 wei/gas，target_gas = 10M）：**

| 场景 | 区块 Gas 使用 | base_fee 变化 |
|------|-------------|--------------|
| 刚好目标 | 10,000,000 | 不变 |
| 轻度拥堵 | 12,000,000 (+20%) | +2.5% |
| 严重拥堵 | 15,000,000 (+50%) | +6.25% |
| 满负载 | 20,000,000 (+100%) | +12.5% |
| 空闲 | 5,000,000 (-50%) | -6.25% |
| 极空闲 | 0 | -12.5% |

**连续拥堵时 base_fee 呈指数增长：** 每区块 +12.5%，连续 6 个满负载区块 base_fee 翻倍。

#### 12.2.4 费用计算示例

```rust
// 示例：3 条指令的交易（Transfer + Approve + BridgeDeposit）
// 参数：base_fee = 10 wei/gas, priority_fee = 50,000 wei

let gas_units = vec![10_000, 5_000, 10_000];  // 三条指令的 gas
let discounts = vec![1.0, 0.5, 0.5];           // 边际折扣

let total_gas: u64 = gas_units.iter().zip(discounts.iter())
    .map(|(g, d)| (*g as f64 * d) as u64)
    .sum();
// = 10,000 × 1.0 + 5,000 × 0.5 + 10,000 × 0.5 = 17,500 gas

let total_fee = base_fee * total_gas + priority_fee;
// = 10 × 17,500 + 50,000 = 225,000 wei = 0.000000225 CALL
```

#### 12.2.5 费用分配

**CALL 支付时：**
```
用户支付的总费用 = base_fee × gas + priority_fee（以 CALL 计价）
  ├── base_fee × gas × 50% → 验证者奖励（CALL）
  ├── base_fee × gas × 50% → 直接销毁（通缩，仅 CALL）
  └── priority_fee 100%    → 打包该交易的验证者（CALL）
```

**稳定币支付时：**
```
用户支付的总费用 = base_fee × gas + priority_fee（以稳定币计价）
  ├── base_fee × gas × 50% → 验证者奖励（稳定币）
  ├── base_fee × gas × 50% → 进入国库储备（稳定币，不销毁）
  └── priority_fee 100%    → 打包该交易的验证者（稳定币）
```

**多币种汇总（按区块）：**
```
区块收入汇总:
  CALL:    1000 CALL → 50% 销毁 + 50% 验证者
  USDC:    20 USDC   → 50% 国库储备 + 50% 验证者
  USDT:    5 USDT    → 50% 国库储备 + 50% 验证者

验证者 A（权重 1/8）获得:
  CALL:  1000 × 50% × 1/8 = 62.5 CALL（奖励）
  USDC:  20 × 50% × 1/8 = 1.25 USDC（奖励）
  USDT:  5 × 50% × 1/8 = 0.3125 USDT（奖励）
  + priority_fee（全部给打包验证者）
```

#### 12.2.6 配置参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| initial_base_fee | 10 wei/gas | 初始基础费率 |
| target_gas_per_block | 10,000,000 gas | 目标区块 gas 使用量 |
| max_gas_per_block | 20,000,000 gas | 最大区块 gas 上限（2× target） |
| adjustment_coefficient | 1/8 | 每区块最大调整 12.5% |
| min_base_fee | 1 wei/gas | base_fee 下限（防止为零） |
| max_base_fee | 1,000,000,000 wei/gas | base_fee 上限（防止极端情况） |

#### 12.2.7 MemPool 准入

```rust
fn accept_to_mempool(tx: &EvmTx) -> Result<()> {
    let current_base_fee = get_current_base_fee();

    // 1. gas_limit 检查
    let estimated_gas = estimate_evm_gas(tx);
    ensure!(tx.gas_limit >= estimated_gas, "Gas limit too low");

    // 2. 费用检查：max_fee_per_gas 必须 >= 当前 base_fee
    ensure!(tx.max_fee_per_gas >= current_base_fee,
            "max_fee insufficient for current base_fee");

    // 3. 余额足够支付最大可能费用
    let max_cost = tx.gas_limit * tx.max_fee_per_gas + tx.value;
    let sender_balance = get_evm_balance(tx.sender);
    ensure!(sender_balance >= max_cost, "Insufficient balance");

    // 4. 预编译调用参数验证（如果目标地址是预编译）
    if is_precompile_address(tx.to) {
        validate_precompile_input(tx.to, &tx.data)?;
    }

    Ok(())
}
```

### 12.3 Gas 代付机制

Gas 代付通过标准 EVM 机制实现：EIP-7702（授权委托）、EIP-4337 Paymaster、或自定义代付合约。

#### 12.3.1 代付模式

```rust
enum GasSponsorMode {
    /// 发送者自付（标准 EVM 交易）
    SelfPay,

    /// EIP-7702 授权委托：Owner 授权 Agent 使用其地址发送交易
    Eip7702Delegation {
        delegator: Address,           // 被授权地址（如 Owner）
        authorization: Eip7702Auth,   // 7702 授权签名
    },

    /// EIP-4337 Paymaster：第三方代付合约支付 gas
    Paymaster {
        paymaster: Address,
        paymaster_data: Bytes,
    },

    /// 自定义代付合约：单笔交易由代付合约验证并支付
    CustomSponsor {
        sponsor_contract: Address,
        sponsor_data: Bytes,
    },
}

#### 12.3.0 稳定币直付 Gas（Stablecoin Direct Pay）

协议允许用户使用治理批准的稳定币直接支付 Gas 费用。验证者按实际收到的稳定币获得奖励，协议不执行币种转换。

**稳定币注册表：**

```rust
/// 允许的 Gas 支付币种
struct FeeCurrencyEntry {
    asset_id: AssetId,
    name: String,                // 如 "USDC"
    decimals: u8,
    oracle_price_key: String,    // 预言机价格对，如 "USDC_CALL"
    added_at_block: u64,
    added_by_proposal: u64,      // 通过治理提案 ID
}

struct FeeCurrencyRegistry {
    /// 已批准的稳定币列表
    allowed_currencies: Vec<FeeCurrencyEntry>,
    /// 每区块稳定币 Gas 支付上限（占总费用的比例，0-10000 bps）
    stablecoin_cap_bps: u16,     // 默认 5000 = 50%
}

impl FeeCurrencyRegistry {
    /// 验证稳定币是否在治理批准列表中
    fn is_allowed(asset_id: AssetId) -> bool {
        Self::allowed_currencies.iter().any(|e| e.asset_id == asset_id)
    }

    /// 查询指定稳定币的 oracle 价格（asset → CALL）
    fn get_call_price(asset_id: AssetId) -> Option<u128> {
        let entry = Self::allowed_currencies.iter()
            .find(|e| e.asset_id == asset_id)?;
        OracleSystem::get_median_price(&entry.oracle_price_key)
    }
}
```

**价格转换逻辑：**

```rust
/// 将 CALL 计价的 fee 转换为目标稳定币数量
fn convert_fee_to_stablecoin(asset_id: AssetId, fee_call: u128) -> Result<u128> {
    // oracle 价格: 1 unit stablecoin = price CALL
    // 例: 1 USDC = 50 CALL → price = 50_000_000_000_000_000_000 (18 decimals)
    let price_call = FeeCurrencyRegistry::get_call_price(asset_id)
        .ok_or("No oracle price for fee currency")?;

    // stablecoin_amount = fee_call / price_call × 10^decimals
    let decimals = get_asset_decimals(asset_id);
    let fee_stablecoin = fee_call
        .checked_mul(10u128.pow(decimals as u32))
        .ok_or("Overflow in fee conversion")?
        .checked_div(price_call)
        .ok_or("Division by zero in fee conversion")?;

    // 向上取整，确保协议不收少
    Ok(fee_stablecoin)
}
```

**执行流程：**

```
1. 用户提交交易: 调用稳定币支付预编译（0x201）
2. 协议验证: USDC 在 FeeCurrencyRegistry 中 ✓
3. 查询 oracle: 1 USDC = 50 CALL
4. 计算: fee_call = gas_used × base_fee
5. 转换: fee_usdc = fee_call / 50（向上取整）
6. 从 sender 协议层 USDC 余额扣除
7. USDC 计入区块费用汇总
8. 验证者按比例获得 USDC 奖励
```

**治理提案 — 添加/移除稳定币：**

```rust
// 治理提案类型扩展
enum ProposalType {
    // ... 已有类型
    FeeCurrencyAdd {
        asset_id: AssetId,
        name: String,
        oracle_price_key: String,
    },
    FeeCurrencyRemove {
        asset_id: AssetId,
        grace_period_blocks: u64,  // 移除前的宽限期
    },
    FeeCurrencyCap {
        new_cap_bps: u16,          // 新的稳定币支付上限（bps）
    },
}
```

| 参数 | 默认值 | 说明 |
|------|--------|------|
| stablecoin_cap_bps | 5000 (50%) | 每区块稳定币 Gas 占总费用上限 |
| min_market_cap_usd | 100,000,000 | 治理准入最低市值 |
| oracle_strikes_before_disable | 10 | 预言机异常 strikes 后自动停用 |
| grace_period_blocks | 86,400 (~1 天) | 移除前的宽限期 |

**Mempool 多币种优先级排序：**

```rust
fn priority_score(tx: &EvmTx) -> u128 {
    // EVM 交易统一按 effective_gas_price 排序
    // 对于 EIP-1559 交易：min(max_fee_per_gas, base_fee + max_priority_fee)
    // 对于 legacy 交易：gas_price
    tx.effective_gas_price()
}
```

**验证者费用分配（多币种）：**

```
区块费用汇总:
  CALL 收入:    1000 CALL
  USDC 收入:    20 USDC
  USDT 收入:    5 USDT

分配:
  50% base_fee × 50% 销毁 → 仅 CALL 部分销毁
  50% base_fee × 50% 验证者 → 按各币种比例分配
  priority_fee 100% → 打包验证者

验证者 A 获得: 250 CALL + 5 USDC + 1.25 USDT + priority_fee
```
```

| 模式 | 适用场景 | 代付者需在线 | 控制粒度 |
|------|---------|------------|---------|
| SelfPay | 普通用户 | - | - |
| EIP-7702 | Agent 支付、平台补贴 | 不需要 | 授权委托 |
| Paymaster | 平台批量补贴用户 | 不需要 | Paymaster 合约控制 |
| CustomSponsor | 低频单笔代付 | 需要 | 代付合约自定义逻辑 |

#### 12.3.2 EIP-7702 授权委托

Owner 通过 EIP-7702 授权 Agent 使用其地址发送交易，Agent 交易直接从 Owner EVM 账户扣除 gas。

```rust
struct Eip7702Auth {
    delegator: Address,           // 被授权地址（Owner）
    delegate: Address,            // 被委托地址（Agent）
    chain_id: u64,
    nonce: u64,
    expires_at: u64,              // 过期时间戳（0 = 永不过期）
    signature: Signature,         // delegator 的 secp256k1 签名
}
```

**授权注册：**

```rust
fn register_eip7702_auth(auth: Eip7702Auth) -> Result<()> {
    verify_signature(&auth.delegator, &auth.signature, &auth.hash())?;
    Eip7702Auths::insert(auth.delegator, auth.delegate, auth);
}

// 授权者随时可撤销
fn revoke_eip7702_auth(delegator: Address, delegate: Address) -> Result<()> {
    Eip7702Auths::remove(delegator, delegate);
}
```

**执行时验证：**

```rust
fn verify_eip7702_tx(tx: &EvmTx) -> Result<()> {
    let sender = recover_evm_signer(tx)?;
    // 检查 sender 是否有 7702 授权
    if let Some(auth) = Eip7702Auths::get(sender) {
        ensure!(current_timestamp() < auth.expires_at, "Authorization expired");
        // 交易由 delegate 签名，但 gas 从 delegator 扣除
        ensure!(tx.authorization_list.contains(auth.delegate), "Invalid delegate");
    }
    Ok(())
}
```

#### 12.3.3 EIP-4337 Paymaster

通过标准 EIP-4337 Paymaster 合约实现第三方代付。

```rust
/// Paymaster 接口（标准 EIP-4337）
interface IPaymaster {
    function validatePaymasterUserOp(
        PackedUserOperation calldata userOp,
        bytes32 userOpHash,
        uint256 maxCost
    ) external returns (bytes memory context, uint256 validationData);

    function postOp(
        PostOpMode mode,
        bytes calldata context,
        uint256 actualGasCost,
        uint256 actualUserOpFeePerGas
    ) external;
}
```

**使用场景：** 平台给新用户补贴 Gas，用户提交 UserOperation 时指定 Paymaster 地址，Paymaster 合约验证后代付 gas。

#### 12.3.4 自定义代付合约（CustomSponsor）

通过自定义代付合约实现灵活的代付逻辑，适合特定业务场景。

```rust
// 交易结构中包含代付合约地址和验证数据
struct CustomSponsorTx {
    evm_tx: EvmTx,
    sponsor_contract: Address,
    sponsor_data: Bytes,          // 代付合约自定义验证数据
}

fn verify_custom_sponsor(
    tx: &CustomSponsorTx,
) -> Result<()> {
    // 调用代付合约验证函数
    let valid = evm_staticcall(
        tx.sponsor_contract,
        abi_encode("verifySponsor(bytes,bytes)", tx.evm_tx.encode(), tx.sponsor_data),
    )?;
    ensure!(valid, "Sponsor verification failed");
    Ok(())
}
```

### 12.4 费用分配

```
用户支付 CALL:
  ├── 50% → 验证者奖励（按质押比例分配）
  ├── 50% → 直接销毁（通缩机制）
```

### 12.5 无通胀

- 无代币增发
- 验证者收入 100% 来自交易费用
- 50% 费用销毁使 CALL 总供应量持续通缩
- 长期验证者收入由链上经济活动驱动

### 12.6 验证者质押

```rust
struct ValidatorStake {
    validator_id: ValidatorId,
    staked_call: u128,              // 质押 CALL 数量
    self_stake: u128,               // 自质押部分
    delegated_call: u128,           // 委托质押
    rewards: u128,                  // 未领取的奖励
    slash_history: Vec<SlashEvent>, // 惩罚历史
}
```

| 参数 | 值 |
|------|------|
| 最小自质押 | 1,000,000 CALL |
| 委托质押上限 | 无限制 |
| 解锁期 | 7 天（约 2,419,200 区块） |
| 双签惩罚 | 扣除全部自质押 |
| 离线惩罚 | 按离线轮次比例扣除 |

### 12.7 费用 AMM（可选升级路径）

未来可支持用户用非 CALL 资产支付 Gas，通过内部 AMM 自动兑换：

```
用户支付 USDC → AMM 自动买入 CALL → 50% 给验证者 / 50% 销毁
```

当前阶段仅支持 CALL 支付 Gas。

---

## 13. 安全设计

### 13.1 密码学

| 用途 | 算法 |
|------|------|
| 交易签名 | secp256k1 |
| 共识签名 | ed25519 |
| 状态承诺 | SHA-256 + Merkle Tree |
| 地址生成 | Keccak-256 (EVM 兼容) |

### 13.2 MEV 防护

- PBS（Proposer-Builder Separation）内置
- 验证者不参与 MEV 提取
- 支付交易在 mempool 中加密（commit-reveal）

### 13.3 链上治理 (On-chain Governance)

Callchain 采用**双轨治理**：验证者投票决定技术参数，CALL 持有者投票决定生态决策。两者通过时间锁协调执行。

#### 13.3.1 治理架构

```rust
/// 治理提案
struct Proposal {
    id: u64,
    proposer: Address,              // 提案人
    proposal_type: ProposalType,     // 提案类型
    title: String,                   // 标题
    description: String,             // 详细说明
    voting_power_yes: u128,          // 赞成票权重
    voting_power_no: u128,           // 反对票权重
    voting_power_abstain: u128,      // 弃权票权重
    start_block: u64,                // 投票开始区块
    end_block: u64,                  // 投票结束区块
    execution_block: u64,            // 时间锁执行区块
    state: ProposalState,
    quorum_required: u128,           // 法定人数门槛
    execution_data: Vec<u8>,         // 序列化执行数据
}

enum ProposalType {
    /// 技术参数变更（gas 价格、区块大小、验证者数量等）
    ParameterChange { param_id: u64, new_value: Vec<u8> },
    /// 协议升级（新特性、指令类型、ZK 电路等）
    ProtocolUpgrade { activation_block: u64, changelog: String },
    /// 生态基金拨款（社区项目资助、空投等）
    TreasurySpend { recipient: Address, amount: u128, asset_id: AssetId },
    /// 验证者惩罚提案（Slash 恶意验证者）
    ValidatorSlash { validator_id: ValidatorId, reason: String },
    /// 合规策略更新（OFAC 黑名单更新等）
    ComplianceUpdate { asset_id: AssetId, new_policy: CompliancePolicy },
    /// 紧急暂停（共识层 bug，需 2/3 验证者联合签名）
    EmergencyPause { reason: String },
}

enum ProposalState {
    Pending,        // 等待投票开始
    Active,         // 投票进行中
    Passed,         // 投票通过，等待时间锁
    Defeated,       // 投票未通过
    Queued,         // 进入时间锁队列
    Executed,       // 已执行
    Expired,        // 时间锁过期未执行
}
```

#### 13.3.2 双轨投票

| 决策类型 | 投票群体 | 通过门槛 | 时间锁 |
|---------|---------|---------|--------|
| 技术参数变更 | 验证者（1 验证者 = 1 票） | 2/3 多数 | 7 天 |
| 协议升级 | 验证者 + CALL 持有者 | 2/3 验证者 + >50% CALL | 14 天 |
| 生态基金拨款 | CALL 持有者（1 CALL = 1 票） | >50% CALL 投票 + 60% 赞成 | 7 天 |
| 验证者惩罚 | 验证者 | 2/3 多数 | 立即 |
| 紧急暂停 | 验证者 | 2/3 多数 | 立即 |

```rust
/// 投票权重计算
fn calculate_voting_power(proposal: &Proposal, voter: Address) -> u128 {
    match proposal.proposal_type {
        ProposalType::ParameterChange { .. }
        | ProposalType::ProtocolUpgrade { .. }
        | ProposalType::ValidatorSlash { .. }
        | ProposalType::EmergencyPause { .. } => {
            // 验证者投票：1 验证者 = 1 票
            if is_validator(voter) { 1 } else { 0 }
        }
        ProposalType::TreasurySpend { .. } => {
            // 社区投票：CALL 余额加权
            get_call_balance(voter)
        }
        ProposalType::ComplianceUpdate { .. } => {
            // 资产发行方 + 验证者联合投票
            let asset = get_asset(proposal.proposal_type.asset_id());
            if asset.issuer == voter { asset.total_supply / 10 } // 发行方 10% 权重
            else if is_validator(voter) { 1 }
            else { 0 }
        }
    }
}

/// 委托投票（Delegation）
/// CALL 持有者可将投票权委托给第三方
struct VoteDelegation {
    delegator: Address,
    delegate: Address,
    amount: u128,               // 委托的 CALL 数量
    expires_at: u64,            // 过期时间（0=永久）
}

type VoteDelegations = HashMap<Address, Vec<VoteDelegation>>; // delegate → delegations

fn get_delegated_voting_power(delegate: Address, asset_id: AssetId) -> u128 {
    VoteDelegations::get(delegate)
        .iter()
        .filter(|d| d.expires_at == 0 || current_timestamp() < d.expires_at)
        .map(|d| d.amount)
        .sum()
}
```

#### 13.3.3 提案流程

```
1. 提交提案
   - 存入押金（防垃圾，10,000 CALL）
   - 指定提案类型、参数、执行数据
   - 提案进入 2 天审查期

2. 投票期（7 天）
   - 验证者/CALL 持有者按权重投票
   - 可选：赞成 / 反对 / 弃权
   - 投票实时计入链上

3. 结果判定
   - 达到法定人数（quorum）且赞成票 > 反对票 → Passed
   - 未达法定人数或反对票 > 赞成票 → Defeated

4. 时间锁（7-14 天，取决于提案类型）
   - Passed 的提案进入时间锁队列
   - 社区有时间协调升级或退出
   - 任何人均可触发执行

5. 执行
   - 到达 execution_block 后自动执行
   - 执行数据写入状态
   - 押金退还给提案人

6. 超时
   - execution_block 后 30 天内未执行 → Expired
   - 押金没收，充入生态基金
```

#### 13.3.4 法定人数 (Quorum)

```rust
fn calculate_quorum(proposal_type: &ProposalType) -> u128 {
    match proposal_type {
        ProposalType::ParameterChange { .. } => {
            // 2/3 验证者参与
            validator_count() * 2 / 3
        }
        ProposalType::ProtocolUpgrade { .. } => {
            // 2/3 验证者 + 总 CALL 供应 20% 参与
            max(validator_count() * 2 / 3, total_call_supply() / 5)
        }
        ProposalType::TreasurySpend { .. } => {
            // 总 CALL 供应 20% 参与
            total_call_supply() / 5
        }
        _ => validator_count() / 2 + 1,  // 简单多数
    }
}
```

#### 13.3.5 治理参数

| 参数 | 值 |
|------|------|
| 提案押金 | 10,000 CALL |
| 审查期 | 2 天（~691,200 区块） |
| 投票期 | 7 天（~2,419,200 区块） |
| 时间锁（参数变更） | 7 天 |
| 时间锁（协议升级） | 14 天 |
| 时间锁（紧急） | 立即 |
| 执行超时 | 30 天 |
| 委托投票 | 支持，可撤销 |
| 二次方投票 | 不支持（1 CALL = 1 票） |

### 13.4 升级机制

- 通过治理提案触发（详见 §19）
- 紧急暂停仅限共识层 bug（需 2/3 验证者签名）
- 无多签管理合约

### 13.5 网络攻击防护 (Network Attack Protection)

#### 13.5.1 区块级别限制

```rust
/// 区块配置限制
struct BlockLimits {
    /// 区块最大字节数（RLP 编码）
    max_block_size: u64,             // 默认 5 MB

    /// 区块最大交易数量
    max_transactions: u32,           // 默认 10,000

    /// Shielded 交易上限（ZK 证明验证成本高）
    max_shielded_per_block: u32,     // 默认 50

    /// 单笔交易最大 calldata 大小
    max_calldata_size: u32,          // 默认 256 KB

    /// 单笔交易最大字节数
    max_tx_size: u32,                // 默认 256 KB

    /// 批量转账最大收款人数
    max_batch_payments: u32,         // 默认 5,000

    /// EVM 区块 gas 上限
    max_evm_gas_per_block: u64,      // 默认 30,000,000
}
```

验证者在打包时强制执行这些限制，超出部分排入下一个区块。

#### 13.5.2 Mempool 防护

| 攻击类型 | 防护措施 | 参数 |
|---------|---------|------|
| 交易洪泛 | 最低费用阈值 + 动态拒绝 | 低于动态阈值直接拒绝 |
| 单地址占满 | 单地址 Pending 上限 | 256 tx/地址 |
| 大交易攻击 | 交易大小上限 | 256 KB |
| 预编译调用膨胀 | calldata 大小上限 | 256 KB |
| 批量转账膨胀 | 批量支付人数上限 | 5,000 人/tx |
| 签名伪造 | 立即验证并丢弃 | 无效签名永不进入 mempool |
| 重放攻击 | Nonce 检查 | 过时 nonce 立即拒绝 |
| Shielded 证明膨胀 | 证明大小验证 | >1KB 的 ZK 证明拒绝 |
| Agent 子账户滥用 | 权限 + 限额检查 | 日限额 + 单笔限额 |

```rust
/// Mempool 准入检查
fn accept_tx(tx: &EvmTx) -> Result<()> {
    // 1. 交易大小检查
    let tx_size = alloy_rlp::encode(tx).len();
    ensure!(tx_size <= BLOCK_LIMITS.max_tx_size as usize, "Tx too large");

    // 2. gas_limit 检查
    ensure!(tx.gas_limit <= BLOCK_LIMITS.max_evm_gas_per_block, "Gas limit too high");

    // 3. 最低 gas price 检查
    ensure!(tx.effective_gas_price() >= current_min_gas_price(), "Gas price too low");

    // 4. 单地址 Pending 上限
    let pending_count = mempool.count_pending(&tx.sender);
    ensure!(pending_count < 256, "Pending limit exceeded");

    // 5. 预编译调用参数验证（如果目标地址是预编译）
    if let Some(to) = tx.to {
        if is_precompile_address(to) {
            validate_precompile_input(to, &tx.data)?;
        }
    }

    // 6. 签名验证
    ensure!(verify_evm_signature(tx), "Invalid signature");

    // 7. Nonce 检查
    ensure!(tx.nonce >= get_evm_nonce(&tx.sender), "Invalid nonce");

    // 8. Pool 容量检查
    ensure!(mempool.evm_pool.len() < 100_000, "Pool full");

    Ok(())
}
```

#### 13.5.3 Shielded Pool 防护

| 风险 | 防护 |
|------|------|
| ZK 证明 DoS（大量无效证明） | 验证失败的交易立即丢弃 + 收取小额证明验证费（即使失败也收取） |
| Nullifier 集合膨胀 | 使用 BitSet 压缩存储，1 亿笔 ~12.5 MB |
| Merkle Tree 深度攻击 | 增量 Merkle Tree 深度上限 32，自动拒绝 |
| 大额隐私转账洗钱 | 合规模式（KYC/Whitelist）由资产发行方配置 |
| 每区块 Shielded 洪泛 | 每区块上限 50 笔，超出排队 |

#### 13.5.4 P2P 网络防护

```rust
struct NetworkLimits {
    /// 最大对等连接数
    max_peers: u32,                  // 默认 50

    /// 单连接最大消息速率
    max_messages_per_second: u32,    // 默认 100

    /// 消息大小上限
    max_message_size: u32,           // 默认 10 MB

    /// 已知交易去重缓存大小
    known_txs_cache_size: u32,       // 默认 1,000,000

    /// 恶意节点封禁时间
    ban_duration_seconds: u64,       // 默认 3600（1 小时）
}
```

| 攻击类型 | 防护 |
|---------|------|
| Sybil 攻击（大量虚假节点） | 连接数上限 + 节点声誉系统，异常连接自动断开 |
| 消息洪泛 | 单连接速率限制，超过阈值暂停 1 小时 |
| 大消息 DoS | 10 MB 消息上限，超出立即断开 |
| 区块/交易重传 | 去重缓存，已知 hash 不重复处理 |
| 日蚀攻击 | 维持 ≥8 个出站连接到不同子网 |
| 路由劫持 | commonware-p2p 支持 TLS 加密通道 + 节点 ID 验证 |

#### 13.5.5 共识层防护

| 攻击类型 | 防护 |
|---------|------|
| 51% 攻击 | Simplex BFT 容忍 <1/3 拜占庭节点，>2/3 诚实才能出块 |
| 双签攻击 | 检测到双签 → 自动 Slash 全部自质押 |
| 验证者离线 | 按离线轮次比例扣除质押，连续 100 轮离线 → 踢出验证者集 |
| 长程攻击（Long-range） | 轻客户端仅信任最近检查点，旧分叉自动拒绝 |
| Nothing-at-Stake | 每轮子集轮换，无法提前知道下一轮提议者 |

#### 13.5.6 经济防护总结

| 层级 | 防护手段 | 成本模型 |
|------|---------|---------|
| 交易层 | 费用防垃圾 | 每笔交易需支付 CALL |
| Mempool | 动态最低费用 + 容量限制 | 低费交易被拒绝 |
| 网络层 | 速率限制 + 连接数上限 | 攻击者带宽成本线性增长 |
| 共识层 | 质押 Slash | 恶意行为损失 > 收益 |
| Shielded | 证明验证费 + 每区块上限 | ZK 证明生成成本高 |
| 治理层 | 提案押金 | 垃圾提案损失 10,000 CALL |

---

## 14. 性能目标

| 指标 | 目标值 |
|------|--------|
| TPS | 5,000+ |
| 区块时间 | 250ms |
| 最终性 | ~500ms |
| 协议支付延迟 | < 10ms（协议层） |
| 桥接延迟 | < 1 个区块（< 250ms） |
| 节点硬件要求 | 4 核 / 8GB / 500GB SSD |

---

## 15. Rust Crate 结构

```
call-core/
├── Cargo.toml
├── crates/
│   ├── primitives/        # 基础类型 (Address, Hash, AssetId, Balance)
│   ├── crypto/            # 密码学 (secp256k1, ed25519, SHA-256)
│   ├── serialization/     # 协议序列化
│   ├── protocol/          # 协议支付层
│   │   ├── registry/      # 资产注册表
│   │   ├── balances/      # 余额管理
│   │   ├── compliance/    # 合规策略引擎
│   │   └── payment/       # 支付交易执行
│   ├── agent/             # Agent 支付层
│   │   ├── registry/      # Agent 注册与身份验证
│   │   ├── permissions/   # 权限与费用配置
│   │   ├── balances/      # Agent 子账户余额
│   │   └── executor/      # Agent 交易执行与费用代付
│   ├── shielded/          # Shielded Pool 隐私层
│   │   ├── merkle/        # 增量 Merkle Tree 管理
│   │   ├── notes/         # Note 创建、加密、存储
│   │   ├── nullifiers/    # Nullifier 集合与防双花
│   │   ├── circuit/       # ZK 电路定义与参数管理
│   │   ├── prover/        # 证明生成（客户端）与验证（节点）
│   │   └── compliance/    # 视图密钥管理与合规审计
│   ├── evm/               # EVM 智能合约层
│   │   ├── executor/      # EVM 执行器 (Revm)
│   │   ├── precompiles/   # 预编译合约 (桥接、协议余额)
│   │   └── contracts/     # 系统合约 (ERC-20 模板、桥接)
│   ├── bridge/            # 内部桥接
│   │   ├── deposit/       # Protocol → EVM
│   │   ├── withdraw/      # EVM → Protocol
│   │   └── sync/          # 跨层状态同步
│   ├── consensus/         # 共识层 (Commonware Simplex)
│   │   ├── simplex/       # Simplex 状态机集成
│   │   ├── proposer/      # 提议者选择与子集轮换
│   │   └── validator/     # 验证者管理与质押
│   ├── network/           # P2P 网络 (commonware-p2p)
│   ├── storage/           # 存储层 (reth-db)
│   ├── rpc/               # RPC 服务器 (jsonrpsee)
│   └── node/              # 节点应用和 CLI
└── tests/
    ├── integration/       # 集成测试
    └── e2e/              # 端到端测试
```

---

## 16. 创世 (Genesis)

### 16.1 创世区块

创世区块是链的初始状态，高度为 0，无父区块。

```rust
struct Genesis {
    chain_id: u64,
    timestamp: u64,
    initial_validators: Vec<ValidatorInfo>,
    initial_assets: Vec<GenesisAsset>,
    consensus_params: ConsensusParams,
}

struct ValidatorInfo {
    id: ValidatorId,
    public_key: PublicKey,
    consensus_key: Ed25519PublicKey,
    stake: u128,
    metadata: ValidatorMetadata,
}

struct GenesisAsset {
    name: String,
    symbol: String,
    decimals: u8,
    issuer: Address,
    initial_supply: u128,
    policy: CompliancePolicy,
}

struct ConsensusParams {
    max_validators: u32,
    subset_size: u32,
    block_time_millis: u64,
    slashing_window: u64,
}
```

### 16.2 创世格式

创世配置以 JSON 格式提供：

```json
{
    "chain_id": 1,
    "timestamp": 1744502400000,
    "initial_validators": [
        {
            "id": 1,
            "public_key": "0x...",
            "consensus_key": "ed25519:...",
            "stake": "1000000000000000000",
            "metadata": { "name": "Validator 1", "url": "https://..." }
        }
    ],
    "initial_assets": [
        {
            "name": "Callchain Token",
            "symbol": "CALL",
            "decimals": 18,
            "issuer": "0x...",
            "initial_supply": "1000000000000000000000000000",
            "policy": "None"
        }
    ],
    "consensus_params": {
        "max_validators": 216,
        "subset_size": 21,
        "block_time_millis": 250,
        "slashing_window": 10000
    },
    "initial_fee_currencies": [
        {
            "asset_symbol": "CALL",
            "oracle_price_key": "CALL_USD"
        }
    ],
    "fee_params": {
        "initial_base_fee": 10,
        "target_gas_per_block": 10000000,
        "max_gas_per_block": 20000000,
        "stablecoin_cap_bps": 5000
    }
}
```

### 16.3 启动流程

```
1. 解析 genesis.json
2. 初始化协议层余额：对每个 GenesisAsset，balances[issuer] = initial_supply
3. 部署对应 ERC-20 合约到 EVM 层（初始供应为 0）
4. 注册初始验证者集
5. 注册初始 Gas 支付币种到 FeeCurrencyRegistry
6. 创建创世区块（height=0, parent_hash=0x0）
7. 计算初始状态根（payment_root + evm_state_root + bridge_root）
8. 节点从高度 0 开始运行共识
```

---

## 17. 交易池 (Mempool)

### 17.1 设计

交易池维护待打包交易，按类型分桶管理：

```rust
struct Mempool {
    evm_pool: PriorityTxs<EvmTx>,                    // EVM 交易（含预编译调用）
    pending_bridges: VecDeque<BridgeOp>,             // 待处理桥接
    known_txs: LruCache<TxHash, ()>,                 // 去重缓存
}
```

### 17.2 优先级与排序

| 维度 | 策略 |
|------|------|
| EvmTx 排序 | 按 gas price 降序 + nonce 顺序 |
| BridgeOp | 按到达顺序 FIFO，区块内批量处理 |

**预编译交易与普通 EVM 交易统一排序：**

所有交易均为标准 EVM 交易，统一按 `gas_price` 降序排列。调用预编译地址的交易与普通合约调用交易在 mempool 中无区别，均通过 gas price 竞争打包优先级。

### 17.3 容量与驱逐

| 参数 | 值 | 说明 |
|------|------|------|
| evm_pool 上限 | 100,000 txs | 根据 gas limit 动态调整 |
| 单地址 Pending 上限 | 256 txs | 防止单地址占满池 |
| 最小 gas price | 动态 | 低于阈值自动驱逐 |
| 生命周期 | 72 区块 | 超时未打包驱逐 |

**驱逐策略：**
1. gas price（CALL 等值）低于当前最低接受阈值的交易优先驱逐
2. nonce 过时（已过期）的交易立即清除
3. 池满时按优先级从尾部驱逐

### 17.4 防垃圾机制

- EvmTx（含预编译调用）：最低 gas price 要求
- 稳定币支付 Gas：需在 FeeCurrencyRegistry 中，且有有效 oracle 价格
- 重复交易检测：已知 TxHash 直接拒绝
- 无效签名交易立即丢弃并记录

---

## 18. 状态转换 (State Transition)

### 18.1 形式化定义

```
State = (ProtocolBalances, EvmState, BridgeState, ShieldedState)

apply_block(state, block) -> Result<State> {
    state = execute_evm_txs(state, block.evm_txs)?;
    state = execute_bridge(state, block.bridge_operations)?;
    state = execute_system_txs(state, block.system_txs)?;
    Ok(state)
}
```

### 18.2 交易有效性规则

**EvmTx 验证（含预编译调用）：**
- 签名有效（secp256k1，以太坊兼容）
- nonce >= 账户当前 nonce
- 发送者余额 >= gas_limit * gas_price + value
- gas_limit <= 区块 gas 上限
- 预编译调用参数通过 ABI 解码验证
- 涉及资产存在且状态为 Active
- 预编译内部合规检查通过（`check_compliance`）

**BridgeOp 验证：**
- 对应 EVM 层桥接合约已触发（WithdrawToProtocol）
- 或协议层桥接请求已记录（DepositToEvm）
- 资产存在且桥接池余额充足

**Shielded 指令验证：**
- ZK 证明验证通过（Groth16/Halo2）
- 所有 nullifier 未被花费（防双花）
- Merkle Tree 根匹配（输入 Notes 确实存在）
- 资产存在且 Shielded 功能已启用
- ShieldedDeposit：发送者透明余额 >= 存入金额
- ShieldedWithdraw：证明中公开提取金额，余额恢复到目标地址
- ShieldedTransfer：证明中隐含 input >= output（不暴露具体值）

### 18.3 原子性保证

区块内所有操作要么全部成功，要么全部回滚：
- EVM 交易（含预编译调用）：Revm 的 Journal 机制保证单 tx 原子性。预编译内部使用快照机制保证协议状态操作原子性
- 桥接操作：两步操作（扣减+铸造 / 销毁+恢复）在同一函数内完成
- 区块级别：状态根在所有操作后计算，不一致则拒绝区块

### 18.4 交易收据（Transaction Receipts）

每个交易执行后生成一条收据，打包到区块中。收据是区块浏览器查询、合约日志读取、事件监听的唯一来源。

#### 18.4.1 预编译调用收据

预编译调用生成标准 EVM 交易收据，额外附加协议层状态变更信息：

```rust
/// 预编译调用收据（扩展标准 EVM 收据）
struct PrecompileReceipt {
    /// 标准 EVM 收据字段
    tx_hash: Hash,
    status: bool,               // true = success, false = reverted
    gas_used: u64,
    contract_address: Option<Address>,
    logs: Vec<EvmLogEntry>,
    logs_bloom: Bloom,

    /// 协议层扩展信息
    precompile_address: Address,    // 被调用的预编译地址
    protocol_state_changes: Vec<ProtocolStateChange>,
    memos: Vec<MemoEntry>,          // 支付备注
}

/// 协议层状态变更摘要
struct ProtocolStateChange {
    asset_id: AssetId,
    address: Address,
    change_type: ChangeType,
    before: u128,
    after: u128,
}

enum ChangeType {
    Balance,
    Allowance,
    AgentBalance,
    ShieldedCommitment,
    NullifierSpent,
}

/// 收据中的备注条目
struct MemoEntry {
    precompile_address: Address,
    memo: PaymentMemo,
}

/// 支付备注
struct PaymentMemo {
    message: String,                // 最大 256 字节
    reference: Option<String>,      // 最大 128 字节
    metadata: Option<Vec<u8>>,      // 最大 1024 字节
}
```

#### 18.4.2 EVM 交易收据

EVM 交易收据遵循以太坊标准格式，与现有以太坊工具链兼容：

```rust
struct EvmReceipt {
    tx_hash: Hash,
    status: bool,               // true = success, false = reverted
    gas_used: u64,
    contract_address: Option<Address>,  // 如果是合约创建
    logs: Vec<EvmLogEntry>,
    logs_bloom: Bloom,          // Bloom 过滤器（快速日志过滤）
}

struct EvmLogEntry {
    address: Address,
    topics: Vec<H256>,
    data: Vec<u8>,
}
```

#### 18.4.3 Shielded 交易收据

Shielded 交易的收据需要隐藏敏感信息（金额、参与者），同时保证可验证性：

```rust
struct ShieldedReceipt {
    tx_hash: Hash,
    status: ExecutionStatus,
    gas_used: u128,
    gas_payer: Address,

    // 公开信息
    nullifiers: Vec<Nullifier>,    // 公开（防双花）
    commitments: Vec<NoteCommitment>, // 公开（Merkle Tree 更新）

    // 隐藏信息（只有 viewing key 持有者可读）
    encrypted_event: Option<Vec<u8>>, // 加密的事件数据
    // 不暴露：发送方、接收方、金额
}
```

Shielded 收据的特点：
- `nullifiers` 和 `commitments` 公开，用于 Merkle Tree 状态维护
- 金额、发送方、接收方不写入收据
- `encrypted_event` 可选，包含加密的详细信息，只有 viewing key 持有者可以解密
- 区块浏览器只显示"发生了 Shielded 操作"，不显示具体内容

#### 18.4.4 外部桥接交易收据

```rust
struct ExternalBridgeReceipt {
    tx_hash: Hash,
    status: ExecutionStatus,
    gas_used: u128,
    bridge_op: ExternalBridgeOp,

    // 桥接特有信息
    source_tx_hash: Option<Hash>,     // 源链交易哈希
    source_block: Option<u64>,        // 源链区块高度
    confirmations: Option<u32>,       // 源链确认数
}
```

#### 18.4.5 区块收据树

每个区块的收据打包成一棵 Merkle Tree，根哈希写入区块头：

```rust
/// 区块头增加字段
struct BlockHeader {
    parent_hash: Hash,
    height: u64,
    timestamp_millis: u64,
    payment_root: Hash,
    evm_state_root: Hash,
    bridge_root: Hash,
    receipt_root: Hash,             // 新增：收据 Merkle 根
    proposer: ValidatorId,
    signature: Signature,
}

/// 收据 Merkle 树
/// 按交易顺序排列，根哈希保证收据完整性
fn compute_receipt_root(receipts: &[Receipt]) -> Hash {
    let leaves: Vec<Hash> = receipts.iter()
        .map(|r| keccak256(rlp_encode(r)))
        .collect();
    build_merkle_root(&leaves)
}
```

#### 18.4.6 收据查询（RPC 接口）

```json
// 按交易哈希查询收据
{
    "method": "call_getTransactionReceipt",
    "params": ["0xabc..."],
    "id": 1
}
→ {
    "tx_hash": "0xabc...",
    "block_height": 42,
    "status": "success",
    "gas_used": "15000",
    "gas_payer": "0x123...",
    "logs": [
        { "address": "0x...", "topics": ["..."], "data": "..." }
    ],
    "state_changes": [
        { "asset_id": 1, "address": "0x123...", "type": "balance", "before": "100", "after": "90" }
    ]
}

// 按区块高度查询所有收据
{
    "method": "call_getBlockReceipts",
    "params": [42],
    "id": 1
}
→ [{ receipt_1, receipt_2, ... }]

// 按地址过滤日志
{
    "method": "call_getLogs",
    "params": [{ address: "0x...", topics: ["..."], from_block: 0, to_block: "latest" }],
    "id": 1
}
→ [{ log_1, log_2, ... }]

// EVM 兼容查询（eth_getTransactionReceipt）
{
    "method": "eth_getTransactionReceipt",
    "params": ["0xabc..."],
    "id": 1
}
→ 标准以太坊收据格式

// 按备注参考号查询交易
{
    "method": "call_getTxByReference",
    "params": ["PAYROLL-2026-03-001"],
    "id": 1
}
→ [{ tx_hash, block_height, memo, status, timestamp }]
```

#### 18.4.7 收据 Prune 策略

收据数据增长快，但主要用于历史查询。Prune 策略见 §10.3：

```
- 近期收据（最近 keep_receipt 区块）：完整存储
- 历史收据：超过 keep_receipt 后 prune
- 收据根哈希（receipt_root）：永远保留在区块头
- prune 后仍可通过 Merkle 证明验证某条收据属于某区块
```

| 节点模式 | 收据保留 |
|----------|---------|
| Validator | 最近 100 万区块 |
| Full | 最近 100 万区块 |
| Light | 无（按需查询全节点） |
| Archive | 所有历史 |

---

## 19. 分叉升级 (Fork/Upgrade)

### 19.1 协议版本

```rust
struct ProtocolVersion {
    major: u32,   // 不兼容变更
    minor: u32,   // 向后兼容特性
    patch: u32,   // Bug 修复
}
```

### 19.2 升级机制选项

| 机制 | 选项 A: 高度激活 | 选项 B: 信号投票 | 选项 C: 治理提案 |
|------|------------------|------------------|------------------|
| 触发方式 | 预设区块高度 | 验证者 2/3 信号 | 链上治理提案通过 |
| 灵活性 | 低（需提前计划） | 中 | 高 |
| 安全性 | 高（确定性） | 中 | 高 |
| 复杂度 | 最低 | 中 | 高 |
| **推荐** | ✅ 用于计划内升级 | ⚠️ 用于紧急升级 | ✅ 长期治理方向 |

**当前选择：高度激活 + 治理提案双轨制**
- 计划内升级：预设区块高度，所有节点同步升级
- 重大变更：通过链上治理提案（2/3 验证者投票）+ 时间锁执行

### 19.3 升级流程

```
1. 提案：提交升级提案（包含新版本、激活高度、变更说明）
2. 投票：验证者在 7 天内投票，需 2/3 多数
3. 时间锁：通过后 7 天时间锁，给节点升级时间
4. 激活：到达预设高度后，新版本规则生效
5. 未升级节点：自动停止出块（版本检查失败）
```

### 19.4 回滚策略

- 升级后 100 个区块内发现严重 bug：2/3 验证者签名可紧急暂停
- 暂停后网络停止出块，直到问题修复
- 无自动回滚（会破坏最终性），需协调重启

---

## 20. 遥测与监控 (Telemetry/Metrics)

### 20.1 指标系统

```rust
// Prometheus 指标
metrics: {
    // 共识层
    call_consensus_round_duration_seconds: Histogram,
    call_consensus_rounds_total: Counter,
    call_consensus_proposals_received: Counter,
    call_consensus_votes_received: Counter,
    call_consensus_validator_set_size: Gauge,

    // 交易层
    call_mempool_size: GaugeVec,          // 按类型分
    call_transactions_processed_total: CounterVec, // 按类型/状态分
    call_transaction_execution_time_seconds: Histogram,

    // 桥接
    bridge_operations_processed_total: Counter,
    bridge_deposit_total: CounterVec,      // 按资产分
    bridge_withdraw_total: CounterVec,

    // P2P 网络
    call_p2p_peers: Gauge,
    call_p2p_messages_sent_total: CounterVec,
    call_p2p_messages_received_total: CounterVec,
    call_p2p_bandwidth_bytes: CounterVec,

    // 性能
    call_block_height: Gauge,
    call_block_processing_time_seconds: Histogram,
    call_state_root_computation_time_seconds: Histogram,

    // 系统
    call_process_cpu_seconds: Counter,
    call_process_memory_bytes: Gauge,
    call_process_open_fds: Gauge,
}
```

### 20.2 集成

- **Prometheus**: 默认在 `:9090` 暴露 `/metrics` 端点
- **OpenTelemetry**: 可选集成，支持 Jaeger/Zipkin 分布式追踪
- **Grafana Dashboards**: 预置仪表板
  - Consensus Overview: 轮次时间、投票率、验证者活性
  - Transaction Throughput: TPS、延迟、池大小
  - Bridge Operations: 存款/提取量、延迟
  - System Health: CPU、内存、磁盘、网络

### 20.3 告警规则

| 告警 | 条件 | 级别 |
|------|------|------|
| 共识停滞 | 60 秒无新区块 | Critical |
| 验证者离线 | 验证者 100 轮未投票 | Warning |
| 交易池溢出 | 池使用率 > 90% | Warning |
| 桥接延迟 | 待处理桥接 > 100 笔 | Warning |
| 内存溢出 | RSS > 6GB | Critical |
| 磁盘空间 | 可用空间 < 50GB | Warning |

---

## 21. 节点启动与配置 (Boot/Config)

### 21.1 CLI 参数

```bash
callchain-node [OPTIONS]

共识层:
    --genesis <PATH>              创世配置文件路径（必需）
    --validator                   以验证者模式运行
    --validator-key <HEX>         验证者私钥
    --consensus-key <HEX>         共识 ed25519 私钥
    --peers <ADDRS>               初始种子节点，逗号分隔

网络层:
    --p2p-listen <ADDR>           P2P 监听地址（默认 0.0.0.0:51235）
    --p2p-advertise <ADDR>        对外公告地址
    --max-peers <N>               最大对等连接数（默认 50）

RPC 层:
    --rpc-http-addr <ADDR>        HTTP RPC 地址（默认 127.0.0.1:8545）
    --rpc-ws-addr <ADDR>          WebSocket 地址（默认 127.0.0.1:8546）
    --rpc-cors <ORIGINS>          CORS 允许来源

存储层:
    --data-dir <PATH>             数据目录（默认 ~/.callchain）
    --db-cache-size <MB>          数据库缓存大小（默认 1024）

监控层:
    --metrics-addr <ADDR>         Prometheus 地址（默认 0.0.0.0:9090）
    --tracing                     启用 OpenTelemetry 追踪

日志层:
    --log-level <LEVEL>           日志级别（默认 info）
    --log-format <FORMAT>         日志格式：json|text（默认 text）
```

### 21.2 配置文件 (TOML)

```toml
[chain]
chain_id = 1
genesis_file = "genesis.json"

[consensus]
validator = true
validator_key_file = "keys/validator.pem"
consensus_key_file = "keys/consensus.pem"

[network]
listen_addr = "0.0.0.0:51235"
advertise_addr = "public-ip:51235"
bootstrap_nodes = [
    "/dns4/seed1.callchain.cc/tcp/51235/p2p/...",
    "/dns4/seed2.callchain.cc/tcp/51235/p2p/...",
]

[rpc]
http_addr = "0.0.0.0:8545"
ws_addr = "0.0.0.0:8546"
cors_origins = ["*"]

[storage]
data_dir = "/var/lib/callchain"
db_cache_size = 2048  # MB

[metrics]
enabled = true
addr = "0.0.0.0:9090"

[logging]
level = "info"
format = "json"
file = "/var/log/callchain/node.log"
```

### 21.3 启动流程

```
1. 解析 CLI 参数 + 配置文件（CLI 优先）
2. 初始化日志系统
3. 打开/创建数据库 (reth-db)
4. 加载创世配置，初始化状态
   - 若数据库为空：执行创世初始化
   - 若数据库已有数据：从最后状态恢复
5. 初始化 P2P 网络（commonware-p2p）
6. 连接种子节点，建立对等连接
7. 初始化共识引擎（Simplex）
8. 启动 RPC 服务器（HTTP + WebSocket）
9. 启动监控端点（Prometheus）
10. 开始同步区块 / 参与共识
```

---

## 22. 状态过期 (State Expiration)

### 22.1 设计选项

| 模型 | 选项 A: 无状态过期 | 选项 B: 状态租金 | 选项 C: 自动过期 |
|------|-------------------|------------------|------------------|
| 状态增长 | 无限增长 | 需支付维持费 | 超时自动清除 |
| 用户负担 | 无 | 周期性支付 | 需定期活跃 |
| 节点负担 | 持续增长 | 可控 | 可控 |
| 实现复杂度 | 最低 | 高 | 中 |
| **推荐** | ✅ 初期 | ⚠️ 长期目标 | ❌ 不适合资产链 |

**当前选择：无状态过期（初期）**

理由：协议支付层的核心价值是确定性余额映射，状态过期会破坏这一保证。初期采用**无状态过期**，协议层余额和桥接状态永不过期。

### 22.2 EVM 层状态管理

EVM 层遵循以太坊 EIP-161 规则：
- 空账户（nonce=0, balance=0, code_hash=empty）在交易后自动清除
- 合约存储槽为零值时不写入磁盘
- 未来可考虑 EIP-7742（有状态过期）作为升级路径

### 22.3 存储优化

- 状态剪枝：保留最近 N 个区块的状态，更早的状态通过 Merkle 证明重建
- 快照压缩：定期创建状态快照，删除旧的历史数据
- 归档节点可选：提供完整历史查询的归档节点模式

### 22.4 状态过期与 Prune 的关系

状态过期（§22）和数据 Prune（§10.3）解决不同层面的问题，但相互配合：

```
状态过期（State Expiration）
  → 解决"哪些逻辑数据应该从账本中移除"
  → 协议语义层：账户不活跃 N 天后是否还保留
  → 决定什么数据"不再有意义"

数据 Prune（Data Pruning）
  → 解决"节点硬盘上保留多少历史数据"
  → 存储实现层：即使数据有意义，也不必保留所有中间状态
  → 决定什么数据"不再需要存储在本地"

关系：
  1. 状态过期减少 prune 的工作量（过期数据自然不需要 prune）
  2. Prune 可以比状态过期更激进（当前余额不过期，但历史中间状态可 prune）
  3. 两者共同保证节点存储可控增长
```

| 数据类型 | 状态过期策略 | Prune 策略 |
|----------|------------|-----------|
| 协议层余额 | 永不过期 | 当前值保留，历史版本 prune |
| Shielded nullifier | 永不过期 | 永不过期（防双花必需） |
| Agent 注册信息 | 永不过期（除非 Revoke） | 当前值保留 |
| EVM 空账户 | 自动清除（EIP-161） | 清除后自然释放存储 |
| 历史交易痕迹 | 不适用 | 超过 keep_recent prune |
| 区块体（交易详情） | 不适用 | 超过 keep_block_body prune |

---

## 23. 轻客户端 (Light Client)

### 23.1 协议

轻客户端不存储完整状态，仅验证区块头：

```rust
struct LightClient {
    trusted_validators: HashMap<ValidatorId, PublicKey>,
    latest_block_header: BlockHeader,
    chain_id: u64,
}

impl LightClient {
    /// 验证新区块头
    fn verify_header(&mut self, header: &BlockHeader) -> Result<()> {
        // 1. 验证父哈希链接
        ensure!(header.parent_hash == self.latest_block_header.hash());

        // 2. 验证 2/3+ 验证者签名
        let signatures = header.aggregate_signature;
        let voting_power = self.calculate_voting_power(&signatures);
        ensure!(voting_power > self.total_voting_power() * 2 / 3);

        // 3. 验证状态根一致性
        self.latest_block_header = header.clone();
        Ok(())
    }

    /// 验证 Merkle 证明
    fn verify_proof<T: MerkleProof>(
        &self,
        proof: &T,
        root: Hash,
    ) -> Result<T::Value> {
        proof.verify(root)
    }
}
```

### 23.2 支持的操作

| 操作 | 方法 |
|------|------|
| 验证区块头 | `light_verifyBlockHeader` |
| 协议余额证明 | `call_getBalanceProof(asset_id, address)` → MerkleProof |
| EVM 余额证明 | `eth_getProof(address, storageKeys, blockNumber)` |
| 交易包含证明 | `call_getTransactionProof(tx_hash)` → MerkleProof |
| 桥接操作证明 | `call_getBridgeProof(op_hash)` |
| Shielded Pool 状态证明 | `call_getShieldedStateProof(asset_id)` → ShieldedStateProof |
| Shielded 余额查询 | `call_getShieldedBalanceProof(viewing_key)` → EncryptedBalanceProof |
| Shielded 交易包含 | `call_getShieldedTxProof(tx_hash)` → ShieldedMerkleProof |

### 23.3 Shielded Pool 轻客户端验证

轻客户端对 Shielded Pool 的支持分为两类：

**全验证模式（验证 ZK 证明）：**
```rust
/// 轻客户端验证 ShieldedTransfer
/// 需要下载 ZK 证明并验证（计算量大，安全性最高）
fn verify_shielded_tx_full(&self, tx: &ShieldedTransfer) -> Result<()> {
    // 1. 验证 ZK 证明（Groth16 ~3ms）
    verify_zk_proof(&tx.proof)?;

    // 2. 验证 nullifier 未被花费（需要全节点提供证明）
    let nullifier_proof = request_nullifier_proof(tx.nullifiers)?;
    ensure!(verify_merkle_proof(&nullifier_proof));

    // 3. 验证 commitment 已上链
    let commit_proof = request_commitment_proof(tx.commitments)?;
    ensure!(verify_merkle_proof(&commit_proof));

    Ok(())
}
```

**简化模式（信任节点摘要，适用于移动端）：**
```rust
/// 轻客户端仅验证 Shielded 状态的摘要证明
/// 不验证 ZK 证明本身，只验证"验证者已验证此交易"
fn verify_shielded_tx_light(&self, tx_hash: Hash) -> Result<()> {
    // 1. 请求全节点提供 Shielded 交易的 Merkle 包含证明
    let proof = request_shielded_merkle_proof(tx_hash)?;

    // 2. 验证该交易确实包含在被验证的区块中
    ensure!(proof.verify(self.latest_block_header.shielded_root));

    // 3. 验证 2/3 验证者已签名该区块头
    // （隐含验证者已验证了 ZK 证明）
    Ok(())
}
```

**Shielded 余额查询（通过 Viewing Key）：**
```rust
/// 轻客户端使用 viewing key 查询 Shielded 余额
fn query_shielded_balance(
    &self,
    viewing_key: &ViewingKey,
    asset_id: AssetId,
) -> Result<u128> {
    // 1. 向全节点请求：用 viewing key 解密相关 Notes
    let notes = request_shielded_notes(viewing_key, asset_id)?;

    // 2. 每个 Note 附带 Merkle 包含证明
    for note in &notes {
        ensure!(note.proof.verify(self.latest_block_header.shielded_root));
    }

    // 3. 总和 = 余额（仅持有 viewing key 可解密）
    Ok(notes.iter().map(|n| n.value).sum())
}
```

### 23.4 同步策略

| 阶段 | 说明 |
|------|------|
| 初始同步 | 从可信检查点（checkpoint）开始，验证每个区块头 |
| 增量同步 | 逐个验证新区块头签名和状态根 |
| 状态同步 | 按需请求全节点获取 Merkle 证明 |

轻客户端资源占用：
- 存储：仅区块头 + 验证者集（< 10MB）
- 带宽：每区块 ~1KB 区块头
- 计算：每区块签名验证（216 个验证者 ~50ms）

---

## 24. 日志与审计 (Logging/Auditing)

### 24.1 结构化日志

```rust
// 示例日志条目
struct LogEntry {
    timestamp: String,        // ISO 8601
    level: LogLevel,          // trace, debug, info, warn, error
    target: String,           // 模块路径
    message: String,
    fields: HashMap<String, Value>,  // 结构化字段
}

// 示例输出（JSON 格式）
{
    "timestamp": "2026-04-13T10:30:00.123Z",
    "level": "info",
    "target": "callchain_protocol::payment",
    "message": "Payment transaction executed",
    "fields": {
        "tx_hash": "0xabc...",
        "asset_id": 42,
        "from": "0x123...",
        "to": "0x456...",
        "amount": "1000000000000000000",
        "fee": "100000000000",
        "duration_ms": 2
    }
}
```

### 24.2 审计日志

与普通日志不同，审计日志是不可变的追加日志，记录所有状态变更：

```rust
struct AuditEntry {
    block_height: u64,
    tx_index: u32,
    tx_type: String,        // "payment", "agent", "evm", "bridge", "system", "shielded"
    action: String,         // "transfer", "agent_pay", "mint", "burn", "deposit", "withdraw",
                            // "shielded_transfer", "shielded_deposit", "shielded_withdraw"
    agent_id: Option<u64>,  // Agent 交易时记录
    fee_payer: Option<String>, // "self", "owner", "third_party"（Agent 交易时）
    before_state: StateSnapshot,  // 变更前的相关状态
    after_state: StateSnapshot,   // 变更后的相关状态
    tx_hash: Hash,
    shielded_details: Option<ShieldedAuditInfo>, // 隐私交易审计（仅 viewing key 持有者可读）
}

/// 隐私交易审计信息（加密存储）
struct ShieldedAuditInfo {
    nullifiers: Vec<Nullifier>,
    commitments: Vec<NoteCommitment>,
    encrypted_amounts: Vec<EncryptedValue>,  // 只有 viewing key 持有者可解密
    compliance_mode: String,                 // "unrestricted", "kyc_required", ...
}
```

审计日志存储：
- 独立数据库表（`audit_log`），仅追加，不可删除
- 定期 Merkle 化，根哈希写入区块头（可选，用于第三方审计验证）

### 24.3 合规报告 API

```json
// 导出特定时间范围内的合规报告
{
    "method": "call_exportComplianceReport",
    "params": [{
        "asset_id": 42,
        "from_timestamp": "2026-01-01T00:00:00Z",
        "to_timestamp": "2026-04-01T00:00:00Z",
        "addresses": ["0x123...", "0x456..."],
        "format": "csv"
    }],
    "id": 1
}
→ { report_url: "https://.../report.csv", expires_at: "..." }
```

### 24.4 日志配置

| 参数 | 选项 | 默认值 |
|------|------|--------|
| 日志级别 | trace, debug, info, warn, error | info |
| 日志格式 | json, text | text |
| 日志输出 | stdout, file, both | stdout |
| 日志轮转 | 按大小（100MB）或按天 | 按天 |
| 日志保留 | 30 天（可配置） | 30 天 |
| 审计日志 | 始终启用 | - |

---

## 25. 预言机（Oracle）

### 25.1 设计

Callchain 原生支持验证者喂价系统，通过共识子集验证者定期提交价格数据，取中位数作为官方价格。价格数据通过 EVM 预编译合约提供 DeFi 合约直接读取。

### 25.2 核心数据结构

```rust
/// 验证者价格提交
struct OracleSubmission {
    asset_id: AssetId,
    price: u128,              // 以 CALL 计价，放大 18 位
    timestamp: u64,
    validator_id: ValidatorId,
    signature: Signature,
}

/// 聚合价格（取中位数）
struct AggregatedPrice {
    asset_id: AssetId,
    median_price: u128,       // 中位数价格
    valid_submissions: u32,    // 有效提交数
    timestamp: u64,            // 更新时间戳
    block_updated: u64,        // 更新的区块高度
}

/// 验证者预言机状态
struct OracleValidatorInfo {
    submission_count: u32,     // 累计提交次数
    outlier_count: u32,        // 偏离中位数 >5% 的次数
    is_active: bool,           // 是否有提交资格
    last_submission: u64,      // 最后一次提交时间
}

/// 历史价格（用于 TWAP）
struct HistoricalPrice {
    price: u128,
    timestamp: u64,
}
```

### 25.3 价格提交流程

```
每个价格更新周期（每 1000 区块 ≈ 4 分钟）：

1. 验证者从外部 API 获取价格
   → 数据源：CoinGecko、Binance、Coinbase（至少 2 个独立源）
   → 验证者本地计算：取多个源的中位数

2. 验证者签名并提交
   → 用 secp256k1 密钥签名：sign(hash(asset_id, price, timestamp))
   → 提交到 Oracle 系统合约

3. 系统合约收集提交
   → 等待 2/3 验证者提交（14/21）
   → 排除异常值（偏离中位数 >5% 的提交）
   → 计算中位数作为官方价格

4. 价格写入状态
   → AggregatedPrice 更新
   → 历史记录追加到 TWAP 队列
   → EVM 预编译合约自动暴露最新价格

5. 异常验证者标记
   → outlier_count +1
   → 连续 10 次异常 → 失去提交资格
```

```rust
/// 验证者提交价格
fn submit_oracle_price(
    submission: OracleSubmission,
) -> Result<()> {
    let validator = get_validator_info(submission.validator_id)?;

    // 1. 验证是否有提交资格
    ensure!(validator.oracle_info.is_active, "Oracle submission disabled");

    // 2. 验证签名
    verify_signature(
        &validator.consensus_key,
        &submission.signature,
        &submission.hash()
    )?;

    // 3. 验证时间窗口（当前周期内）
    let current_period = current_block_height() / ORACLE_UPDATE_INTERVAL;
    let submission_period = submission.timestamp / ORACLE_PERIOD_SECS;
    ensure!(submission_period == current_period, "Wrong submission period");

    // 4. 防重放（每周期每验证者只能提交一次）
    ensure!(
        !OracleSubmissions::has_submitted(
            submission.validator_id,
            submission.asset_id,
            current_period
        ),
        "Already submitted this period"
    );

    // 5. 记录提交
    OracleSubmissions::insert(
        submission.validator_id,
        submission.asset_id,
        current_period,
        submission.price,
    );

    OracleValidatorInfo::record_submission(submission.validator_id);

    // 6. 如果收集到足够的提交，触发聚合
    let submission_count = OracleSubmissions::count_for_asset(submission.asset_id, current_period);
    if submission_count >= oracle_quorum() {
        aggregate_and_publish_price(submission.asset_id, current_period)?;
    }

    Ok(())
}

/// 聚合价格：取中位数，排除异常值
fn aggregate_and_publish_price(asset_id: AssetId, period: u64) -> Result<()> {
    let submissions = OracleSubmissions::get_all(asset_id, period);

    // 排序
    let mut prices: Vec<u128> = submissions.iter().map(|s| s.price).collect();
    prices.sort();

    // 计算中位数
    let median = prices[prices.len() / 2];

    // 标记异常值（偏离中位数 >5%）
    for submission in &submissions {
        let deviation = ((submission.price as i128 - median as i128).abs() as u128) * 100 / median;
        if deviation > 5 {
            OracleValidatorInfo::mark_outlier(submission.validator_id);
        }
    }

    // 检查验证者是否因多次异常被禁用
    for submission in &submissions {
        let info = OracleValidatorInfo::get(submission.validator_id);
        if info.outlier_count >= 10 {
            info.is_active = false;
            OracleValidatorInfo::insert(submission.validator_id, info);
        }
    }

    // 更新聚合价格
    let aggregated = AggregatedPrice {
        asset_id,
        median_price: median,
        valid_submissions: submissions.len() as u32,
        timestamp: current_timestamp(),
        block_updated: current_block_height(),
    };
    AggregatedPrices::insert(asset_id, aggregated);

    // 追加 TWAP 历史记录
    PriceHistory::push(asset_id, HistoricalPrice {
        price: median,
        timestamp: current_timestamp(),
    });

    emit_event("PriceUpdated", asset_id, median, current_timestamp());
    Ok(())
}
```

### 25.4 EVM 预编译接口

DeFi 合约通过预编译合约读取价格数据：

```solidity
/// 预编译合约地址：0x0000...0101
interface ICallOracle {
    /// 获取最新价格
    /// @return price 以 CALL 计价（18 位小数）
    /// @return timestamp 价格更新时间戳
    function getPrice(bytes32 assetId)
        external view
        returns (uint256 price, uint256 timestamp);

    /// 获取时间加权平均价格（TWAP）
    /// @param window 时间窗口（秒）
    /// @return twap 时间加权平均价格
    function getTWAP(bytes32 assetId, uint256 window)
        external view
        returns (uint256 twap);

    /// 检查价格是否过期
    /// @param maxAge 最大允许年龄（秒）
    /// @return isStale true 表示价格已过期
    function isStale(bytes32 assetId, uint256 maxAge)
        external view
        returns (bool isStale);

    /// 获取预言机状态
    /// @return updateInterval 价格更新间隔（区块）
    /// @return quorum 法定人数
    function getOracleStatus()
        external view
        returns (uint256 updateInterval, uint256 quorum);
}
```

**使用示例：**

```solidity
contract MyDEX {
    ICallOracle public constant ORACLE = ICallOracle(0x0000000000000000000000000000000000000101);

    function swap(address tokenIn, uint256 amountIn) external {
        (uint256 price, uint256 timestamp) = ORACLE.getPrice(tokenIn);

        // 检查价格是否有效
        require(!ORACLE.isStale(tokenIn, 300), "Price too stale");

        // 计算输出金额
        uint256 amountOut = (amountIn * price) / 1e18;

        // 执行交换...
    }
}
```

### 25.5 配置参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| 更新间隔 | 1000 区块（~4 分钟） | 价格更新频率 |
| 法定人数 | 14（2/3 of 21） | 触发聚合的最小提交数 |
| 异常阈值 | 5% | 偏离中位数 >5% 标记为异常 |
| 异常容忍上限 | 10 次 | 累计 10 次异常失去提交资格 |
| TWAP 窗口上限 | 24 小时 | TWAP 最大查询范围 |
| 价格过期时间 | 15 分钟 | 超过此时间视为过期 |
| 初始数据源要求 | ≥2 个独立源 | 验证者必须从至少 2 个 API 获取价格 |

### 25.6 支持的资产

| 资产 | 数据源 | 优先级 |
|------|--------|--------|
| CALL/USD | CoinGecko, Binance | 最高（协议原生） |
| USDC/USD | CoinGecko, Binance, Coinbase | 高 |
| ETH/USD | CoinGecko, Binance, Coinbase | 高 |
| BTC/USD | CoinGecko, Binance | 中 |
| 其他协议资产 | 按需添加 | 低 |

---

## 26. 关键设计决策

### 25.1 为什么双账本而非单一账本

| 单一账本 | 双账本 |
|----------|--------|
| EVM 合约需要适配协议层 API | EVM 合约完全独立，无需适配 |
| 实现复杂（锁仓感知、状态同步） | 实现简单（两层互不干扰） |
| DeFi 合约需要修改 | DeFi 合约和以太坊一模一样 |
| 用户体验无缝 | 需要桥接操作（但可自动化） |

选择**双账本**：简单性 > 无缝体验。桥接操作可通过钱包自动化实现近乎无感的体验。

### 25.2 为什么选择 Commonware Simplex

- O(n) 通信复杂度，216 验证者下仅 216 条消息/轮（vs Tendermint 的 ~46K 条）
- 状态机最简洁 — 轮次、提议、投票、提交，四步完成一轮
- `commonware-consensus` crate 可直接使用，无需从零实现
- 子集轮换天然支持 — 每轮随机抽样提议者
- Tempo 链已在生产环境验证
- 审计面最小 — 代码量最少，形式化验证可行

### 25.3 为什么选择 Reth 而非自研执行引擎

- 2026 年 Reth 已是 EVM 执行的事实标准
- 完整的工具链（Foundry, Hardhat 兼容）
- 不需要重新实现 EVM 语义
- Reth 的模块化架构允许深度定制

---

## 附录 A：交易生命周期

```
1. 用户发起操作
   ├── 协议支付 → 签名 EvmTx（调用预编译地址）→ 广播到 mempool
   ├── EVM 交易 → 签名 EvmTx → 广播到 mempool
   └── 桥接操作 → 通过钱包自动创建 → 广播

2. 验证者收集交易
   ├── 标准优先级：EvmTx（含预编译调用）
   └── 内部队列：BridgeOp

3. 验证者打包区块
   ├── 执行 EVM 交易 → 更新 EVM 状态（预编译调用同步更新协议状态）
   ├── 执行桥接操作 → 同步两层余额
   └── 执行系统交易 → 奖励/费用结算

4. Simplex 共识出块
   ├── 提议者打包 → 广播提议
   ├── 验证者投票 → 2/3 多数
   └── 最终确认 → 区块不可回滚

5. 节点同步
   ├── 接收新区块
   ├── 验证签名和状态根
   └── 更新本地状态
```

## 附录 B：术语表

| 术语 | 定义 |
|------|------|
| Protocol Payment Layer | 协议支付层，维护原生余额映射 |
| EVM Contract Layer | EVM 智能合约层，运行智能合约 |
| Internal Bridge | 内部桥接，两层之间的资产转换机制 |
| Asset Registry | 资产注册表，记录所有协议资产 |
| CompliancePolicy | 合规策略，定义资产的转账限制 |
| BridgeOp | 桥接操作，在两层之间转移资产 |
| Simplex | 低延迟 BFT 共识算法，O(n) 通信复杂度 |
| Commonware | Simplex 共识的 Rust 实现框架 |
