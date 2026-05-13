# Callchain RPC Layer

## Overview

The RPC Layer (`crates/rpc`) provides external access to Callchain via JSON-RPC (HTTP) and WebSocket subscriptions. It exposes two endpoint families:

- **Standard Ethereum RPC** (`eth_*`) — compatibility with Ethereum tooling (wallets, explorers, indexers)
- **Callchain Extension RPC** (`call_*`) — native protocol operations (payments, governance, oracle, bridge, agent, shielded)

The RPC layer is the primary interface for users, dApps, validators, and operators.

---

## Architectural Rule

**All state-mutating RPCs construct standard EVM transactions targeting precompile addresses and submit via `eth_sendRawTransaction` → mempool → consensus → `Block::execute`.**

Direct state mutations from RPC handlers are prohibited. State-changing operations are invoked by calling EVM precompiles (`0x101`–`0x209`) during block execution. This ensures deterministic state transitions, replay protection through standard EVM nonces, and uniform gas accounting.

All state-mutating operations are submitted via standard `eth_sendRawTransaction` with an RLP-encoded EVM transaction targeting the appropriate precompile address. The former `call_submit` convenience endpoint and individual write endpoints (`call_sendPayment`, `call_register`, `call_governanceVote`, etc.) have been removed.

**EVM Precompile Path**: All protocol features are also accessible via standard EVM transactions sent to precompile addresses (`0x101`–`0x209`). Users can call `eth_sendRawTransaction` with an RLP-encoded EVM transaction targeting any precompile. This is the recommended path for MetaMask, Solidity contracts, and dApp integrations. See [precompile.md](precompile.md) for the ABI reference.

---

## Endpoint Inventory

### Ethereum-Compatible RPC (`eth_*`)

| Endpoint | Type | Description |
|----------|------|-------------|
| `eth_getBalance` | Read-only | Reads EVM balance from EVM storage (`CallEvmAccounts`) |
| `eth_call` | Read-only | Executes read-only EVM call, returns output |
| `eth_sendRawTransaction` | Transaction | Decodes RLP tx, validates nonce/balance, submits to mempool |
| `eth_getTransactionReceipt` | Read-only | Returns protocol receipt by tx hash |
| `eth_blockNumber` | Read-only | Returns current block height |
| `eth_getLogs` | Read-only | Address-indexed log lookup, unfiltered fallback capped at 10k receipts |
| `eth_getProof` | Read-only | Returns account state (balance, nonce, codeHash, storageRoot) with state root proof |
| `eth_chainId` | Read-only | Returns the chain ID |
| `eth_gasPrice` | Read-only | Returns the current base fee |
| `eth_syncing` | Read-only | Returns `false` (fully synced) or sync progress object |
| `eth_getTransactionCount` | Read-only | Returns EVM nonce for an address |
| `eth_getCode` | Read-only | Returns contract bytecode for an address |
| `eth_getStorageAt` | Read-only | Returns storage slot value for an address |
| `eth_estimateGas` | Read-only | Executes EVM call and returns gas used |
| `eth_getBlockByNumber` | Read-only | Returns block data by number tag (`latest`, `pending`, hex) |
| `eth_getBlockByHash` | Read-only | Returns block data by hash (not stored in RPC state — returns null) |
| `eth_getTransactionByHash` | Read-only | Returns transaction data by hash (looks up in receipts) |

### Read-Only Callchain RPC (`call_*`)

| Endpoint | Description |
|----------|-------------|
| `call_assetInfo` | Returns asset metadata by `asset_id` |
| `call_protocolBalance` | Returns protocol-layer balance for an address and asset |
| `call_getNonce` | Returns the next nonce for an address |
| `call_compliancePolicy` | Returns compliance policy for an asset |
| `call_totalBalance` | **Legacy** — returns outdated `Asset.total_supply` field; use `call_assetInfo` instead |
| `call_agentInfo` | Returns agent metadata by `agent_id` |
| `call_agentBalance` | Returns total balance held by an agent |
| `call_agentHistory` | Returns receipt history filtered by agent owner |
| `call_shieldedDepositProve` | Returns error — proving requires local `call-cli` or dedicated prover service |
| `call_shieldedTransferProve` | Returns error — proving requires local `call-cli` or dedicated prover service |
| `call_shieldedBalance` | Queries shielded balance for a viewing key |
| `call_shieldedTreeState` | Returns Merkle tree root, leaf count, nullifier count |
| `call_getTransactionReceipt` | Returns protocol receipt by tx hash (Callchain-native) |
| `call_getBlockReceipts` | Returns all receipts for a given block height |
| `call_getLogs` | Returns logs filtered by address (Callchain-native) |
| `call_getTxByReference` | Looks up a receipt by external reference / tx hash |
| `call_getRollbackHistory` | Returns rollback signature history from `ForkManager` |
| `call_getScheduledUpgrades` | Returns scheduled fork upgrades and next activation |
| `call_lightVerifyBlockHeader` | Verifies block header signatures against validator quorum |
| `call_lightGetBalanceProof` | Generates Merkle proofs for shielded note commitments |
| `call_lightGetTransactionProof` | Returns receipt inclusion proof for a tx hash |
| `call_lightVerifyShieldedTx` | Validates nullifier sets against spent state |
| `call_lightGetShieldedBalance` | Returns shielded note/nullifier counts for a viewing key |
| `call_governanceGetProposal` | Returns proposal details by `proposal_id` |
| `call_governanceGetAllProposals` | Returns all proposals summary |
| `call_governanceIsPaused` | Returns whether the chain is under emergency pause |
| `call_oracleGetPrice` | Returns median price, submission count, staleness for an asset |
| `call_oracleGetTwap` | Returns 24h TWAP for an asset |
| `call_oracleGetValidatorInfo` | Returns oracle participation stats for a validator |
| `call_bridgeGetDepositStatus` | Returns per-deposit status by `sourceTxHash` |
| `call_validatorList` | Returns all validators with stake and bonding info |

### EVM Precompile Write Operations

All state-mutating operations are submitted via `eth_sendRawTransaction` with a standard RLP-encoded EVM transaction targeting the appropriate precompile address. The `call_submit` convenience endpoint has been removed.

**Example — submitting a transfer via `eth_sendRawTransaction`:**

```json
POST / HTTP/1.1
{
  "jsonrpc": "2.0",
  "method": "eth_sendRawTransaction",
  "params": [
    "0xf8a201..."
  ],
  "id": 1
}
```

The raw transaction is a standard EVM transaction with:
- `to`: precompile address (e.g., `0x201` for asset operations)
- `data`: ABI-encoded function selector + arguments (e.g., `transfer(uint64,address,uint128)`)
- `gas_limit`: sufficient for the precompile's fixed gas cost
- `max_fee_per_gas`: current network base fee

Response:
```json
{
  "jsonrpc": "2.0",
  "result": "0x...",
  "id": 1
}
```

#### Supported Precompile Operations

All precompiles use **dynamic gas metering**: `gas_used = base_gas + sloads * 50 + sstores * 500`.

| Operation | Precompile | Function Selector | Base Gas |
|-----------|------------|-------------------|----------|
| `transfer` | `0x201` | `transfer(uint64,address,uint128)` | 5,000 |
| `batchTransfer` | `0x201` | `batchTransfer(uint64,address[],uint128[])` | 5,000 |
| `register` | `0x201` | `register(string,string,uint8,uint128)` | 50,000 |
| `mint` | `0x201` | `mint(uint64,address,uint128)` | 6,000 |
| `burn` | `0x201` | `burn(uint64,address,uint128)` | 5,000 |
| `registerAgent` | `0x209` | `registerAgent(bytes32,string,string)` | 6,000 |
| `grant` | `0x209` | `grant(uint64,uint64,uint128)` | 6,000 |
| `revoke` | `0x209` | `revoke(uint64,uint64)` | 6,000 |
| `submitProposal` | `0x203` | `submitProposal(uint8,string,string,bytes)` | 10,000 |
| `vote` | `0x203` | `vote(uint64,uint8)` | 10,000 |
| `queue` | `0x203` | `queue(uint64)` | 15,000 |
| `execute` | `0x203` | `execute(uint64)` | 30,000 |
| `emergencyPause` | `0x203` | `emergencyPause(string)` | 20,000 |
| `emergencyResume` | `0x203` | `emergencyResume()` | 20,000 |
| `externalBridgeDeposit` | `0x103` | `externalBridgeDeposit(bytes32,uint64,...)` | 10,000 |
| `externalBridgeWithdraw` | `0x103` | `externalBridgeWithdraw(uint64,address,...)` | 8,000 |
| `switchToEvm` | `0x207` | `switchToEvm(uint64,address,uint128)` | 8,000 |
| `switchToProtocol` | `0x207` | `switchToProtocol(uint64,address,uint128)` | 8,000 |
| `stake` | `0x204` | `stake(bytes,uint128)` | 20,000 |
| `unstake` | `0x204` | `unstake(uint64)` | 20,000 |
| `claimUnbonded` | `0x204` | `claimUnbonded(uint64)` | 15,000 |
| `submitRollbackSignature` | `0x203` | `submitRollbackSignature(uint64,uint64,...)` | 10,000 |

> **Note**: Dynamic storage gas is added automatically by `EvmStorageProvider` per Cancun rules (warm/cold sload, sstore refunds). The base gas covers decoding and business logic overhead.

See [precompile.md](precompile.md) for the full ABI reference including exact selector bytes and argument encoding.

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
│  │ eth_getBalance      │  │ call_assetInfo                │  │
│  │ eth_call            │  │ call_protocolBalance          │  │
│  │ eth_sendRawTx       │  │ call_getNonce                 │  │
│  │ eth_getReceipt      │  │ call_getTransactionReceipt    │  │
│  │ eth_blockNumber     │  │ call_validatorList            │  │
│  │ eth_getLogs         │  │ call_governanceGetProposal    │  │
│  │ eth_getProof        │  │ call_oracleGetPrice           │  │
│  │ eth_chainId         │  │                               │  │
│  │ eth_gasPrice        │  │                               │  │
│  │ eth_estimateGas     │  │                               │  │
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

## Execution Flow

### EVM Precompile Write Flow (via `eth_sendRawTransaction`)

```
Client
    ↓
Build EVM transaction { to: precompile_addr, data: selector + args, ... }
    ↓
Sign EVM tx → eth_sendRawTransaction { raw_rlp_tx }
    ↓
RPC Handler decodes RLP → recovers signer → validates nonce + balance
    ↓
Mempool defense (rate limit, dedup) → inserts into EVM mempool
    ↓
Return pending tx hash
    ↓
Block production (consensus) selects tx from mempool by gas price
    ↓
Block::execute → revm executes tx → precompile_fn() called
    ↓
State transition committed atomically → receipt stored
```

### EVM Transaction Flow (via `eth_sendRawTransaction`)

```
Client
    ↓
POST eth_sendRawTransaction { raw_rlp_tx }
    ↓
RPC Handler decodes RLP → recovers signer
    ↓
Validates nonce + balance (read-only checks)
    ↓
Mempool defense (rate limit, dedup)
    ↓
Inserts into EVM mempool → returns tx hash
    ↓
Block production includes tx → EVM execution in Block::execute
    ↓
Receipt stored after block finalization
```

**Important:** `eth_sendRawTransaction` does NOT execute the transaction immediately. The previous double-execution bug (executing in RPC handler AND in block production) has been fixed.

---

## WebSocket Subscriptions

9 subscription channels with broadcast capacity:

| Subscribe | Unsubscribe | Capacity | Event Payload |
|-----------|-------------|----------|---------------|
| `call_subscribeNewBlocks` | `call_unsubscribeNewBlocks` | 1024 | `{ height, hash, proposer, tx_count }` |
| `call_subscribeNewPayments` | `call_unsubscribeNewPayments` | 1024 | `{ tx_hash, from, to, asset_id, amount }` |
| `call_subscribeBridgeCompleted` | `call_unsubscribeBridgeCompleted` | 256 | `{ op_id, status }` |
| `call_subscribeAssetRegistered` | `call_unsubscribeAssetRegistered` | 256 | `{ asset_id, symbol, issuer }` |
| `call_subscribeAgentExecuted` | `call_unsubscribeAgentExecuted` | 256 | `{ agent_id, action }` |
| `call_subscribeAgentRevoked` | `call_unsubscribeAgentRevoked` | 256 | `{ agent_id }` |
| `call_subscribeShieldedDeposit` | `call_unsubscribeShieldedDeposit` | 512 | `{ commitment }` |
| `call_subscribeShieldedWithdrawal` | `call_unsubscribeShieldedWithdrawal` | 512 | `{ nullifier }` |
| `call_subscribeGovernance` | `call_unsubscribeGovernance` | 256 | `{ event, proposal_id, details }` |

Subscribers that fall behind receive a `Lagged { dropped: N }` notification.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `RpcConfig`, `start_http_server()`, `build_rpc_module()` |
| `handlers.rs` | `RpcState`, `NodeProposalExecutor`, `submit_payment()`, `submit_evm_tx()` |
| `standard.rs` | Ethereum-compatible RPC endpoints (17 methods) |
| `callchain.rs` | Callchain-native RPC endpoints: read-only queries |
| `ws.rs` | WebSocket subscription manager and registration |

### Prover Service (`crates/prover/`)

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
| HTTP JSON-RPC server | Ready | jsonrpsee is production-grade, TLS + rate limiting configured |
| Standard Ethereum RPC | Ready | 17 endpoints including gas estimation, block queries, code/storage |
| EVM precompile write flow | Ready | All state mutations go through `eth_sendRawTransaction` targeting precompile addresses |
| Precompile-based flow | Ready | All state mutations go through EVM tx → precompile → mempool → consensus |
| Agent RPC | Ready | `RegisterAgent` / `GrantAgentBalance` / `RevokeAgentBalance` via `eth_sendRawTransaction` |
| Rollback RPC | Ready | `SubmitRollbackSignature` via `eth_sendRawTransaction` |
| Governance RPC | Ready | Read-only queries + `eth_sendRawTransaction` for state changes |
| Oracle RPC | Ready | Read-only price and TWAP queries; submission is via consensus |
| Bridge RPC | Ready | Deposit + withdraw via `eth_sendRawTransaction`; per-deposit status lookup |
| Shielded RPC | Ready | Proving via dedicated service (`call-prover`), balance query wired |
| Light client RPC | Ready | Real Merkle proofs, shielded validation correct |
| WebSocket subscriptions | Ready | Lag notifications sent to subscribers |
| CORS configuration | Ready | Configurable via `RpcConfig.cors_allowed_origins` |
