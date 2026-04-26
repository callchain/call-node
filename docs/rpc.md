# Callchain RPC Layer

## Overview

The RPC Layer (`crates/rpc`) provides external access to Callchain via JSON-RPC (HTTP) and WebSocket subscriptions. It exposes two endpoint families:

- **Standard Ethereum RPC** (`eth_*`) — compatibility with Ethereum tooling (wallets, explorers, indexers)
- **Callchain Extension RPC** (`call_*`) — native protocol operations (payments, governance, oracle, bridge, agent, shielded)

The RPC layer is the primary interface for users, dApps, validators, and operators.

---

## Architectural Rule

**All state-mutating RPCs must construct a `ProtocolTransaction` and submit via `insert_protocol_tx` → mempool → consensus → `Block::execute`.**

Direct state mutations from RPC handlers are prohibited. State-changing operations are implemented as `Instruction` variants executed inline during block production. This ensures deterministic state transitions, replay protection through consensus ordering, and uniform gas accounting.

The only write endpoint is `call_submit`, which accepts a `ProtocolTransaction` containing one or more `Instruction`s. All former individual write endpoints (`call_sendPayment`, `call_registerAsset`, `call_governanceVote`, etc.) have been removed and consolidated into this unified interface.

---

## Endpoint Inventory

### Ethereum-Compatible RPC (`eth_*`)

| Endpoint | Type | Description |
|----------|------|-------------|
| `eth_getBalance` | Read-only | Reads EVM balance from `EvmState` |
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
| `call_totalBalance` | Returns total protocol-layer balance sum for an asset (equivalent to `protocol_supply`) |
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

### Unified Write Endpoint (`call_submit`)

All state-mutating operations are submitted through a single endpoint:

```json
POST / HTTP/1.1
{
  "jsonrpc": "2.0",
  "method": "call_submit",
  "params": {
    "sender": "0x...",
    "nonce": 1,
    "instructions": [
      {
        "type": "Transfer",
        "asset_id": 1,
        "to": "0x...",
        "amount": "5000",
        "memo": { "message": "hello" }
      }
    ],
    "signature": "0x..."
  },
  "id": 1
}
```

Response:
```json
{
  "jsonrpc": "2.0",
  "result": {
    "txHash": "0x...",
    "status": "pending"
  },
  "id": 1
}
```

#### Supported Instruction Types

Each instruction in the `instructions` array must have a `"type"` field. Supported types:

| Type | Fields | Gas |
|------|--------|-----|
| `Transfer` | `asset_id`, `to`, `amount`, `memo?` | 100,000 |
| `RegisterAsset` | `symbol`, `name`, `decimals`, `max_supply` | 200,000 |
| `RegisterAgent` | `pubkey`, `name`, `url` | 50,000 |
| `GrantAgentBalance` | `agent_id`, `asset_id`, `amount` | 10,000 |
| `RevokeAgentBalance` | `agent_id`, `asset_id` | 10,000 |
| `GovernanceSubmitProposal` | `proposal_type`, `title`, `description`, `execution_data` | 200,000 |
| `GovernanceVote` | `proposal_id`, `vote` | 50,000 |
| `GovernanceQueue` | `proposal_id` | 50,000 |
| `GovernanceExecute` | `proposal_id` | 100,000 |
| `GovernanceEmergencyPause` | `reason` | 200,000 |
| `GovernanceEmergencyResume` | — | 200,000 |
| `ExternalBridgeDeposit` | `source_tx_hash`, `source_chain`, ... | 200,000 |
| `ExternalBridgeWithdraw` | `target_chain`, `target_address`, ... | 200,000 |
| `BridgeToEvm` | `asset_id`, `to`, `amount` | 25,000 |
| `BridgeToProtocol` | `asset_id`, `to`, `amount` | 25,000 |
| `EvmIssuerMint` | `asset_id`, `to`, `amount` | 50,000 |
| `ValidatorStake` | `ed25519_pubkey`, `self_stake` | 200,000 |
| `ValidatorUnstake` | `validator_id` | 100,000 |
| `ValidatorClaimUnbonded` | `validator_id` | 100,000 |
| `SubmitRollbackSignature` | `validator_id`, `target_height`, `target_version_major`, `target_version_minor`, `target_version_patch`, `nonce`, `signature` | 50,000 |

All `call_submit` requests require:
- `sender`: the signer's address
- `nonce`: transaction nonce for replay protection
- `signature`: 65-byte secp256k1 signature (`r || s || v`) over `compute_tx_hash()`

Multiple instructions can be included in a single `ProtocolTransaction` for atomic batch execution.

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
│  │ eth_chainId         │  │ call_submit  ← unified write  │  │
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

### Unified Write Flow (via `call_submit`)

```
Client
    ↓
POST call_submit { sender, nonce, instructions, signature }
    ↓
RPC Handler parses JSON → converts "type" to externally-tagged Instruction
    ↓
Build ProtocolTransaction { sender, nonce, instructions, auth, ... }
    ↓
tx.verify_signature() → state.insert_protocol_tx(tx) → Mempool
    ↓
Return pending tx hash { txHash, status: "pending" }
    ↓
Block production (consensus) selects tx from mempool
    ↓
Block::execute → execute_agent_instruction / execute_governance_instruction / ...
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
| `callchain.rs` | Callchain-native RPC endpoints: read-only queries + unified `call_submit` |
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
| Unified write endpoint (`call_submit`) | Ready | Single entry point for all state mutations; multi-instruction batch support |
| Instruction-based flow | Ready | All state mutations go through `ProtocolTransaction` → mempool → consensus |
| Agent RPC | Ready | `RegisterAgent` / `GrantAgentBalance` / `RevokeAgentBalance` via `call_submit` |
| Rollback RPC | Ready | `SubmitRollbackSignature` via `call_submit` |
| Governance RPC | Ready | Read-only queries + `call_submit` for state changes |
| Oracle RPC | Ready | Read-only price and TWAP queries; submission is via consensus |
| Bridge RPC | Ready | Deposit + withdraw via `call_submit`; per-deposit status lookup |
| Shielded RPC | Ready | Proving via dedicated service (`call-prover`), balance query wired |
| Light client RPC | Ready | Real Merkle proofs, shielded validation correct |
| WebSocket subscriptions | Ready | Lag notifications sent to subscribers |
| CORS configuration | Ready | Configurable via `RpcConfig.cors_allowed_origins` |
