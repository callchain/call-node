# Asset System

## Overview

The Callchain asset system supports both protocol-native assets (registered on-chain via consensus) and their corresponding ERC-20 wrapped representations on the EVM layer. This document describes the asset registration flow, EVM bridge registration, and the invariant guarantees between protocol and EVM state.

## Asset Registration

Assets are registered through a protocol transaction containing an `Instruction::RegisterAsset`. This ensures the operation is executed atomically during block execution, is ordered relative to other transactions, and is subject to the same consensus, fee, and nonce rules as all other protocol instructions.

### Instruction

```rust
Instruction::RegisterAsset {
    symbol: String,
    name: String,
    decimals: u8,
}
```

### Execution Flow

During block execution, `execute_protocol_instructions` processes `RegisterAsset` as follows:

1. **Fee collection**: Deduct `asset_registration_fee` (in CALL, asset_id = 1) from the sender's balance. The fee amount is read from `GovernanceManager::config.asset_registration_fee`.
2. **Asset allocation**: Register the asset in `AssetRegistry`:
   - Allocate a monotonically increasing `asset_id` (starting from 1).
   - Store metadata: `symbol`, `name`, `decimals`, `issuer = sender`, `total_supply = 0`, `status = Active`, `compliance_policy = 0`, `registered_at = current_block_height`.
3. **EVM wrapped token deployment** (if `register_evm_bridge` is enabled): The system deployer (`Address::repeat_byte(0xFF)`) deploys a `WrappedToken` contract on the EVM layer with:
   - `name = asset.name`
   - `symbol = asset.symbol`
   - `decimals = asset.decimals`
   - `initial_supply = 0`
4. **EVM contract address binding**: Set `asset.evm_contract_address` to the deployed contract address. This binding is immutable once set.

All four steps are atomic. If any step fails, the transaction reverts and no partial state is committed.

### Why a Transaction Instead of Direct RPC

The legacy `call_registerAsset` RPC handler wrote directly to `AssetRegistry` memory without constructing a `ProtocolTransaction`. This approach had the following issues:

- **No consensus ordering**: Each node processed the registration independently, leading to potential state divergence under concurrent registrations.
- **No atomic rollback**: If the RPC handler crashed or was interrupted, the registry could be left in an inconsistent state.
- **No fee/nonce enforcement**: The registration bypassed the standard transaction validation pipeline.

Moving registration into an `Instruction` solves all of these by leveraging the existing transaction execution and block-building infrastructure.

### RPC Interface

`call_registerAsset` is retained as a user-facing convenience RPC, but its implementation changes:

```json
POST /{
  "jsonrpc": "2.0",
  "method": "call_registerAsset",
  "params": {
    "symbol": "MYTOKEN",
    "name": "My Token",
    "decimals": 18,
    "sender": "0x...",
    "nonce": 42,
    "signature": "0x..."
  },
  "id": 1
}
```

The RPC handler:
1. Parses parameters and validates the EIP-191 signature.
2. Constructs a `ProtocolTransaction` containing a single `Instruction::RegisterAsset`.
3. Inserts the transaction into the mempool via `state.insert_protocol_tx(tx)`.
4. Returns the pending `txHash`.

The actual registration is executed when the transaction is included in a block.

## Asset Metadata

```rust
pub struct Asset {
    pub id: AssetId,                        // protocol asset identifier
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub issuer: Address,                    // who registered the asset
    pub total_supply: Balance,
    pub status: AssetStatus,                // Active | Frozen | Delisted
    pub compliance_policy: u8,
    pub registered_at: u64,
    pub evm_contract_address: Option<Address>, // set at registration time, immutable
}
```

## EVM Bridge Registration

For assets that require a custom EVM contract (rather than the system-deployed `WrappedToken`), a separate `RegisterEvmBridge` instruction is used. This is an opt-in path for advanced users who deploy their own ERC-20 contracts on EVM and want to link them to an existing protocol asset.

### Instruction

```rust
Instruction::RegisterEvmBridge {
    asset_id: AssetId,
    evm_contract_address: Address,
}
```

### Execution Flow

1. **Issuer verification**: `sender == asset.issuer`.
2. **Immutability check**: `asset.evm_contract_address` must be `None` (already bound assets cannot be relinked).
3. **Contract validation** (EVM static call): Call `totalSupply()` on the EVM contract at `evm_contract_address`. Reject if `totalSupply > 0`. This guarantees the EVM contract starts with zero supply, ensuring the bridge can maintain the invariant:
   ```
   protocol_asset_total_minted_to_evm == evm_wrapped_token_total_supply
   ```
4. **Optional code-hash whitelist** (governance-configurable): Verify the deployed bytecode hash matches an approved template. If disabled, any contract that exposes `bridgeMint(address,uint256)` and `bridgeBurn(uint256)` is accepted.
5. **Binding**: Set `asset.evm_contract_address = evm_contract_address`.

### Security Guarantees

- Only the asset issuer can link an EVM contract.
- The link is immutable: once set, it cannot be changed by anyone (including the issuer). If the wrong address is registered, the bridge operations for that asset will revert, and the asset must be delisted or governance must intervene.
- The EVM contract must start with `totalSupply == 0`. This prevents an attacker from linking a pre-minted contract and using `WithdrawFromEvm` to drain protocol balances.
- The `bridgeMint` function on the EVM contract must be restricted to the protocol bridge address (`Address::repeat_byte(0xFF)` or a configurable bridge address). Otherwise, anyone could mint EVM tokens arbitrarily, breaking the bridge invariant.

## Bridge Invariants

For any asset with an active EVM bridge:

```
sum(protocol balance deductions via BridgeToEvm)
  == sum(protocol balance credits via WithdrawFromEvm)
  == evm_wrapped_token.totalSupply()
```

This invariant is maintained by the bridge execution logic in `execute_bridge_instruction`:

- `BridgeToEvm`: Deduct protocol balance, then call `bridgeMint` on the EVM contract (via the bridge address).
- `WithdrawFromEvm`: Call `bridgeBurn` on the EVM contract (burning the sender's EVM balance), then credit protocol balance.

## EVM Wrapped Token Reference Template

The system provides a reference `WrappedToken.sol` contract that satisfies the bridge requirements:

```solidity
contract WrappedToken {
    string public name;
    string public symbol;
    uint8 public decimals;
    uint256 public totalSupply;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    address public bridge;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    constructor(string memory _name, string memory _symbol, uint8 _decimals) {
        name = _name;
        symbol = _symbol;
        decimals = _decimals;
        totalSupply = 0;
        bridge = msg.sender; // set to system deployer, later restricted
    }

    function transfer(address _to, uint256 _value) public returns (bool) { /* ... */ }
    function approve(address _spender, uint256 _value) public returns (bool) { /* ... */ }
    function transferFrom(address _from, address _to, uint256 _value) public returns (bool) { /* ... */ }

    function bridgeMint(address _to, uint256 _value) public {
        require(msg.sender == bridge, "only bridge");
        totalSupply += _value;
        balanceOf[_to] += _value;
        emit Transfer(address(0), _to, _value);
    }

    function bridgeBurn(uint256 _value) public {
        require(balanceOf[msg.sender] >= _value, "insufficient balance");
        totalSupply -= _value;
        balanceOf[msg.sender] -= _value;
        emit Transfer(msg.sender, address(0), _value);
    }
}
```

Note: The `bridge` address is set at construction time to the system deployer. During `RegisterAsset` execution, the bridge execution path must use this same address as the `caller` for `bridgeMint` calls.

## Governance Parameters

The following parameters affect asset registration and can be updated via governance:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `asset_registration_fee` | `1_000_000` CALL | Fee paid by the issuer to register an asset |
| `register_evm_bridge` | `true` | Whether to auto-deploy a wrapped token on EVM during registration |
| `evm_bridge_whitelist_required` | `false` | Whether `RegisterEvmBridge` requires the contract bytecode to match a whitelist |

## Delisting and Lifecycle

Assets can transition through the following states:

- **Active**: All operations permitted.
- **Frozen**: Transfers, mints, and burns are blocked. Bridge operations are paused.
- **Delisted**: The asset is permanently removed from the active registry. Existing balances remain in `AccountState` but the asset cannot be used in new transactions.

State transitions are controlled by the asset issuer (via `Instruction::UpdateCompliance`) or by governance (via `GovernanceEmergencyPause`).

## Roadmap / Enhancement: Convert Direct-State RPCs to Instruction-Based Transactions

Several RPC endpoints currently modify shared state directly (bypassing the transaction mempool and consensus pipeline). This causes state divergence across nodes, removes atomic rollback guarantees, and skips fee/nonce enforcement. The following RPCs should be converted to use `Instruction`-based `ProtocolTransaction` flows.

### Asset and Payment Layer

| RPC | Current Behavior | Target Instruction | Files Affected |
|-----|------------------|-------------------|----------------|
| `call_registerAsset` | Directly writes `asset_registry` and `balance_state` | `Instruction::RegisterAsset` | `crates/rpc/src/callchain.rs`, `crates/protocol/src/instructions.rs`, `crates/consensus/src/block.rs` |
| `call_sendPayment` | Inserts into mempool but executes immediately in RPC | Standard `Instruction::Transfer` via `state.insert_protocol_tx` | `crates/rpc/src/callchain.rs`, `crates/rpc/src/handlers.rs` |

### Agent Layer

| RPC | Current Behavior | Target Instruction | Files Affected |
|-----|------------------|-------------------|----------------|
| `call_agentRegister` | Directly writes `agent_registry` and `balance_state` | `Instruction::AgentRegister` | `crates/rpc/src/callchain.rs`, `crates/protocol/src/instructions.rs`, `crates/consensus/src/block.rs` |
| `call_agentGrant` | Directly writes `balance_state` and `agent_balances` | `Instruction::AgentGrant` | `crates/rpc/src/callchain.rs`, `crates/protocol/src/instructions.rs`, `crates/consensus/src/block.rs` |
| `call_agentRevoke` | Directly writes `agent_balances` | `Instruction::AgentRevoke` | `crates/rpc/src/callchain.rs`, `crates/protocol/src/instructions.rs`, `crates/consensus/src/block.rs` |

### Consensus / Governance Layer

| RPC | Current Behavior | Target Instruction | Files Affected |
|-----|------------------|-------------------|----------------|
| `call_submitRollbackSignature` | Directly writes `fork_manager` and `pending_rollback` | `Instruction::SubmitRollbackSignature` | `crates/rpc/src/callchain.rs`, `crates/protocol/src/instructions.rs`, `crates/consensus/src/block.rs` |

### Implementation Checklist

1. **Define new `Instruction` variants** in `crates/protocol/src/instructions.rs`.
2. **Add execution logic** in `execute_protocol_instructions` (and `execute_bridge_instruction` / `execute_validator_instruction` where applicable) in `crates/consensus/src/block.rs`.
3. **Update RPC handlers** in `crates/rpc/src/callchain.rs` to construct `ProtocolTransaction` and call `state.insert_protocol_tx(tx)` instead of direct state writes.
4. **Update `submit_payment`** in `crates/rpc/src/handlers.rs` to remove immediate execution; rely on the normal block-building pipeline.
5. **Add tests** for each new instruction in block execution and E2E tests.
6. **Update docs** (`rpc.md`, `protocol.md`) to reflect the new transaction-based flows.

### Bridge Execution Fix: Fixed Bridge Address for `bridgeMint`

With the introduction of `onlyBridge` access control on `WrappedToken.sol`, `BridgeToEvm` must use a fixed protocol bridge address as the EVM caller instead of the transaction sender.

| Change | Current | Target | Files Affected |
|--------|---------|--------|----------------|
| `BridgeToEvm` caller for `bridgeMint` | `sender` (protocol user address) | `BRIDGE_EVM_ADDRESS` (e.g. `Address::repeat_byte(0xFF)`) | `crates/consensus/src/block.rs`, `crates/protocol/src/lib.rs` |
| `WrappedToken` constructor | `bridge = msg.sender` | Accept `bridge` as constructor argument | `crates/evm/contracts/WrappedToken.sol` |
| Genesis CALL deployment | Auto-deploys `WrappedToken` for asset_id == 1 | Remove (CALL bridges as native EVM balance) | `crates/chainspec/src/genesis.rs` |

**Details:**

- `evm_call_bridge_mint` must be called with `caller = BRIDGE_EVM_ADDRESS` so that `WrappedToken.bridgeMint` passes the `onlyBridge` check.
- `WithdrawFromEvm` does **not** need this change because `bridgeBurn` burns the caller's own balance and requires no special access control.
- A protocol-level constant `BRIDGE_EVM_ADDRESS` should be defined (e.g. in `crates/protocol/src/lib.rs`) and used consistently across genesis deployment, instruction execution, and any system-initiated EVM calls.

### Why This Matters

- **Atomicity**: Direct state writes cannot be rolled back if a subsequent step fails. Instructions are executed atomically during block building.
- **Consensus**: Only transactions included in a block are agreed upon by validators. Direct writes create divergent local state.
- **Replay protection**: The transaction pipeline enforces nonces, fees, and mempool defense rules. Direct writes bypass all of these.
- **Auditability**: Instruction execution produces receipts and events. Direct writes leave no trace.

## Related Documents

- [internal_bridge.md](internal_bridge.md) — BridgeToEvm and WithdrawFromEvm execution details
- [protocol.md](protocol.md) — Instruction execution and atomicity guarantees
- [transaction.md](transaction.md) — Transaction format, fees, and nonce rules
- [rpc.md](rpc.md) — RPC method reference
