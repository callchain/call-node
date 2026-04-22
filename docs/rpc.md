# Callchain RPC Layer

## Overview

The RPC Layer (`crates/rpc`) provides external access to Callchain via JSON-RPC (HTTP) and WebSocket subscriptions. It exposes two endpoint families:

- **Standard Ethereum RPC** (`eth_*`) — compatibility with Ethereum tooling (wallets, explorers, indexers)
- **Callchain Extension RPC** (`call_*`) — native protocol operations (payments, governance, oracle, bridge, agent, shielded)

The RPC layer is the primary interface for users, dApps, validators, and operators. It must balance accessibility with security, as many critical operations (governance, oracle submissions, bridge deposits) are exposed.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  RPC Server (jsonrpsee)                                      │
│  ├── HTTP on :8545                                           │
│  ├── WebSocket on :8546                                      │
│  └── max_connections: 100 (default)                          │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ Standard RPC        │  │ Callchain Extension RPC       │  │
│  │ eth_getBalance      │  │ call_sendPayment              │  │
│  │ eth_call            │  │ call_governanceSubmitProposal │  │
│  │ eth_sendRawTx       │  │ call_oracleGetPrice           │  │
│  │ eth_getReceipt      │  │ call_bridgeSubmitDeposit      │  │
│  │ eth_blockNumber     │  │ call_agentRegister            │  │
│  │ eth_getLogs         │  │ call_shielded*                │  │
│  │ eth_getProof      │  │ call_light*                   │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ WebSocket Subscriptions                                  ││
│  │ call_subscribeNewBlocks / Payments / Bridge / ...       ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│  Prover Service (call-prover) — separate process             │
│  ├── HTTP on :8550 (default)                                 │
│  ├── POST /prove/deposit                                     │
│  ├── POST /prove/transfer                                    │
│  ├── POST /prove/withdraw                                    │
│  └── GET  /health                                            │
│                                                             │
│  Generates Groth16 proofs (BN254, ~128B) for shielded       │
│  transactions. Keeps private keys off the node and allows    │
│  independent CPU scaling for proof generation.               │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. RPC Server (`lib.rs`)

`RpcConfig` defaults:
- HTTP: `127.0.0.1:8545`
- WebSocket: `127.0.0.1:8546`
- max_connections: 100

`build_rpc_module()` combines standard + callchain + WebSocket subscriptions into a single `RpcModule`.

### 2. Standard Ethereum RPC (`standard.rs`)

| Endpoint | Status | Notes |
|----------|--------|-------|
| `eth_getBalance` | Ready | Reads EVM balance from `EvmState` |
| `eth_call` | Ready | Executes read-only EVM call, returns output |
| `eth_sendRawTransaction` | Ready | Decodes RLP tx, validates nonce/balance, executes immediately |
| `eth_getTransactionReceipt` | Ready | Returns protocol receipt by tx hash |
| `eth_blockNumber` | Ready | Returns current block height |
| `eth_getLogs` | Ready | Address-indexed log lookup, unfiltered fallback capped at 10k receipts |
| `eth_getProof` | Ready | Returns account state (balance, nonce, codeHash, storageRoot) with state root proof |

### 3. Callchain Extension RPC (`callchain.rs`)

#### Payment (`call_sendPayment`)

Validates EIP-191 personal_sign signatures (`\x19Ethereum Signed Message:\n32` prefix) by recovering the signer from the tx hash. Receipts from immediate execution are marked with `block_number: 0` and `"pending": true` in the JSON response.

#### Asset Registration (`call_registerAsset`)

Registers a new asset in the `AssetRegistry`. Requires an EIP-191 `personal_sign` signature over `keccak256("RegisterAsset:{symbol}:{name}:{decimals}:{issuer}")`; the recovered signer must match the issuer address. Deducts `governance.config.asset_registration_fee` from the issuer's CALL balance.

#### Agent (`call_agentRegister`, `call_agentGrant`, `call_agentRevoke`)

Agent registration deducts `fee_params.base_fee` from the owner's CALL balance.

#### Shielded (`call_shieldedDepositProve`, `call_shieldedTransferProve`, `call_shieldedBalance`)

Proving is handled by a dedicated **prover service** (`call-prover`), a separate process that runs alongside the node. The RPC endpoints return actionable error messages directing clients to the prover CLI (`call-cli shielded deposit-prove <args>`). The prover service exposes HTTP endpoints at `POST /prove/deposit`, `POST /prove/transfer`, and `POST /prove/withdraw`, generating Groth16 proofs over BN254 (~128 bytes). This architecture keeps private keys off the node and allows independent CPU scaling.

`call_shieldedBalance` accepts a viewing key and queries `ShieldedState.note_registry`, returning the actual balance and note count.

#### Light Client (`call_lightVerifyBlockHeader`, `call_lightGetBalanceProof`, `call_lightVerifyShieldedTx`)

`call_lightGetBalanceProof` generates real Merkle proofs from `IncrementalMerkleTree` for shielded note commitments. `call_lightVerifyShieldedTx` validates nullifier sets — a transaction is valid only when none of its nullifiers have been spent.

#### Oracle (`call_oracleGetPrice`, `call_oracleGetTwap`)

Read-only oracle endpoints. Price submission is handled by consensus (validators submit via block production, not RPC).

#### Governance (`call_governanceSubmitProposal`, `call_governanceVote`, `call_governanceExecute`)

Signature verification with replay protection (block-window nonces) is implemented for all three endpoints. `require_governance_auth` flag controls whether signatures are mandatory.

#### Bridge (`call_bridgeSubmitDeposit`, `call_bridgeSubmitWithdraw`, `call_bridgeGetDepositStatus`, `call_lightClientBridgeDeposit`)

`call_bridgeSubmitDeposit` submits external deposits with validator signatures for consensus processing. `call_bridgeSubmitWithdraw` initiates a withdrawal to an external chain (burns protocol balance, emits event for validator relay). `call_bridgeGetDepositStatus` returns per-deposit status (pending/challenge_period, finalized, or challenged) by looking up `sourceTxHash` in bridge events. `call_lightClientBridgeDeposit` verifies deposits via light client (header RLP + MPT proof + receipt proof) with challenge period.

### 4. WebSocket Subscriptions (`ws.rs`)

9 subscription channels with broadcast capacity:

| Channel | Capacity |
|---------|----------|
| NewBlocks | 1024 |
| NewPayments | 1024 |
| BridgeCompleted | 256 |
| AssetRegistered | 256 |
| AgentExecuted | 256 |
| AgentRevoked | 256 |
| ShieldedDeposit | 512 |
| ShieldedWithdrawal | 512 |
| Governance | 256 |

### 5. Shared RPC State (`handlers.rs`)

`RpcState` holds all subsystem states behind `RwLock`s:

```rust
pub struct RpcState {
    pub balance_state: RwLock<BalanceState>,
    pub asset_registry: RwLock<AssetRegistry>,
    pub compliance_engine: RwLock<ComplianceEngine>,
    pub evm_state: RwLock<EvmState>,
    pub bridge_state: RwLock<BridgeStateManager>,
    pub validator_state: RwLock<ValidatorStateManager>,
    pub log_index: RwLock<HashMap<Address, Vec<(u64, TxHash, usize)>>>,
    pub receipts: RwLock<HashMap<TxHash, ProtocolReceipt>>,
    // ... 20+ more fields
}
```

Receipts are stored in-memory with `block_number: 0` marking for pending transactions. Receipts are pruned via `prune_receipts(1000)` on block finalization.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `RpcConfig`, `start_http_server()`, `build_rpc_module()` |
| `handlers.rs` | `RpcState`, `NodeProposalExecutor`, `submit_payment()`, `submit_evm_tx()` |
| `standard.rs` | Ethereum-compatible RPC endpoints |
| `callchain.rs` | Callchain-native RPC endpoints (~1264 lines) |
| `ws.rs` | WebSocket subscription manager and registration |

### 6. Prover Service (`crates/prover/`)

A separate binary (`call-prover`) that accepts shielded proving requests over HTTP and returns ZK proofs.

| File | Role |
|------|------|
| `main.rs` | CLI args (`--listen-addr`), boot, server startup |
| `server.rs` | Axum routes: `/prove/deposit`, `/prove/transfer`, `/prove/withdraw`, `/health` |

Usage:
```
call-prover                          # default: 127.0.0.1:8550
call-prover --listen-addr 0.0.0.0:8550
```

Uses `RealProver::global()` from `call-shielded` which loads production ceremony keys if available, otherwise falls back to dev trusted setup.

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| HTTP JSON-RPC server | 🟢 Ready | jsonrpsee is production-grade, TLS + rate limiting configured |
| Standard Ethereum RPC | 🟢 Ready | Address-indexed logs, real account proofs, pending receipt marking |
| Callchain payment RPC | 🟢 Ready | EIP-191 signatures, pending receipt marking |
| Asset registration | 🟢 Ready | Registration fee enforced from governance config |
| Agent RPC | 🟢 Ready | Registration fee enforced |
| Governance RPC | 🟢 Ready | Signature + nonce-based replay protection on all three endpoints |
| Oracle RPC | 🟢 Ready | Read-only price and TWAP queries; submission is via consensus |
| Bridge RPC | 🟢 Ready | Deposit + withdraw endpoints, per-deposit status lookup, light client bridge |
| Shielded RPC | 🟢 Ready | Proving via dedicated service (`call-prover`), balance query wired, actionable error messages |
| Light client RPC | 🟢 Ready | Real Merkle proofs, shielded validation correct |
| WebSocket subscriptions | 🟢 Ready | Lag notifications sent to subscribers |
| CORS configuration | 🟢 Ready | Configurable via `RpcConfig.cors_allowed_origins` |
| Startup logging | 🟢 Ready | Structured `tracing::info!` on server start |

---

## Test Status

- `cargo test -p call-rpc` — unit tests cover RPC module building, subscription registration, handler state operations
- Missing: `eth_getLogs` performance tests, light client balance proof verification tests, WebSocket lag handling tests
