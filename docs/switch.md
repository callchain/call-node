# Callchain Internal Bridge

## Overview

The Internal Bridge manages asset flow between the **Protocol Payment Layer** (`AccountState`) and the **EVM Contract Layer** (`EvmState`) within a single Callchain node. It enables:

- **Deposits (Protocol → EVM):** Users bridge protocol balance into the EVM layer
- **Withdrawals (EVM → Protocol):** Users bridge EVM assets back to the protocol layer

Internal bridge operations are **atomic** and execute within a single block. If the EVM side fails, the protocol balance is rolled back automatically.

---

## Switch Precompile (`0x207`)

Address: `0x0000000000000000000000000000000000000207`

The Switch precompile provides bidirectional bridging between protocol balance and EVM assets using an **escrow model**. All state changes are atomic via checkpoint/rollback.

### ABI Interface

```solidity
interface IProtocolSwitch {
    function switchToEvm(uint64 assetId, address to, uint128 amount) external;
    function switchToProtocol(uint64 assetId, address to, uint128 amount) external;
}
```

### Key Structures

#### `SwitchStorage`

Wraps a `StorageBackend` to read/write protocol-side state stored under `ASSET_ADDRESS` (`0x201`):

| Method | Purpose |
|--------|---------|
| `load_protocol_bal(assetId, addr)` | Read protocol balance from `slot_balance(assetId, addr)` |
| `add_protocol_bal(assetId, addr, amount)` | Increment balance with overflow check |
| `sub_protocol_bal(assetId, addr, amount)` | Decrement balance with underflow check |
| `read_evm_contract(assetId)` | Read the bound ERC-20 contract address |
| `check_asset_active(assetId)` | Verify `status == 0` (Active) |
| `check_has_erc20(assetId)` | Verify `has_erc20 == 1` (CALL asset_id=1 always passes) |

#### `StorageProviderDb`

Implements revm's `Database` trait so a nested EVM instance can read from the current execution context:

- `basic(address)` → returns `AccountInfo` (balance, code, code_hash) via `raw_balance_get` / `raw_code_get`
- `storage(address, index)` → returns storage value via `raw_sload`
- Uses `raw_*` methods to avoid double-counting gas

#### `execute_evm_call(db, contract, data)`

Runs a temporary revm instance to execute an actual ERC-20 call:

1. Build `TxEnv` with `caller = 0x207`, `gas_limit = 100_000`, `kind = Call(contract)`
2. Create mainnet revm context with `StorageProviderDb` as database
3. Set block env (number, timestamp) from outer context
4. Execute `transact(tx_env)`
5. Extract `gas.spent()` from result and deduct from outer provider budget
6. Return `(ExecutionResult, EvmState)`

#### `apply_state_changes(storage, state)`

Syncs the nested EVM's `EvmState` back to the outer `StorageProvider`:

- For each touched account, iterate storage diffs: if `present_value != original_value`, call `raw_sstore(address, slot, present_value)`
- For balance changes: if `new_balance > old_balance`, call `balance_add`; else `balance_sub`
- Uses `raw_sstore` to bypass gas accounting (gas already tracked by inner EVM)

### Method Details

#### `switchToEvm(assetId, to, amount)` — Protocol → EVM

- **Flow**:
  1. Validate `amount > 0` and `to != Address::ZERO`
  2. `check_asset_active(assetId)` — asset must be Active
  3. `check_has_erc20(assetId)` — must support EVM bridge (or `assetId == 1` for CALL)
  4. **`sub_protocol_bal(assetId, caller, amount)`** — deduct caller's protocol balance
  5. **Credit EVM side**:
     - If `assetId == 1` (CALL): `balance_add(to, amount)` — native EVM balance
     - Otherwise (ERC-20):
       - `code_get(contract)` — verify contract has code
       - Build `transfer(to, amount)` calldata
       - `execute_evm_call()` — run nested EVM call from `0x207`
       - Verify result is `Success` and returns `true` (`decode_abi_bool`)
       - `apply_state_changes()` — write nested EVM state diffs back
  6. `checkpoint_commit()` on success; `checkpoint_revert()` on any failure
- **Gas**: 20000 + nested EVM call gas
- **Escrow requirement**: `0x207` must hold enough ERC-20 tokens. If escrow balance is insufficient, the nested `transfer` reverts and the entire operation rolls back (protocol balance restored).

#### `switchToProtocol(assetId, to, amount)` — EVM → Protocol

- **Flow**:
  1. Validate `amount > 0` and `to != Address::ZERO`
  2. `check_asset_active(assetId)`
  3. `check_has_erc20(assetId)`
  4. **Deduct EVM side**:
     - If `assetId == 1` (CALL): `balance_sub(caller, amount)` — native EVM balance
     - Otherwise (ERC-20):
       - `code_get(contract)` — verify contract has code
       - Build `transferFrom(caller, 0x207, amount)` calldata
       - `execute_evm_call()` — run nested EVM call
       - Verify result is `Success` and returns `true`
       - `apply_state_changes()` — write state diffs back
  5. **`add_protocol_bal(assetId, to, amount)`** — credit recipient's protocol balance
  6. Atomic commit or rollback
- **Gas**: 20000 + nested EVM call gas
- **Approve requirement**: User must first call `approve(0x207, amount)` on the ERC-20 contract. Without approval, `transferFrom` reverts and the entire operation rolls back.

### Atomicity Guarantee

Both methods execute inside `dispatch::mutate_void`:

```rust
let checkpoint = storage.checkpoint();
let result = handler(decoded, storage);
if result.is_ok() && gas_sufficient {
    storage.checkpoint_commit(checkpoint);
} else {
    storage.checkpoint_revert(checkpoint);
}
```

This means:
- `switchToEvm`: if protocol balance was deducted but ERC-20 `transfer` fails → **all state restored**
- `switchToProtocol`: if ERC-20 `transferFrom` succeeds but protocol balance credit fails → **all state restored**

---

## Architecture

The bridge uses an **escrow model**: the Switch precompile address (`0x207`) holds ERC-20 tokens in its own balance. Protocol balance and EVM balance are kept synchronized by moving tokens into and out of this escrow.

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
│         │    (deduct protocol       │   (transferFrom to     │
│         │     → transfer ERC-20     │    0x207 escrow        │
│         │      from 0x207 escrow)   │    → credit protocol)  │
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

The internal bridge treats assets differently based on `asset_id` and `has_erc20`:

| Asset ID | Asset Type | `has_erc20` | Deposit Behavior | Withdraw Behavior |
|----------|-----------|-------------|------------------|-------------------|
| `0` | Virtual USD | — | **Rejected** | **Rejected** |
| `1` | CALL (native) | — | Deduct protocol balance → **Add native EVM balance** (`evm_state.set_balance`) | Deduct native EVM balance → **Credit protocol balance** |
| `>=2` | Protocol-only | `0` | **Rejected** — `AssetHasNoErc20Bridge` | **Rejected** — `AssetHasNoErc20Bridge` |
| `>=2` | ERC-20 backed | `1` | Deduct protocol balance → **Transfer ERC-20 from `0x207` escrow to user** (`transfer`) | **Transfer ERC-20 from user to `0x207` escrow** (`transferFrom`) → Credit protocol balance |

**Key design decision:** CALL (asset_id=1) bridges as **native EVM gas balance**, not as a wrapped ERC-20. This allows bridged CALL to be used directly for EVM transaction gas and native transfers. User-defined assets must be registered via `registerErc20` (setting `has_erc20 = 1`) to enable switching. Protocol-only assets registered via `register` (`has_erc20 = 0`) cannot leave the protocol layer.

### Escrow Model

The Switch precompile (`0x207`) acts as an escrow holder for ERC-20 backed assets:

- **Deposit (`switchToEvm`)**: The protocol balance is deducted from the sender, and the Switch precompile transfers ERC-20 tokens from its own escrow balance to the recipient. The escrow must hold sufficient tokens, or the transfer reverts.
- **Withdraw (`switchToProtocol`)**: The user's ERC-20 tokens are transferred into the `0x207` escrow via `transferFrom`. The user must first `approve(0x207, amount)` on the ERC-20 contract. After the tokens are received, the protocol balance is credited to the recipient.

### Liquidity Requirement

ERC-20 escrow bridging requires the Switch precompile (`0x207`) to hold a token balance before any `switchToEvm` deposit can succeed. Liquidity is injected into escrow through:

1. **User-initiated withdrawals**: `switchToProtocol` transfers tokens from the user into `0x207`.
2. **Direct transfer**: Anyone can `transfer(0x207, amount)` on the ERC-20 contract to seed the pool.

If escrow balance is insufficient, `switchToEvm` reverts atomically and no state changes are committed.

---

## Precompile Alternative

The **Switch precompile at `0x207`** provides the same bridging functionality via standard EVM transactions, with an important restriction: **only ERC-20 backed assets (`has_erc20 == 1`) and CALL (`asset_id == 1`) are accepted**. Protocol-only assets (`has_erc20 == 0`) are rejected with `AssetHasNoErc20Bridge`.

| Operation | Precompile Function | Gas | Restrictions |
|---|---|---|---|
| `SwitchToEvm` | `switchToEvm(uint64,address,uint128)` | 30,000 | `asset_id == 1` or `has_erc20 == 1` |
| `SwitchToProtocol` | `switchToProtocol(uint64,address,uint128)` | 30,000 | `asset_id == 1` or `has_erc20 == 1` |

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

- `DepositToEvm`: `call_bridge::execute_deposit` deducts protocol balance, then transfers ERC-20 from the `0x207` escrow to the recipient (or sets native balance for CALL).
- `WithdrawToProtocol`: `call_bridge::execute_withdraw` transfers ERC-20 from the user into the `0x207` escrow via `transferFrom` (or deducts native balance for CALL), then credits protocol balance.

Both operations are rate-limited and support atomic rollback via snapshots.

### 2. User-Facing Precompile Calls

Ordinary users can initiate bridging by calling the Switch precompile (`0x207`) via a standard EVM transaction:

```solidity
// Protocol → EVM: deduct protocol balance, transfer ERC-20 from 0x207 escrow
function switchToEvm(uint64 assetId, address to, uint128 amount) external returns (bool);

// EVM → Protocol: transfer ERC-20 from user to 0x207 escrow, credit protocol balance
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
3. **Check `has_erc20`** — reject protocol-only assets (`has_erc20 == 0` and `asset_id != 1`)
4. **Check bridge not paused** for this asset
5. **Check per-tx limit** against `BridgeConfig::max_per_tx`
6. **Check daily limit** — auto-resets per `blocks_per_day`
7. **Check protocol balance** sufficient
8. **Deduct protocol balance**
9. **Bridge to EVM:**
   - If `asset_id == 1`: `evm_state.set_balance(to, current + amount)`
   - If `asset_id >= 2`: nested EVM call `ERC20.transfer(to, amount)` from `0x207` escrow
9. **Record deposit** in `bridge_state`
10. If EVM operation reverts → entire transaction fails, snapshot rollback restores protocol balance

### BridgeToProtocol Execution Flow

1. **Reject asset_id == 0** (virtual USD)
2. **Validate asset registered**
3. **Check `has_erc20`** — reject protocol-only assets
4. **Check bridge not paused**
5. **Check per-tx limit**
6. **Check daily limit**
7. **Withdraw from EVM:**
   - If `asset_id == 1`: check `evm_state.get_balance(sender) >= amount`, then `evm_state.set_balance(sender, balance - amount)`
   - If `asset_id >= 2`: nested EVM call `ERC20.transferFrom(sender, 0x207, amount)` to move tokens into escrow
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
| `crates/bridge/src/precompile.rs` | Bridge precompile functions (`externalBridgeDeposit`, `externalBridgeWithdraw`, `challengeBridgeDeposit`) |
| `crates/switch/src/precompile.rs` | Switch precompile functions (`switchToEvm`, `switchToProtocol`) |
| `crates/rpc/src/callchain.rs` | `call_bridgeToEvm` and `call_bridgeToProtocol` RPC handlers |
| `tests/signer.py` | Python signing helpers for EVM bridge transactions |
| `tests/rpc_client.py` | Python RPC client methods for bridge endpoints |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| CALL native bridging | Ready | asset_id==1 uses `evm_state.set_balance` directly; no ERC-20 contract needed |
| User asset ERC-20 bridging | Ready | Requires `registerErc20` (sets `has_erc20 = 1` and `evm_contract_address`); nested EVM `transfer`/`transferFrom` via escrow model |
| Virtual USD rejection | Ready | asset_id==0 explicitly rejected in both deposit and withdraw paths |
| Atomic rollback | Ready | Snapshot of account + evm_state + bridge_state before each tx; full restore on failure |
| Rate limiting | Ready | Per-tx and daily limits enforced; auto-reset per `blocks_per_day` |
| Bridge pause | Ready | Per-asset pause via `BridgeStateManager` |
| User-facing RPC | Ready | `call_bridgeToEvm` and `call_bridgeToProtocol` with signature verification and mempool submission |
| E2E test helpers | Ready | Python signing and RPC wrappers in `tests/signer.py` and `tests/rpc_client.py` |
