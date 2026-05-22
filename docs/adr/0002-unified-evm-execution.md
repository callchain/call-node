# ADR-0002: Unified EVM Execution via Precompiled Contracts

- Status: Accepted
- Date: 2026-04-13
- Author(s): Callchain Core Team

## Context

Callchain must support both:

1. **Protocol-native operations** — asset transfers, staking, governance voting, oracle price submission, compliance checks, shielded pool deposits/withdrawals, and cross-chain bridging.
2. **Standard EVM smart contracts** — full Ethereum compatibility so existing DeFi ecosystems can migrate seamlessly.

A conventional design would maintain two separate ledgers: a "protocol ledger" for native operations and an "EVM ledger" for contract execution, bridged via a translation layer. This approach is used by several L1s but introduces complexity: dual state roots, dual execution engines, dual gas models, and fragile synchronization logic.

The team needed a design that gives smart contracts first-class access to protocol state while preserving a single state root, a single execution engine, and Ethereum-compatible tooling.

## Decision

Run **all protocol operations inside the EVM** via a fixed set of **native precompiled contracts** at addresses `0x101`–`0x209`. There is no separate protocol ledger. Protocol state (balances, validator stakes, asset registries, compliance lists, governance proposals) is stored in EVM storage slots under the precompile addresses. Standard EVM transactions call these precompiles with ABI-encoded input, and revm routes the calls to native Rust handlers.

### Precompile Address Assignment

| Address | Precompile | Operations |
|---------|------------|------------|
| `0x101` | Oracle | `getPrice`, `getTWAP`, `isStale`, `submitPrice` |
| `0x103` | Bridge | `externalBridgeDeposit`, `externalBridgeWithdraw`, `challengeBridgeDeposit` |
| `0x201` | Asset | `transfer`, `batchTransfer`, `approve`, `transferFrom`, `mint`, `burn`, `register`, `registerErc20`, `createWrapper` |
| `0x202` | Shielded | `deposit`, `withdraw`, `transfer` |
| `0x203` | Governance | `submitProposal`, `vote`, `queue`, `execute`, `emergencyPause`, `emergencyResume` |
| `0x204` | Validator | `stake`, `unstake`, `claimUnbonded` |
| `0x205` | Compliance | `updateCompliance`, `checkCompliance` |
| `0x207` | Switch | `switchToEvm`, `switchToProtocol` (internal bridge) |
| `0x209` | Agent | `register`, `grant`, `revoke` |

### Execution Architecture

- **Single executor**: `EvmExecutor` in `crates/evm/src/executor.rs` wraps revm with `SpecId::CANCUN`.
- **Custom precompile provider**: `CallPrecompiles` in `crates/precompile/src/lib.rs` implements revm's `PrecompileProvider`. It tries custom precompiles first, then falls back to standard Ethereum precompiles.
- **State access**: Precompiles receive an `EvmStorageProvider` that wraps revm's live journal, giving them `sload`/`sstore` access with automatic gas tracking and call-depth isolation.
- **Atomicity**: Because precompiles write to EVM storage through the journal, revm's existing transaction-level rollback mechanism handles failure automatically. No separate protocol-state snapshot is needed.
- **Composability**: Smart contracts can call multiple precompiles in a single transaction. A payroll contract can loop over `0x201` transfers, and if any one fails, the entire invocation reverts.

### Internal Bridge (Switch Precompile `0x207`)

Assets can be bound to ERC-20 contracts in two modes:

- **EVM-dominant** (`dominance = 0`): The Switch precompile holds tokens in escrow. Deposits move tokens from the user to the precompile; withdrawals release escrowed tokens.
- **Protocol-dominant** (`dominance = 1`): The Switch precompile mints/burns a system `WrappedToken` contract on demand.

This unifies protocol balances and ERC-20 balances under the same EVM state tree.

## Consequences

### Positive

- **One state root**: The block header's `state_root` is exactly the EVM state root. There is no separate protocol state root to reconcile, which simplifies light clients, bridges, and fraud proofs.
- **One execution engine**: Only revm runs transactions. There is no dual VM, no cross-VM call overhead, and no special "system transaction" path (except for block-reward settlement, which is planned to become an EVM transaction as well).
- **Full composability**: Solidity contracts can import protocol functionality as ordinary external calls. Existing wallets, explorers, and indexers work without modification.
- **Automatic atomicity**: Revm's journal provides per-call and per-transaction rollback. A failed governance vote or invalid transfer reverts exactly like a failed ERC-20 transfer.
- **Gas uniformity**: All operations consume EVM gas. There is no separate "protocol gas" or fee token.
- **Simplified auditing**: The entire state transition function lives inside revm plus a set of pure Rust precompile handlers. Auditors do not need to reason about two state machines.

### Negative / Trade-offs

- **Precompile address exhaustion**: The range `0x101`–`0x209` provides limited slots. New protocol features may eventually require an extension scheme (e.g., a dispatcher precompile with function-level routing).
- **Storage layout discipline**: All protocol data is packed into EVM storage slots under precompile addresses. The `storage_slot` helper in `crates/precompile/src/storage.rs` derives deterministic slots from key fragments, but this is an ad-hoc schema. There is no automatic schema migration for precompile storage.
- **Compliance check overhead**: Every standard value transfer (not just precompile calls) triggers a compliance read from `COMPLIANCE_ADDRESS` storage in `execute_tx_db`. This adds an extra `sload` to every transaction, even when the compliance policy is `None`.
- **Debugging complexity**: Protocol state is no longer visible in a separate "protocol state" RPC namespace. Developers must query `eth_getStorageAt` against precompile addresses to inspect raw protocol state, which is opaque without knowledge of the slot-layout scheme.
- **Precompile gas model**: Precompiles use dynamic metering (`base + sloads*50 + sstores*500`). This is simpler than Ethereum's `SSTORE` refund rules but may diverge from user expectations for "warm" vs "cold" storage access.

## Alternatives Considered

- **Separate protocol ledger + EVM bridge**: Rejected because it requires dual state roots, dual execution engines, and complex atomicity guarantees between the two ledgers. Most existing L1s that took this path have struggled with state-root synchronization and cross-ledger replay bugs.
- **Cosmos SDK / appchain module model**: Rejected because it sacrifices EVM compatibility. DeFi protocols would need to be rewritten in a different execution model.
- **Substrate pallet + Frontier EVM**: Rejected because Substrate's FRAME model still separates pallet state from EVM state, reintroducing the dual-ledger problem.

## References

- `crates/evm/src/executor.rs` — `EvmExecutor`, transaction execution with custom precompiles
- `crates/precompile/src/lib.rs` — `CallPrecompiles`, `StatefulPrecompile` trait, address constants
- `crates/precompile/src/storage.rs` — `EvmStorageProvider`, `storage_slot` helper
- `docs/spec.md` §3.5, §3.6, §3.7 — precompile invocation model, execution semantics, gas accounting
