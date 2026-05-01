# Callchain Internal Bridge

## Overview

The Internal Bridge manages asset flow between the **Protocol Payment Layer** (`AccountState`) and the **EVM Contract Layer** (`EvmState`) within a single Callchain node. It enables:

- **Deposits (Protocol → EVM):** Users bridge protocol balance into the EVM layer
- **Withdrawals (EVM → Protocol):** Users bridge EVM assets back to the protocol layer

Internal bridge operations are **atomic** and execute within a single block. If the EVM side fails, the protocol balance is rolled back automatically.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Internal Bridge (Protocol ↔ EVM)                          │
│                                                             │
│  ┌──────────────┐        ┌──────────────────────────────┐  │
│  │ AccountState │        │ EvmState                     │  │
│  │ (protocol)   │◄──────►│ (EVM layer)                  │  │
│  └──────────────┘        └──────────────────────────────┘  │
│         ▲                           ▲                       │
│         │    BridgeToEvm            │   BridgeToProtocol     │
│         │    (deduct protocol       │   (burn ERC-20 /      │
│         │     → mint ERC-20 or      │    deduct native       │
│         │      set native balance)  │    → credit protocol)  │
│         │                           │                       │
│  ┌──────┴───────────────────────────┴──────┐               │
│  │ Block::execute (Step 3)                  │               │
│  │  execute_bridge_precompile()             │               │
│  │  - snapshot/rollback atomicity           │               │
│  └──────────────────────────────────────────┘               │
│                                                             │
│  Two entry points:                                          │
│  1. BridgeOp in block.bridge_operations (agent/block lvl)  │
│  2. EVM transaction calling Switch precompile (user-init)  │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

---

## Asset Bridging Rules

The internal bridge treats assets differently based on `asset_id`:

| Asset ID | Asset Type | Deposit Behavior | Withdraw Behavior |
|----------|-----------|------------------|-------------------|
| `0` | Virtual USD | **Rejected** | **Rejected** |
| `1` | CALL (native) | Deduct protocol balance → **Add native EVM balance** (`evm_state.set_balance`) | Deduct native EVM balance → **Credit protocol balance** |
| `>=2` | User-defined asset | Deduct protocol balance → **Mint ERC-20 wrapped token** (`bridgeMint`) | **Burn ERC-20 wrapped token** (`bridgeBurn`) → Credit protocol balance |

**Key design decision:** CALL (asset_id=1) bridges as **native EVM gas balance**, not as a wrapped ERC-20. This allows bridged CALL to be used directly for EVM transaction gas and native transfers. User-defined assets are always bridged as wrapped ERC-20 contracts deployed via `EvmExecutor::deploy_erc20_template`.

### Supply Tracking

Bridge operations update `Asset.evm_supply` in `AssetRegistry`:

| Operation | `protocol_supply` | `evm_supply` | Source |
|-----------|------------------|-------------|--------|
| `BridgeToEvm` (deposit) | — | +amount | `registry.add_evm_supply()` |
| `BridgeToProtocol` (withdraw) | — | -amount | `registry.sub_evm_supply()` |
| `EvmIssuerMint` | — | +amount | `registry.add_evm_supply()` |

`evm_supply` is the protocol layer's tracked view of EVM circulation. It is used for cap enforcement (`all_supply() = protocol_supply + evm_supply`) but is not the source of truth for individual EVM balances — that remains `WrappedToken.balanceOf`.

---

## Precompile Alternative

The **Switch precompile at `0x207`** provides the same bridging functionality via standard EVM transactions:

| Operation | Precompile Function | Gas |
|---|---|---|
| `BridgeToEvm` | `switchToEvm(uint64,address,uint128)` | 30,000 |
| `BridgeToProtocol` | `switchToProtocol(uint64,address,uint128)` | 30,000 |

Solidity contracts and MetaMask can call these functions directly. See [precompile.md](precompile.md) for the full ABI.

## Entry Points

### 1. Block-Level BridgeOp (Agent / Block Producer)

`BridgeOp` variants are included in the dedicated `bridge_operations` field of `Block`. These are typically constructed by Agents or block producers for automated bridging workflows.

```rust
pub enum BridgeOp {
    DepositToEvm { asset_id, from, to, amount },
    WithdrawToProtocol { asset_id, from, to, amount },
}
```

Execution happens during `Block::execute` (Step 3, after EVM transactions and protocol transactions):

- `DepositToEvm`: `call_bridge::execute_deposit` deducts protocol balance, then mints EVM tokens or sets native balance.
- `WithdrawToProtocol`: `call_bridge::execute_withdraw` burns EVM tokens or deducts native balance, then credits protocol balance.

Both operations are rate-limited and support atomic rollback via snapshots.

### 2. User-Facing Precompile Calls

Ordinary users can initiate bridging by calling the Switch precompile (`0x207`) via a standard EVM transaction:

```solidity
// Protocol → EVM: deduct protocol balance, mint wrapped ERC-20
function switchToEvm(uint64 assetId, address to, uint128 amount) external returns (bool);

// EVM → Protocol: burn wrapped ERC-20, credit protocol balance
function switchToProtocol(uint64 assetId, address to, uint128 amount) external returns (bool);
```

These calls are executed by revm during `Block::execute`. The Switch precompile receives the caller address from the EVM context, accesses `AccountState` and `AssetRegistry` via the state hook, and performs the cross-layer transfer atomically.

**Gas cost:** Both `switchToEvm` and `switchToProtocol` cost **8,000 gas**.

---

## Inline Execution in Block::execute

### Bridge Precompile Detection

During block execution, bridge-related EVM calls to precompiles (`0x103`, `0x207`) are handled by the respective precompile functions. The Switch precompile (`0x207`) handles `switchToEvm` / `switchToProtocol`, while the Bridge precompile (`0x103`) handles `externalBridgeDeposit` / `externalBridgeWithdraw` / `challengeBridgeDeposit`.

Bridge precompiles receive:
- `caller`: the EVM caller address (from revm context)
- `account`: mutable `AccountState`
- `bridge_state`: mutable `BridgeStateManager`
- `config`: `BridgeConfig`
- `validators`: current validator set (for external bridge)
- `current_block_height`: current block number
- `evm_state`: mutable `EvmState`
- `evm_executor`: `EvmExecutor` for contract calls
- `registry`: `AssetRegistry` for contract address lookup

### BridgeToEvm Execution Flow

1. **Reject asset_id == 0** (virtual USD)
2. **Validate asset registered** in `AssetRegistry`
3. **Check bridge not paused** for this asset
4. **Check per-tx limit** against `BridgeConfig::max_per_tx`
5. **Check daily limit** — auto-resets per `blocks_per_day`
6. **Check protocol balance** sufficient
7. **Deduct protocol balance**
8. **Bridge to EVM:**
   - If `asset_id == 1`: `evm_state.set_balance(to, current + amount)`
   - If `asset_id >= 2`: `evm_executor.evm_call_bridge_mint(sender, contract_addr, evm_state, to, amount)`
9. **Record deposit** in `bridge_state`
10. If EVM operation reverts → entire transaction fails, snapshot rollback restores protocol balance

### BridgeToProtocol Execution Flow

1. **Reject asset_id == 0** (virtual USD)
2. **Validate asset registered**
3. **Check bridge not paused**
4. **Check per-tx limit**
5. **Check daily limit**
6. **Withdraw from EVM:**
   - If `asset_id == 1`: check `evm_state.get_balance(sender) >= amount`, then `evm_state.set_balance(sender, balance - amount)`
   - If `asset_id >= 2`: `evm_executor.evm_call_bridge_burn(sender, contract_addr, evm_state, amount)`
7. **Credit protocol balance** to `to`
8. **Record withdrawal** in `bridge_state`
9. If EVM operation reverts → entire transaction fails, snapshot rollback

### Atomic Rollback

`Block::execute` takes snapshots before processing each transaction:

```rust
let balance_snapshot = account.clone();
let evm_snapshot = evm_state.clone();
let bridge_snapshot = bridge_state.clone();
```

If any bridge precompile call fails, all three states are restored:

```rust
*account = balance_snapshot;
*evm_state = evm_snapshot;
*bridge_state = bridge_snapshot;
```

This ensures that a failed `BridgeToEvm` does not leave the user's protocol balance deducted without corresponding EVM credit, and a failed `BridgeToProtocol` does not burn EVM tokens without protocol credit.

---

## RPC Endpoints

### `call_bridgeToEvm`

Submit a user-initiated bridge from protocol to EVM.

**Parameters:**
```json
{
  "sender": "0x...",
  "to": "0x...",
  "assetId": 1,
  "amount": "1000000000000000000",
  "nonce": 123,
  "signature": "0x..."
}
```

**Response:**
```json
{
  "txHash": "0x...",
  "status": "pending"
}
```

### `call_bridgeToProtocol`

Submit a user-initiated withdrawal from EVM to protocol.

**Parameters:**
```json
{
  "sender": "0x...",
  "to": "0x...",
  "assetId": 1,
  "amount": "1000000000000000000",
  "nonce": 123,
  "signature": "0x..."
}
```

Both endpoints build an EVM transaction calling the Switch precompile (`0x207`) with:
- `gas_limit = 30_000`
- Standard EIP-1559 fee fields
- ABI-encoded `switchToEvm` or `switchToProtocol` call data

The signature is a standard Ethereum ECDSA signature over the RLP-encoded EVM transaction.

---

## Python Test Helpers

### `tests/signer.py`

```python
def sign_bridge_to_evm(
    private_key: str,
    sender: str,
    nonce: int,
    asset_id: int,
    to: str,
    amount: int,
    gas_limit: int = 25_000,
    max_fee: int = 250_000,
) -> dict:
    """Build a signed BridgeToEvm payload."""

def sign_bridge_to_protocol(
    private_key: str,
    sender: str,
    nonce: int,
    asset_id: int,
    to: str,
    amount: int,
    gas_limit: int = 25_000,
    max_fee: int = 250_000,
) -> dict:
    """Build a signed BridgeToProtocol payload."""
```

Both helpers:
1. Build an EVM transaction calling the Switch precompile (`0x207`)
2. Compute `tx_hash` via `compute_tx_hash()` (matching Rust canonical hash)
3. Sign with `sign_raw()` (raw secp256k1, **not** EIP-191)
4. Return a dict ready for the RPC client

### `tests/rpc_client.py`

```python
def bridge_to_evm(self, params: Dict) -> Dict:
    return self._call("call_bridgeToEvm", [params])

def bridge_to_protocol(self, params: Dict) -> Dict:
    return self._call("call_bridgeToProtocol", [params])
```

---

## Rate Limiting & Safety

The internal bridge shares `BridgeStateManager` rate limits with the external bridge:

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `max_per_tx` | 1,000 tokens | Maximum single bridge amount |
| `daily_limit_per_asset` | 10,000 tokens | Daily volume cap per asset |
| `blocks_per_day` | 345,600 | ~1 day at 250ms block time (daily limit reset cadence) |

**Daily usage auto-reset:** `check_and_update_daily_limit` clears usage when `current_block >= daily_usage_reset_at + blocks_per_day`.

**Bridge pause:** Individual assets can be paused via `bridge_state.pause_asset(asset_id)`. Paused assets reject all bridge operations.

---

## File Map

| File | Role |
|------|------|
| `crates/bridge/src/deposit.rs` | `execute_deposit` — Protocol → EVM (BridgeOp::DepositToEvm) |
| `crates/bridge/src/withdraw.rs` | `execute_withdraw` — EVM → Protocol (BridgeOp::WithdrawToProtocol) |
| `crates/consensus/src/block.rs` | `execute_bridge_precompile` — inline execution for user bridge precompile calls |
| `crates/precompiles/src/bridge.rs` | Bridge precompile functions (`externalBridgeDeposit`, `externalBridgeWithdraw`, `challengeBridgeDeposit`) |
| `crates/precompiles/src/switch.rs` | Switch precompile functions (`switchToEvm`, `switchToProtocol`) |
| `crates/rpc/src/callchain.rs` | `call_bridgeToEvm` and `call_bridgeToProtocol` RPC handlers |
| `tests/signer.py` | Python signing helpers for EVM bridge transactions |
| `tests/rpc_client.py` | Python RPC client methods for bridge endpoints |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| CALL native bridging | Ready | asset_id==1 uses `evm_state.set_balance` directly; no ERC-20 contract needed |
| User asset ERC-20 bridging | Ready | Requires `AssetRegistry::evm_contract_address` registration; `evm_call_bridge_mint` / `evm_call_bridge_burn` |
| Virtual USD rejection | Ready | asset_id==0 explicitly rejected in both deposit and withdraw paths |
| Atomic rollback | Ready | Snapshot of account + evm_state + bridge_state before each tx; full restore on failure |
| Rate limiting | Ready | Per-tx and daily limits enforced; auto-reset per `blocks_per_day` |
| Bridge pause | Ready | Per-asset pause via `BridgeStateManager` |
| User-facing RPC | Ready | `call_bridgeToEvm` and `call_bridgeToProtocol` with signature verification and mempool submission |
| EVM issuer mint | Ready | `evmIssuerMint` precompile + `WrappedToken.issuerMint`; cap enforced at protocol layer |
| E2E test helpers | Ready | Python signing and RPC wrappers in `tests/signer.py` and `tests/rpc_client.py` |
