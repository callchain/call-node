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
│  │ eth_sendRawTx       │  │ call_oracleSubmitPrice        │  │
│  │ eth_getReceipt      │  │ call_bridgeSubmitDeposit      │  │
│  │ eth_blockNumber     │  │ call_agentRegister            │  │
│  │ eth_getLogs         │  │ call_shielded*                │  │
│  │ eth_getProof (stub) │  │ call_light*                   │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ WebSocket Subscriptions                                  ││
│  │ call_subscribeNewBlocks / Payments / Bridge / ...       ││
│  └─────────────────────────────────────────────────────────┘│
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

**Gap #1 — No TLS/HTTPS support:** RPC servers bind to plain HTTP. In production, sensitive operations (governance, payments) would traverse the network unencrypted.

**Gap #2 — No authentication/authorization:** There is no API key, JWT, or IP allowlist. Anyone with network access can call any endpoint including governance and emergency pause.

**Gap #3 — No rate limiting:** `max_connections` caps concurrent connections but does not limit requests per second per client. A single connection can flood the server.

### 2. Standard Ethereum RPC (`standard.rs`)

| Endpoint | Status | Notes |
|----------|--------|-------|
| `eth_getBalance` | Ready | Reads EVM balance from `EvmState` |
| `eth_call` | Ready | Executes read-only EVM call, returns output |
| `eth_sendRawTransaction` | Ready | Decodes RLP tx, validates nonce/balance, executes immediately |
| `eth_getTransactionReceipt` | Ready | Returns protocol receipt by tx hash |
| `eth_blockNumber` | Ready | Returns current block height |
| `eth_getLogs` | Partial | Scans ALL receipts linearly (O(n)), no log index |
| `eth_getProof` | Stub | Returns empty proof always |

**Gap #4 — `eth_getLogs` is O(n) over all receipts:** The implementation loads all receipts into memory and filters linearly. With 1M+ blocks, this is a DoS vector. No bloom filter or log index exists.

**Gap #5 — `eth_getProof` returns empty stub:** Always returns `{"address": "0x0...", "storageProof": []}`. Merkle proofs for account/storage are not generated.

**Gap #6 — EVM transactions execute immediately:** `eth_sendRawTransaction` validates, inserts into mempool, and executes immediately within the RPC handler. There is no actual mempool-based block production for EVM txs — they bypass the consensus block pipeline.

### 3. Callchain Extension RPC (`callchain.rs`)

#### Payment (`call_sendPayment`)

Validates secp256k1 signature by recovering signer from a keccak256 preimage of all tx fields. This is the **only** place in the codebase where protocol-layer signatures are actually verified (contrast with `execute_protocol_instructions` which skips verification entirely).

**Gap #7 — Signature scheme is non-standard:** Uses raw keccak256 concatenation of fields, not EIP-191 or EIP-712. Wallets would need custom signing code.

**Gap #8 — Payment executes immediately:** Like EVM txs, protocol payments are executed immediately in the RPC handler, bypassing the block production pipeline. This means:
- No nonce sequencing enforcement at the RPC layer
- No gas fee deduction (fee is hardcoded to `gas_used * 10`)
- Transaction is inserted into mempool but also executed right away

#### Asset Registration (`call_registerAsset`)

Registers a new asset in the `AssetRegistry`.

**Gap #9 — No registration fee or permission check:** Anyone can register assets. The 10 CALL registration fee is not enforced. No check that the issuer address controls the registration.

#### Agent (`call_agentRegister`, `call_agentGrant`, `call_agentRevoke`)

**Gap #10 — Agent registration has no fee/permission check:** Anyone can register an agent. No staking requirement or registration fee.

**Gap #11 — `call_agentHistory` uses gas payer matching:** It searches receipts where `gas_payer == agent.owner`, which is not a meaningful agent activity metric. Actual agent instructions are not tracked in receipts.

#### Shielded (`call_shieldedDepositProve`, `call_shieldedTransferProve`, `call_shieldedBalance`)

**Gap #12 — Proving endpoints return errors:** Both proving endpoints return an error instructing users to use a CLI wallet. No remote proving service is available.

**Gap #13 — `call_shieldedBalance` always returns 0:** It does not actually query shielded notes. Users cannot check shielded balances via RPC.

#### Light Client (`call_lightVerifyBlockHeader`, `call_lightGetBalanceProof`, `call_lightVerifyShieldedTx`)

**Gap #14 — `call_lightVerifyBlockHeader` does not verify Ed25519 signatures:** The code counts signatures by checking pubkey hex string length (64 chars) but does not perform Ed25519 signature verification. The comment says "Simple match — in production would verify Ed25519 sig."

**Gap #15 — `call_lightGetBalanceProof` generates fake proofs:** It computes `keccak256("{asset_id}:{address}:{balance}")` and calls it a "Merkle proof." There is no actual Merkle tree over balances. Light clients receiving this proof cannot verify anything.

**Gap #16 — `call_lightVerifyShieldedTx` logic is inverted:** `valid` is set to `spent.is_empty() || spent.len() < nullifiers.len()`, which returns true even when some nullifiers are already spent. A fully spent transaction (all nullifiers spent) would return `valid = false`, but a partially spent one returns `valid = true`.

#### Governance (`call_governanceSubmitProposal`, `call_governanceVote`, `call_governanceExecute`)

Signature verification with replay protection (block-window nonces) is implemented for all three endpoints. `require_governance_auth` flag controls whether signatures are mandatory.

**Gap #17 — `call_governanceExecute` signature check is weak:** It verifies the signature format but uses `Address::default()` as the expected signer, meaning any valid signature (from any key) passes. The actual executor permission is checked inside `gov.execute_proposal()` but the RPC-level signature binding is broken.

#### Oracle (`call_oracleSubmitPrice`)

Accepts price submissions from validators with an Ed25519-like 64-byte signature.

**Gap #18 — Oracle signature verification not implemented:** The `OracleSubmission` stores a 64-byte signature but `oracle.submit_price()` does not verify it. The RPC endpoint accepts any 64-byte blob.

#### Bridge (`call_bridgeSubmitDeposit`, `call_lightClientBridgeDeposit`)

`call_bridgeSubmitDeposit` queues external deposits with a challenge period.

**Gap #19 — Bridge deposit signatures are counted but not cryptographically verified:** `verify_bridge_signatures()` checks that enough signatures are present but does not verify each signature against validator pubkeys. Anyone can submit a deposit with dummy signatures.

**Gap #20 — `call_lightClientBridgeDeposit` is feature-gated but compilation issues possible:** The `#[cfg(feature = "light-client-bridge")]` gate wraps an async block but the return type path may not compile correctly when the feature is off.

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

**Gap #21 — No subscription authentication:** Any WebSocket client can subscribe to any channel. No API key or IP restriction.

**Gap #22 — Broadcast channels can lag silently:** When a subscriber falls behind, `broadcast::error::RecvError::Lagged(n)` is logged but the subscriber is not notified of dropped events. Applications may miss critical events (e.g., bridge completion) without knowing.

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
    // ... 20+ more fields
}
```

**Gap #23 — Receipts are in-memory only:** `RpcState.receipts` is a `HashMap<TxHash, ProtocolReceipt>` in memory. On node restart, all receipts are lost. No persistence to DB.

**Gap #24 — Receipt pruning is hardcoded to 1000 blocks:** `finalize_block()` calls `prune_receipts(1000)`. This is not configurable and may be too aggressive for some use cases.

**Gap #25 — EVM transaction fee currency hardcoded to CALL:** In `submit_evm_tx`, the receipt always sets `fee_currency: FeeCurrency::Call` regardless of what the transaction actually paid.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `RpcConfig`, `start_http_server()`, `build_rpc_module()` |
| `handlers.rs` | `RpcState`, `NodeProposalExecutor`, `submit_payment()`, `submit_evm_tx()` |
| `standard.rs` | Ethereum-compatible RPC endpoints |
| `callchain.rs` | Callchain-native RPC endpoints (~1264 lines) |
| `ws.rs` | WebSocket subscription manager and registration |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| HTTP JSON-RPC server | 🟢 Ready | jsonrpsee is production-grade |
| Standard Ethereum RPC | 🟡 Partial | `eth_getLogs` is O(n), `eth_getProof` stubbed |
| Callchain payment RPC | 🟡 Partial | Signature verified but non-standard scheme, immediate execution |
| Governance RPC | 🟡 Partial | Signature + replay protection present, execute signature binding weak |
| Oracle RPC | 🟡 Partial | Accepts submissions but does not verify signatures |
| Bridge RPC | 🟡 Partial | Challenge period works but signature verification missing |
| Agent RPC | 🟡 Partial | No fee/permission checks |
| Shielded RPC | 🔴 Not ready | Proving unavailable, balance query returns 0 |
| Light client RPC | 🔴 Not ready | Signature verification skipped, fake balance proofs |
| WebSocket subscriptions | 🟡 Partial | Works but no auth, silent lag |
| Security | 🔴 Not ready | No TLS, no auth, no rate limiting |

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | **No TLS/HTTPS** | High | All RPC traffic is unencrypted. Sensitive operations exposed over plain HTTP. |
| 2 | **No authentication/authorization** | Critical | No API keys, JWT, or IP allowlist. Anyone can call governance, emergency pause, oracle submit. |
| 3 | **No rate limiting** | High | `max_connections` only caps connections. No per-client request throttling. DoS vector. |
| 4 | **`eth_getLogs` is O(n)** | High | Scans all receipts linearly. Will degrade severely with chain growth. |
| 5 | **`eth_getProof` stubbed** | Medium | Always returns empty proof. Breaks light client compatibility. |
| 6 | **EVM txs execute immediately** | High | Bypass block production pipeline. No consensus ordering, no block inclusion. |
| 7 | **Payment signature non-standard** | Medium | Uses raw keccak256 preimage, not EIP-191/712. Wallet integration friction. |
| 8 **Protocol payments execute immediately** | High | Same as EVM txs — bypasses block production, no gas enforcement. |
| 9 | **Asset registration unpermissioned** | Medium | No fee, no issuer verification. Anyone can spam asset registrations. |
| 10 | **Agent registration unpermissioned** | Medium | No fee, no stake. Anyone can register agents. |
| 11 | **Shielded proving unavailable** | High | RPC proving endpoints return errors. Users cannot create shielded transactions via API. |
| 12 | **`call_shieldedBalance` always 0** | Medium | Returns hardcoded 0. Users cannot query shielded balances. |
| 13 | **Light client block header sigs not verified** | Critical | `call_lightVerifyBlockHeader` counts signatures without Ed25519 verification. Fake headers pass. |
| 14 | **Light client balance proofs are fake** | Critical | `call_lightGetBalanceProof` hashes a string, not a real Merkle proof. |
| 15 | **Light client shielded validation inverted** | High | Partially spent transactions return `valid = true`. |
| 16 | **Governance execute signature binding broken** | High | `call_governanceExecute` verifies signature format against `Address::default()`, not the proposer. |
| 17 | **Oracle signatures not verified** | High | `call_oracleSubmitPrice` accepts any 64-byte signature. Fake price submissions pass. |
| 18 | **Bridge deposit signatures not verified** | Critical | `verify_bridge_signatures` counts but does not verify signatures. Fake deposits pass. |
| 19 | **Receipts in-memory only** | High | All transaction receipts are lost on node restart. No DB persistence. |
| 20 | **Receipt pruning hardcoded** | Low | 1000-block retention is not configurable. |
| 21 | **WebSocket no auth** | Medium | Any client can subscribe to sensitive channels (payments, bridge, governance). |
| 22 | **WebSocket silent lag** | Low | Lagged subscribers are not notified of dropped events. |
| 23 | **No CORS configuration** | Medium | Default jsonrpsee CORS policy may block browser dApps or be too permissive. |
| 24 | **No RPC request/response logging** | Low | No structured logging of RPC calls for audit/debugging. |

---

## Test Status

- `cargo test -p call-rpc` — unit tests cover RPC module building, subscription registration, handler state operations
- Missing: TLS tests, auth tests, rate limit tests, `eth_getLogs` performance tests, light client verification tests, bridge signature verification tests, WebSocket lag handling tests
