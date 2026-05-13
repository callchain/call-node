# Callchain Internal Bridge

## Overview

The Internal Bridge manages asset flow between the **Protocol Payment Layer** (`AccountState`) and the **EVM Contract Layer** (`EvmState`) within a single Callchain node. It enables:

- **Deposits (Protocol → EVM):** Users bridge protocol balance into the EVM layer
- **Withdrawals (EVM → Protocol):** Users bridge EVM assets back to the protocol layer

Internal bridge operations are **atomic** and execute within a single block. If the EVM side fails, the protocol balance is rolled back automatically.

---

## Switch Precompile (`0x207`)

Address: `0x0000000000000000000000000000000000000207`

The Switch precompile provides bidirectional bridging between protocol balance and EVM assets. Two models are supported depending on the asset's `dominance`:

- **Escrow model** (`dominance = 0`, EVM-dominant): The Switch precompile (`0x207`) holds ERC-20 tokens in its own balance. Used for external ERC-20 contracts registered via `registerErc20`.
- **Mint/burn model** (`dominance = 1`, PROTOCOL-dominant): The Switch precompile creates or destroys ERC-20 tokens in real time via `bridgeMint`/`bridgeBurn`. Used for system wrapper contracts deployed via `createWrapper`.

All state changes are atomic via checkpoint/rollback.

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
       - Read `dominance` from asset metadata
       - **If `dominance == 0` (EVM)**: Build `transfer(to, amount)` calldata — transfer from `0x207` escrow
       - **If `dominance == 1` (PROTOCOL)**: Build `bridgeMint(to, amount)` calldata — mint new tokens
       - `execute_evm_call()` — run nested EVM call from `0x207`
       - Verify result is `Success`
       - For escrow (`dominance == 0`): check `decode_abi_bool` on return data (ERC-20 `transfer` returns `bool`)
       - For mint/burn (`dominance == 1`): skip bool check (`bridgeMint` is void, no return value)
       - `apply_state_changes()` — write nested EVM state diffs back
  6. `checkpoint_commit()` on success; `checkpoint_revert()` on any failure
- **Gas**: 20000 + nested EVM call gas
- **Escrow requirement (EVM-dominant)**: `0x207` must hold enough ERC-20 tokens. If escrow balance is insufficient, the nested `transfer` reverts and the entire operation rolls back (protocol balance restored).
- **Mint requirement (PROTOCOL-dominant)**: No escrow needed. `bridgeMint` creates new tokens. The `WrappedToken` contract enforces `msg.sender == bridge` (i.e., `0x207`).

#### `switchToProtocol(assetId, to, amount)` — EVM → Protocol

- **Flow**:
  1. Validate `amount > 0` and `to != Address::ZERO`
  2. `check_asset_active(assetId)`
  3. `check_has_erc20(assetId)`
  4. **Deduct EVM side**:
     - If `assetId == 1` (CALL): `balance_sub(caller, amount)` — native EVM balance
     - Otherwise (ERC-20):
       - `code_get(contract)` — verify contract has code
       - Read `dominance` from asset metadata
       - **If `dominance == 0` (EVM)**: Build `transferFrom(caller, 0x207, amount)` calldata — move tokens into escrow
       - **If `dominance == 1` (PROTOCOL)**: Build `bridgeBurn(caller, amount)` calldata — destroy caller's tokens
       - `execute_evm_call()` — run nested EVM call
       - Verify result is `Success`
       - For escrow (`dominance == 0`): check `decode_abi_bool` on return data (ERC-20 `transferFrom` returns `bool`)
       - For burn (`dominance == 1`): skip bool check (`bridgeBurn` is void, no return value)
       - `apply_state_changes()` — write state diffs back
  5. **`add_protocol_bal(assetId, to, amount)`** — credit recipient's protocol balance
  6. Atomic commit or rollback
- **Gas**: 20000 + nested EVM call gas
- **Approve requirement (EVM-dominant)**: User must first call `approve(0x207, amount)` on the ERC-20 contract. Without approval, `transferFrom` reverts and the entire operation rolls back.
- **Balance requirement (PROTOCOL-dominant)**: Caller must hold enough ERC-20 tokens. `bridgeBurn` checks `balanceOf[from] >= amount` in the contract (where `from` is the caller).

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

The bridge supports two models depending on `dominance`:

### Escrow Model (`dominance = 0`, EVM-dominant)

The Switch precompile address (`0x207`) holds ERC-20 tokens in its own balance. Protocol balance and EVM balance are kept synchronized by moving tokens into and out of this escrow.

```
┌─────────────────────────────────────────────────────────────┐
│  Internal Bridge (Protocol ↔ EVM) — Escrow Model           │
│                                                             │
│  ┌──────────────┐        ┌──────────────────────────────┐  │
│  │ AccountState │        │ EvmState                     │  │
│  │ (protocol)   │◄──────►│ (EVM layer)                  │  │
│  └──────────────┘        └──────────────────────────────┘  │
│         ▲                           ▲                       │
│         │    switchToEvm            │   switchToProtocol     │
│         │    (deduct protocol       │   (transferFrom to     │
│         │     → transfer ERC-20     │    0x207 escrow        │
│         │      from 0x207 escrow)   │    → credit protocol)  │
│         │                           │                       │
│  └──────┴───────────────────────────┴──────┐               │
│  │ Switch precompile (0x207)                │               │
│  │  - checkpoint / rollback atomicity       │               │
│  └──────────────────────────────────────────┘               │
│                                                             │
│  Entry point:                                               │
│  EVM transaction calling Switch precompile (user-initiated) │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### Mint/Burn Model (`dominance = 1`, PROTOCOL-dominant)

No escrow is needed. The Switch precompile creates or destroys ERC-20 tokens in real time via `bridgeMint`/`bridgeBurn`. Supply is synchronized per-switch.

```
┌─────────────────────────────────────────────────────────────┐
│  Internal Bridge (Protocol ↔ EVM) — Mint/Burn Model        │
│                                                             │
│  ┌──────────────┐        ┌──────────────────────────────┐  │
│  │ AccountState │        │ EvmState                     │  │
│  │ (protocol)   │◄──────►│ (EVM layer)                  │  │
│  └──────────────┘        └──────────────────────────────┘  │
│         ▲                           ▲                       │
│         │    switchToEvm            │   switchToProtocol     │
│         │    (deduct protocol       │   (bridgeBurn          │
│         │     → bridgeMint)         │    → credit protocol)  │
│         │                           │                       │
│  └──────┴───────────────────────────┴──────┐               │
│  │ Switch precompile (0x207)                │               │
│  │  - checkpoint / rollback atomicity       │               │
│  └──────────────────────────────────────────┘               │
│                                                             │
│  No escrow balance required. WrappedToken.bridgeMint        │
│  and bridgeBurn are called by 0x207 directly.               │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

---

## Asset Bridging Rules

The internal bridge treats assets differently based on `asset_id`, `has_erc20`, and `dominance`:

| Asset ID | Asset Type | `has_erc20` | `dominance` | Deposit Behavior (`switchToEvm`) | Withdraw Behavior (`switchToProtocol`) |
|----------|-----------|-------------|-------------|----------------------------------|----------------------------------------|
| `0` | Virtual USD | — | — | **Rejected** | **Rejected** |
| `1` | CALL (native) | — | — | Deduct protocol balance → **Add native EVM balance** (`evm_state.set_balance`) | Deduct native EVM balance → **Credit protocol balance** |
| `>=2` | Protocol-only | `0` | unset | **Rejected** — `AssetHasNoErc20Bridge` | **Rejected** — `AssetHasNoErc20Bridge` |
| `>=2` | ERC-20 backed | `1` | `0` (EVM) | Deduct protocol balance → **Transfer ERC-20 from `0x207` escrow to user** (`transfer`) | **Transfer ERC-20 from user to `0x207` escrow** (`transferFrom`) → Credit protocol balance |
| `>=2` | Protocol wrapper | `1` | `1` (PROTOCOL) | Deduct protocol balance → **Mint ERC-20 to user** (`bridgeMint`) | **Burn ERC-20 from user** (`bridgeBurn`) → Credit protocol balance |

**Key design decision:** CALL (asset_id=1) bridges as **native EVM gas balance**, not as a wrapped ERC-20. This allows bridged CALL to be used directly for EVM transaction gas and native transfers. User-defined assets must have `has_erc20 = 1` to enable switching. This can be achieved via:
- `registerErc20(address)` — binds an external ERC-20 contract (`dominance = 0`, escrow model)
- `createWrapper(assetId)` — deploys a system `WrappedToken` (`dominance = 1`, mint/burn model)

Protocol-only assets registered via `register` (`has_erc20 = 0`) cannot leave the protocol layer until `createWrapper` is called.

### Escrow Model (`dominance = 0`, EVM-dominant)

The Switch precompile (`0x207`) acts as an escrow holder for ERC-20 backed assets:

- **Deposit (`switchToEvm`)**: The protocol balance is deducted from the sender, and the Switch precompile transfers ERC-20 tokens from its own escrow balance to the recipient. The escrow must hold sufficient tokens, or the transfer reverts.
- **Withdraw (`switchToProtocol`)**: The user's ERC-20 tokens are transferred into the `0x207` escrow via `transferFrom`. The user must first `approve(0x207, amount)` on the ERC-20 contract. After the tokens are received, the protocol balance is credited to the recipient.

### Mint/Burn Model (`dominance = 1`, PROTOCOL-dominant)

No escrow is needed. The Switch precompile creates or destroys ERC-20 tokens in real time:

- **Deposit (`switchToEvm`)**: The protocol balance is deducted from the sender, and the Switch precompile calls `bridgeMint(to, amount)` on the system `WrappedToken` to create new ERC-20 tokens for the recipient.
- **Withdraw (`switchToProtocol`)**: The Switch precompile calls `bridgeBurn(caller, amount)` on the system `WrappedToken` to destroy the caller's ERC-20 tokens, then credits the corresponding protocol balance to the recipient. The caller must hold enough ERC-20 tokens.

The `WrappedToken` contract enforces `msg.sender == bridge` (i.e., `0x207`) for `bridgeMint`/`bridgeBurn`, ensuring only the Switch precompile can create or destroy tokens.

### Liquidity Requirement

**Escrow model (`dominance = 0`)**: The Switch precompile (`0x207`) must hold a token balance before any `switchToEvm` deposit can succeed. Liquidity is injected into escrow through:

1. **User-initiated withdrawals**: `switchToProtocol` transfers tokens from the user into `0x207`.
2. **Direct transfer**: Anyone can `transfer(0x207, amount)` on the ERC-20 contract to seed the pool.

If escrow balance is insufficient, `switchToEvm` reverts atomically and no state changes are committed.

**Mint/burn model (`dominance = 1`)**: No escrow balance is required. Tokens are created on demand during `switchToEvm` and destroyed during `switchToProtocol`. No liquidity risk exists.

---

## Precompile Alternative

The **Switch precompile at `0x207`** provides the same bridging functionality via standard EVM transactions, with an important restriction: **only ERC-20 backed assets (`has_erc20 == 1`) and CALL (`asset_id == 1`) are accepted**. Protocol-only assets (`has_erc20 == 0`) are rejected with `AssetHasNoErc20Bridge`.

| Operation | Precompile Function | Base Gas | Restrictions |
|---|---|---|---|
| `SwitchToEvm` | `switchToEvm(uint64,address,uint128)` | 20,000 (+ nested EVM call gas) | `asset_id == 1` or `has_erc20 == 1` |
| `SwitchToProtocol` | `switchToProtocol(uint64,address,uint128)` | 20,000 (+ nested EVM call gas) | `asset_id == 1` or `has_erc20 == 1` |

Solidity contracts and MetaMask can call these functions directly by sending an EVM transaction to `0x207`. See [precompile.md](precompile.md) for the full ABI.

### Atomicity

Both methods execute inside `dispatch::mutate_void`, which creates a checkpoint before the handler and either commits or reverts on completion. This means if any step fails (e.g., protocol balance deduction succeeds but the nested EVM call reverts), **all state changes are rolled back automatically**. No manual snapshot/rollback is required in `Block::execute`.

---

## File Map

| File | Role |
|------|------|
| `crates/switch/src/precompile.rs` | Switch precompile (`switchToEvm`, `switchToProtocol`) |
| `crates/switch/src/precompile.rs` | `SwitchStorage` — protocol balance add/sub, asset metadata reads |
| `crates/asset/src/precompile.rs` | Asset precompile (`register`, `registerErc20`, `createWrapper`) |
| `crates/asset/src/lib.rs` | `AssetStorage` — dominance, supply, balance, metadata management |
| `crates/precompile/src/evm_caller.rs` | `execute_evm_call`, `apply_state_changes`, `StorageProviderDb` |
| `crates/evm/contracts/WrappedToken.sol` | Reference ERC-20 with `bridgeMint` / `bridgeBurn` |
| `crates/consensus/src/block.rs` | Block execution pipeline — EVM tx execution, system settlement, state root computation |
| `crates/chainspec/src/genesis.rs` | Genesis asset registration and balance seeding |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| CALL native bridging | Ready | asset_id==1 uses `evm_state.set_balance` directly; no ERC-20 contract needed |
| User asset ERC-20 bridging (escrow) | Ready | Requires `registerErc20`; nested EVM `transfer`/`transferFrom` via `0x207` escrow |
| User asset ERC-20 bridging (mint/burn) | Ready | Requires `createWrapper`; nested EVM `bridgeMint`/`bridgeBurn` |
| Virtual USD rejection | Ready | asset_id==0 explicitly rejected in both deposit and withdraw paths |
| Atomic rollback | Ready | `dispatch::mutate_void` checkpoint/rollback inside precompile; no manual snapshot needed |
| Rate limiting | Not implemented | Per-tx and daily limits are not enforced in the Switch precompile |
| Bridge pause | Not implemented | Per-asset pause is not enforced in the Switch precompile |
| User-facing RPC | Not implemented | No dedicated bridge RPC endpoints; users send `eth_sendRawTransaction` to `0x207` directly |
| E2E test helpers | Not implemented | Python signing and RPC wrappers not yet available |
