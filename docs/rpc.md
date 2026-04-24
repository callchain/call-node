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

Endpoints that still bypass this flow are documented below as **Direct Execution** and should be migrated to Instruction-based models.

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

### Read-Only Callchain RPC (`call_*`)

| Endpoint | Description |
|----------|-------------|
| `call_assetInfo` | Returns asset metadata by `asset_id` |
| `call_protocolBalance` | Returns protocol-layer balance for an address and asset |
| `call_getNonce` | Returns the next nonce for an address |
| `call_compliancePolicy` | Returns compliance policy for an asset |
| `call_totalBalance` | Returns total minted supply for an asset |
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

### Instruction-Based RPC (`call_*` via `insert_protocol_tx`)

These endpoints construct a `ProtocolTransaction` wrapping one or more `Instruction`s, submit it to the mempool via `insert_protocol_tx`, and return a pending tx hash. Execution happens during block production in `Block::execute`.

| Endpoint | Instruction | Gas |
|----------|-------------|-----|
| `call_registerAsset` | `RegisterAsset { symbol, name, decimals }` | 200,000 |
| `call_sendPayment` | `Transfer { asset_id, to, amount, memo }` | 100,000 |
| `call_agentRegister` | `RegisterAgent { pubkey, name, url }` | 50,000 |
| `call_agentGrant` | `GrantAgentBalance { agent_id, asset_id, amount }` | 10,000 |
| `call_agentRevoke` | `RevokeAgentBalance { agent_id, asset_id }` | 10,000 |
| `call_submitRollbackSignature` | `SubmitRollbackSignature { validator_id, target_height, target_version_*, nonce, signature }` | 50,000 |
| `call_governanceSubmitProposal` | `GovernanceSubmitProposal { proposal_type, title, description, execution_data }` | 200,000 |
| `call_governanceVote` | `GovernanceVote { proposal_id, vote }` | 50,000 |
| `call_governanceQueue` | `GovernanceQueue { proposal_id }` | 50,000 |
| `call_governanceExecute` | `GovernanceExecute { proposal_id }` | 100,000 |
| `call_governanceEmergencyPause` | `GovernanceEmergencyPause { reason }` | 200,000 |
| `call_governanceEmergencyResume` | `GovernanceEmergencyResume` | 200,000 |
| `call_bridgeSubmitDeposit` | `ExternalBridgeDeposit { source_tx_hash, source_chain, ... }` | 200,000 |
| `call_bridgeSubmitWithdraw` | `ExternalBridgeWithdraw { target_chain, target_address, ... }` | 200,000 |
| `call_bridgeToEvm` | `BridgeToEvm { asset_id, to, amount }` | 25,000 |
| `call_withdrawFromEvm` | `WithdrawFromEvm { asset_id, to, amount }` | 25,000 |
| `call_validatorStake` | `ValidatorStake { ed25519_pubkey, self_stake }` | 200,000 |
| `call_validatorUnstake` | `ValidatorUnstake { validator_id }` | 100,000 |
| `call_validatorClaimUnbonded` | `ValidatorClaimUnbonded { validator_id }` | 100,000 |

All instruction-based endpoints require:
- `sender`: the signer's address
- `nonce`: transaction nonce for replay protection
- `signature`: 65-byte secp256k1 signature (`r || s || v`)

`call_submitRollbackSignature` additionally requires:
- `txSignature`: 65-byte secp256k1 signature for the `ProtocolTransaction` auth
- `rollbackSignature`: 64-byte Ed25519 signature embedded in the instruction payload

### Direct Execution RPC (Not Yet Instruction-Based)

These endpoints still execute state changes directly in the RPC handler and bypass the mempool/consensus flow. They should be migrated to Instruction-based models.

| Endpoint | Behavior |
|----------|----------|
| `call_lightClientBridgeDeposit` | Directly calls `process_light_client_deposit` with mutable `balance_state` and `bridge_state` access. Only available when the `light-client-bridge` feature is enabled. |

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
│  │ eth_call            │  │ call_registerAsset            │  │
│  │ eth_sendRawTx       │  │ call_agentRegister            │  │
│  │ eth_getReceipt      │  │ call_governanceSubmitProposal │  │
│  │ eth_blockNumber     │  │ call_bridgeSubmitDeposit      │  │
│  │ eth_getLogs         │  │ call_validatorStake           │  │
│  │ eth_getProof        │  │ call_submitRollbackSignature  │  │
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

### Instruction-Based Flow (Preferred)

```
RPC Handler
    ↓
Parse JSON params → construct Instruction(s)
    ↓
Build ProtocolTransaction { sender, nonce, instructions, signature, ... }
    ↓
state.insert_protocol_tx(tx) → Mempool
    ↓
Block production (consensus) selects tx from mempool
    ↓
Block::execute → execute_agent_instruction / execute_rollback_instruction / ...
    ↓
State transition committed atomically
```

### Direct Execution Flow (Deprecated)

```
RPC Handler
    ↓
Parse JSON params
    ↓
Directly acquire write lock on subsystem state
    ↓
Mutate state immediately
    ↓
Return result
```

Direct execution lacks:
- Consensus ordering and replay protection
- Uniform gas accounting
- Atomic rollback on failure
- Deterministic replay across nodes

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
| `standard.rs` | Ethereum-compatible RPC endpoints (7 methods) |
| `callchain.rs` | Callchain-native RPC endpoints (~60 methods) |
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
| Standard Ethereum RPC | Ready | Address-indexed logs, real account proofs, pending receipt marking |
| Instruction-based RPC | Ready | All state mutations go through `ProtocolTransaction` → mempool → consensus |
| Agent RPC | Ready | `call_agentRegister` / `Grant` / `Revoke` are now Instruction-based |
| Rollback RPC | Ready | `call_submitRollbackSignature` is now Instruction-based |
| Governance RPC | Ready | Signature + nonce-based replay protection on all endpoints |
| Oracle RPC | Ready | Read-only price and TWAP queries; submission is via consensus |
| Bridge RPC | Ready | Deposit + withdraw are Instruction-based; per-deposit status lookup |
| Shielded RPC | Ready | Proving via dedicated service (`call-prover`), balance query wired |
| Light client RPC | Ready | Real Merkle proofs, shielded validation correct |
| WebSocket subscriptions | Ready | Lag notifications sent to subscribers |
| CORS configuration | Ready | Configurable via `RpcConfig.cors_allowed_origins` |
