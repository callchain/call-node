# CALL Asset Dual-Balance Architecture

## Overview

CALL (asset_id = 1) is the native token of the Callchain protocol. Unlike other assets that live primarily in the protocol layer, CALL has **two independent balance stores**:

1. **EVM native balance** — standard Ethereum account balance, used for gas payment and all EVM-level transfers.
2. **Protocol balance** — stored in the Asset precompile (0x201) storage slots, used by protocol-layer operations (`transfer`, `batchTransfer`, `stake`, `agent grant`, etc.).

These two balances are **independent and never automatically synchronized**. A user's total CALL holdings is the **sum** of both balances, but each balance is spent from its own layer.

## Rationale

Previously, the system attempted to keep both balances in sync: genesis seeded both sides, asset precompile `transfer` mirrored changes to native balance, and block execution automatically "pre-bridged" protocol CALL to EVM for gas. This created several problems:

- **Divergence bug**: EVM transactions modify native balance but do not write back to protocol slots, causing permanent inconsistency.
- **Complexity**: Every protocol operation on CALL required conditional native-balance bookkeeping.
- **Confusion**: Users could not reason about which balance was authoritative.

The new model makes the separation explicit: both balances are real, both are spendable (in their own domain), and migration between them is a deliberate user action via the Switch precompile.

## Genesis Initialization

At genesis, CALL is distributed **only as native EVM balance** (`set_balance`). Protocol-layer slots for CALL are **not seeded** for regular accounts.

```rust
// chainspec/src/genesis.rs
if is_call {
    // CALL: native EVM balance only — no protocol slot
    evm_state.set_balance(addr, U256::from(*amount));
} else {
    // Other assets: seed both protocol and EVM balances
    state_accessors::seed_balance(evm_state, asset.asset_id, addr, *amount);
    evm_state.set_balance(addr, U256::from(*amount));
}
```

The genesis staking escrow (for validator self-stake) still receives protocol balance because staking is a protocol-layer operation. Validators who wish to stake must first `switchToProtocol` their CALL from EVM to the protocol layer.

## Balance Semantics

| Operation Layer | Balance Store | Used By |
|----------------|---------------|---------|
| EVM native | `account.info.balance` | `eth_sendRawTransaction`, contract calls, gas payment |
| Protocol slot | `ASSET_ADDRESS slot_balance(1, addr)` | Asset precompile `transfer`, `batchTransfer`, `stake`, Agent `grantBalance` |

### Querying Balances

- `eth_getBalance(addr)` → **native EVM balance only**
- Asset precompile `balanceOf(1, addr)` → **protocol balance only**

There is no single RPC that returns the sum. Wallets and explorers should display both or aggregate client-side.

### Total Supply

- `totalSupply` in AssetStorage tracks **protocol-layer supply only**.
- True circulating CALL = `protocol_supply + sum(native_balances)`.

## Migration Between Layers

Users move CALL between layers via the **Switch precompile (0x207)**:

### `switchToProtocol(uint64 assetId, address to, uint128 amount)`

Moves CALL from EVM native balance to protocol balance.

- Deducts `amount` from caller's **native EVM balance**.
- Adds `amount` to `to`'s **protocol balance** via `AssetStorage::add_balance`.

### `switchToEvm(uint64 assetId, address to, uint128 amount)`

Moves CALL from protocol balance to EVM native balance.

- Deducts `amount` from caller's **protocol balance** via `AssetStorage::deduct_balance`.
- Adds `amount` to `to`'s **native EVM balance**.

**CALL does not require an ERC-20 wrapper** — it switches directly between the two native balance stores.

## Removed Automatic Sync

The following automatic CALL balance sync behaviors have been removed:

1. **Asset precompile transfer/batchTransfer/transferFrom** no longer mirror CALL moves to native balance.
2. **Block pre-bridge** (`crates/consensus/src/block.rs`) no longer auto-moves protocol CALL to EVM for gas payment. EVM transactions must have sufficient native balance or they fail.
3. **Genesis no longer seeds protocol slots** for CALL distribution.

## Impact on Protocol Operations

### Gas Payment

EVM transactions require sufficient **native EVM balance** to pay for gas. If a user only holds CALL in the protocol layer, they must first `switchToEvm` before sending EVM transactions.

### Staking

Validator `stake` deducts from **protocol balance**. Genesis validators whose self-stake is held in the staking escrow can stake immediately. New validators must `switchToProtocol` before calling `stake`.

### Agent Operations

Agent `grantBalance`, `pay`, and `batchPay` operate on **protocol balances**. Users must `switchToProtocol` before granting CALL to an agent.

### Governance

Governance proposal deposits and treasury payouts currently operate on **protocol balances** (via `add_balance_evm` / `deduct_balance_evm` helpers). This behavior is unchanged.

## Implementation Details

### Key Files

| File | Responsibility |
|------|---------------|
| `crates/chainspec/src/genesis.rs` | Genesis: CALL distributed as native balance only |
| `crates/asset/src/precompile.rs` | Asset precompile: no CALL→native sync in transfer/transferFrom/batchTransfer |
| `crates/consensus/src/block.rs` | Block execution: no pre-bridge protocol→EVM for gas |
| `crates/switch/src/precompile.rs` | Switch precompile: `switchToEvm` / `switchToProtocol` for CALL migration |

### Constants

- `CALL_ASSET_ID = 1` — hardcoded native asset identifier.
- Switch precompile checks `assetId == CALL_ASSET_ID` to use native balance add/sub instead of ERC-20 escrow or bridgeMint.

## Future Considerations

- A unified `totalBalanceOf` view function could be added to the Asset precompile to return `protocol_balance + native_balance` for convenience.
- Wallets should be aware of the dual-balance model and guide users to `switchToEvm` when native balance is insufficient for gas.
