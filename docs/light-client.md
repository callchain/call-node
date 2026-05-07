# Callchain Light Client

## Overview

Callchain maintains **two** light clients:

1. **Protocol Light Client** (`crates/node/src/light_client.rs`) — verifies Callchain's own block headers using validator BLS aggregate signatures and parent-hash chain. Runs as an independent tokio task (`LightClientService`) that actively gossips verified headers across the P2P network.
2. **Ethereum Light Client** (`crates/light-client`) — verifies Ethereum block headers and transaction/receipt inclusion proofs via MPT proofs. Enables the Callchain bridge to validate cross-chain deposits.

---

## Protocol Light Client (`crates/node/src/light_client.rs`)

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  LightClientService (independent tokio task)                 │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ LightClient                                          │   │
│  │  • chain_id, trusted_validators, total_validators   │   │
│  │  • bls_pubkeys: HashMap<ValidatorId, [u8; 48]>      │   │
│  │  • verified_headers: HashMap<u64, BlockHash>        │   │
│  │  • latest_block_header: Option<BlockHeader>         │   │
│  │  • db: Option<Arc<DatabaseEnv>>  (persistent)       │   │
│  └─────────────────────────────────────────────────────┘   │
│                          ▲                                  │
│           ┌──────────────┴──────────────┐                  │
│           │                             │                  │
│    LocalBlock event              PeerAnnouncement event     │
│    (from block producer)       (from P2P LIGHT_CLIENT_CH)  │
│           │                             │                  │
│           └──────────────┬──────────────┘                  │
│                          │                                  │
│              sync_incremental(header, signatures)           │
│                          │                                  │
│           ┌──────────────┼──────────────┐                  │
│           ▼              ▼              ▼                  │
│      verify_header()  persist()    if epoch boundary:      │
│      • parent_hash    to MDBX       refresh_validator_set()│
│      • BLS aggregate                re-read from EVM state │
│      • quorum check                                         │
│      • basic checks                                         │
│                                                             │
│    On LocalBlock success ──► broadcast HeaderAnnouncement  │
│    On PeerAnnouncement ──► verify only (no rebroadcast)    │
└─────────────────────────────────────────────────────────────┘
```

### Key Features

| Feature | Implementation |
|---------|----------------|
| **BLS aggregate signature verification** | `bls_verify_aggregate()` over block hash using validator subset bitmap |
| **Parent-hash chain** | Each header's `parent_hash` must match the previously verified header at `height-1` |
| **Reorg handling** | `handle_reorg()` removes all verified headers at or above the fork height (memory + MDBX) |
| **Persistent storage** | Verified headers saved to MDBX via `save_light_client_header()` on every `sync_incremental()` |
| **Validator set refresh** | `refresh_validator_set()` re-reads validator count, addresses, pubkeys, and BLS pubkeys from EVM state |
| **Active header gossip** | `LightClientService` broadcasts `HeaderAnnouncement` to peers on `LIGHT_CLIENT_CHANNEL` (6) after local block finalization |
| **No amplification** | Peer announcements are verified but never re-broadcast, preventing gossip amplification |

### Wire Format

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderAnnouncement {
    pub header: BlockHeader,
    pub signatures: BlockSignatures,
}
```

Serialized with **bincode** (not wrapped in `NetworkMessage`) and sent on `LIGHT_CLIENT_CHANNEL = 6`.

### Boot Sequence

```
1. open DB / load genesis
2. start_network()     ← network receive loop dispatches LIGHT_CLIENT_CHANNEL
3. start_light_client_service()
   └─ reads validator set from EVM state
   └─ spawns LightClientService tokio task
4. start consensus (solo/BFT)
   └─ block producer / BFT finalize sends LocalBlock events to service
```

---

## Ethereum Light Client (`crates/light-client`)

Provides trustless verification of Ethereum block headers and transaction/receipt inclusion proofs. Enables the Callchain bridge to validate cross-chain deposits without relying on validator multi-signatures.

**Security model:**
- Starts from a trusted anchor (known-good finalized Ethereum block)
- Verifies each subsequent header links to the anchor via `parent_hash` chain
- Out-of-order headers are buffered and flushed when their parent arrives (gap tolerance)
- Reorg detection unwinds orphaned headers above the fork point
- Transaction inclusion proven via Merkle-Patricia Trie (MPT) proofs against `transactions_root`
- Receipt inclusion proven via MPT proofs against `receipts_root`
- Bridge events parsed from verified receipt logs
- Finalized checkpoint from Ethereum consensus can be set externally for consensus verification

**Important:** This light client does NOT verify Ethereum's consensus layer (BLS signatures from beacon chain). It relies on the parent_hash chain for header validity, with optional consensus checkpoint tracking via `set_finalized_block()`.

---

## Ethereum Light Client Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  EthLightClient                                              │
│                                                             │
│  GenesisState (trusted anchor)                              │
│    ├── anchor_hash: B256                                    │
│    └── anchor_block: u64                                    │
│                                                             │
│  Verified Headers: block_number → EthHeader                 │
│  Gap Buffer: BTreeMap<u64, EthHeader>                       │
│  Finalized: Option<(u64, B256)>                             │
│                                                             │
│  submit_header() ──→ verify parent_hash chain               │
│       │                  │                                  │
│       │                  └──→ parent missing → buffer        │
│       │                  └──→ parent mismatch → handle_reorg │
│       │                                                     │
│       └──→ flush_buffer() ──→ process buffered headers      │
│       │                                                     │
│       └──→ verify_tx_inclusion() ──→ MPT proof vs tx_root   │
│       └──→ verify_receipt_and_parse_bridge_event()          │
│                └──→ MPT proof vs receipts_root              │
│                └──→ parse receipt logs                      │
│                └──→ extract BridgeEvent                     │
│                                                             │
│  advance_anchor() ──→ prune old headers                     │
│  set_finalized_block() ──→ track consensus checkpoint       │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Header Verification (`ethereum.rs`)

`EthLightClient::submit_header()` verifies:
1. Block number > anchor block
2. No duplicate header at this block number
3. `parent_hash` matches a previously verified header
   - If parent is missing but block is within range, header is buffered (gap tolerance)
   - If parent exists but hash mism, reorg handling is triggered
4. Block hash matches keccak256(RLP encoding)

**Gap tolerance:** Headers arriving out of order are stored in a `BTreeMap<u64, EthHeader>` buffer (max 64). When a missing parent arrives, buffered headers are flushed recursively in order.

**Reorg handling:** When a header's parent hash doesn't match the expected chain, `handle_reorg()` walks back to find the fork point, unwinds orphaned headers above it, and inserts the new header. Reorgs below the finalized block are rejected.

**Consensus verification:** `set_finalized_block()` tracks an externally-confirmed finalized checkpoint. Headers at or below this block are considered consensus-verified. `is_consensus_verified(block)` checks this.

**Anchor advancement:** `advance_anchor()` moves the trusted anchor forward to a more recent verified block and prunes all headers below it, freeing memory.

### 2. MPT Proof Verifier (`verifier.rs`)

Implements read-only Merkle-Patricia Trie proof verification:

- `verify_mpt_proof(root_hash, key_bytes, proof_nodes)` → `Option<Vec<u8>>`
- Supports leaf, extension, and branch nodes
- Recomputes node hashes at each step
- Handles compact encoding (nibbles)

**Production ready:** Yes. The MPT verifier is well-tested with leaf-only, extension+leaf, branch, and tampered-hash failure cases.

### 3. Transaction Inclusion Verification (`ethereum.rs`)

`verify_tx_inclusion()` checks that a transaction hash exists in the block's transactions trie via MPT proof.

**Production ready:** Yes. Correctly uses `transactions_root` from the verified header and the tx_hash as the MPT key.

### 4. Receipt Verification and Bridge Event Parsing (`ethereum.rs`)

`verify_receipt_and_parse_bridge_event()`:
1. Looks up verified header
2. Extracts `receipts_root`
3. Verifies MPT proof using `rlp_encode_u64(receipt_index)` as the MPT key
4. Parses receipt RLP to extract logs, stripping EIP-2718 type byte (0x00–0x7f) for typed receipts
5. Finds bridge deposit event in logs, validating `topics[0]` against `BRIDGE_DEPOSIT_EVENT_SIG`

### 5. Light Client Bridge Deposit (`rpc/src/callchain.rs`)

The `call_lightClientBridgeDeposit` RPC endpoint (feature-gated by `light-client-bridge`) accepts an Ethereum header RLP, tx proof, and receipt proof, then processes a bridge deposit. It verifies header via `submit_header()`, tx inclusion via `verify_tx_inclusion()`, receipt via `verify_receipt_and_parse_bridge_event()`, and queues the deposit for the challenge period.

**Anchor advancement:** `advance_anchor()` advances the trusted anchor to a more recent verified block and prunes headers below it, freeing memory.

### 6. Ethereum Sync (`sync.rs` — feature `eth-sync`)

- `sync_single_header(eth_rpc_url, block_number)` — fetches header via `eth_getBlockByNumber`
- `sync_header_range(eth_rpc_url, client, from, to)` — iteratively fetches and submits headers
- `fetch_finalized_checkpoint(beacon_url)` — fetches finalized checkpoint from beacon API

---

## File Map

### Protocol Light Client

| File | Role |
|------|------|
| `crates/node/src/light_client.rs` | `LightClient` — BLS aggregate verification, parent-hash chain, reorg handling, persistent header storage, Merkle/ZK proof verification |
| `crates/node/src/light_client_service.rs` | `LightClientService` — independent tokio task, event loop, header gossip broadcast, peer announcement verification |
| `crates/node/src/network_handler.rs` | `LIGHT_CLIENT_CHANNEL = 6` constant |
| `crates/node/src/lib.rs` | `start_light_client_service()` method, network receive loop dispatch |
| `crates/node/src/boot.rs` | Boot sequence wiring |

### Ethereum Light Client

| File | Role |
|------|------|
| `crates/light-client/src/lib.rs` | Crate root, re-exports |
| `crates/light-client/src/ethereum.rs` | `EthLightClient`, header verification with gap buffer and reorg, tx/receipt verification, bridge event parsing, anchor advancement |
| `crates/light-client/src/verifier.rs` | `verify_mpt_proof()`, compact encode/decode, RLP parsing |
| `crates/light-client/src/types.rs` | `EthHeader`, `TxInclusionProof`, `ReceiptProof`, `BridgeEvent`, `GenesisState`, `LightClientError` |
| `crates/light-client/src/sync.rs` | Ethereum header sync (feature `eth-sync`) |

---

## Production Readiness Assessment

### Protocol Light Client

| Component | Status | Notes |
|-----------|--------|-------|
| BLS aggregate signature verification | 🟢 Ready | `bls_verify_aggregate()` with validator subset bitmap |
| Parent-hash chain | 🟢 Ready | `verify_header()` checks `parent_hash` against verified headers |
| Reorg handling | 🟢 Ready | `handle_reorg()` removes orphaned headers from memory + MDBX |
| Persistent storage | 🟢 Ready | `CallLightClientHeaders` MDBX table, load on startup, save on sync |
| Validator set refresh | 🟢 Ready | `refresh_validator_set()` re-reads from EVM state at epoch boundaries |
| Independent service | 🟢 Ready | `LightClientService` runs as standalone tokio task |
| Active header gossip | 🟢 Ready | Broadcasts `HeaderAnnouncement` after local block finalization |
| No amplification | 🟢 Ready | Peer announcements verified but not re-broadcast |

### Ethereum Light Client

| Component | Status | Notes |
|-----------|--------|-------|
| Header chain verification | 🟢 Ready | Parent hash chain works, gap buffer, reorg handling, finalized checkpoint tracking |
| MPT proof verifier | 🟢 Ready | Well-tested, handles all node types |
| Transaction inclusion | 🟢 Ready | Correct key and root usage |
| Receipt inclusion | 🟢 Ready | Correct index key usage, typed receipt format supported |
| Bridge event parsing | 🟢 Ready | Event signature check enforced, structured parsing |
| Memory management | 🟢 Ready | Anchor advancement with pruning, finalized block protection |
| Gap tolerance | 🟢 Ready | Out-of-order headers buffered and flushed |
| Ethereum sync | 🟢 Ready | `eth-sync` feature for automated header fetch |
| Integration | 🔴 BLOCKED | `call_lightClientBridgeDeposit` RPC disabled (see Known Issues) |

---

## Production Readiness Gaps

### Resolved Gaps — Protocol Light Client

| # | Fix | Details |
|---|-----|---------|
| 10 | **Independent service** | `LightClientService` is a standalone tokio task, not tied to sync. Receives local blocks from producer/BFT and peer announcements from P2P. |
| 11 | **Active header broadcast** | After local block verification, service broadcasts `HeaderAnnouncement` on `LIGHT_CLIENT_CHANNEL`. |
| 12 | **Validator set refresh** | `refresh_validator_set()` re-reads validator count, addresses, pubkeys, and BLS pubkeys from EVM state. Triggered at epoch boundaries. |

### Resolved Gaps — Ethereum Light Client

| # | Fix | Details |
|---|-----|---------|
| 1 | **Consensus verification via finalized checkpoint** | `set_finalized_block()` tracks externally-confirmed finalized block. Headers at or below are consensus-verified. Reorgs below finalized are rejected. |
| 2 | **Reorg handling** | `handle_reorg()` walks back to find fork point, unwinds orphaned headers, inserts new header. Before-finalized reorgs rejected. |
| 3 | **Memory pruning** | `advance_anchor()` prunes headers below new anchor. `prune_headers()` available for manual cleanup. Finalized and anchor blocks never pruned. |
| 4 | **Gap tolerance buffer** | Out-of-order headers stored in BTreeMap buffer (max 64). `flush_buffer()` processes them in order when parent arrives. |
| 5 | **Receipt proof uses correct index key** | `verify_mpt_proof` now uses `rlp_encode_u64(receipt_index)` as the MPT key. |
| 6 | **Event signature check enforced** | `topics[0]` validated against `BRIDGE_DEPOSIT_EVENT_SIG` keccak256 hash. |
| 7 | **Typed receipts supported** | EIP-2718 type byte stripped before RLP parsing. |
| 8 | **Light client bridge integrated** | `process_light_client_deposit()` fully implements header submission, tx/receipt verification, event parsing, and deposit queuing. |
| 9 | **Anchor advancement** | `advance_anchor()` moves anchor to verified block, prunes headers below it. |
| 10 | **Sync from Ethereum** | `eth-sync` feature provides `sync_single_header()`, `sync_header_range()`, `fetch_finalized_checkpoint()`. |

---

## Known Architecture Issues

### `call_lightClientBridgeDeposit` writes EVM storage directly from RPC layer

**Issue:** The `call_lightClientBridgeDeposit` RPC endpoint previously performed light-client verification (MPT proofs, receipt parsing) and then **directly wrote to `EvmState`** via `process_light_client_deposit_evm()`.

**Status: BLOCKED (2026-05-03).** The endpoint now returns an error immediately:
```
call_lightClientBridgeDeposit is disabled: direct EVM writes are not permitted.
Use standard bridge deposit flow.
```

The original direct-execution code has been removed from `crates/rpc/src/handlers/callchain.rs`.

**Why the original code was problematic:**

| Aspect | Normal EVM transaction path | `call_lightClientBridgeDeposit` (old) |
|--------|---------------------------|---------------------------------------|
| Submission | `eth_sendRawTransaction` | Direct RPC call |
| Execution | Mempool → consensus → block producer → EVM execution | RPC handler only |
| Transaction record | Has tx hash, receipt, gas consumed | **No transaction record** |
| State root inclusion | Changes captured in block state root | **Only local node state changes** |
| Network verification | All nodes re-execute and validate | **Other nodes do not know about it** |

**Impact:** This broke the EVM-Only State Architecture invariant that *all state mutations go through the EVM execution path and are captured in the block state root*. In a multi-node network, nodes that did not receive this RPC call would have divergent EVM state.

**Mitigation applied:**
- Endpoint blocked at RPC layer. No direct EVM writes.
- `call_submit` (the legacy protocol-transaction submission endpoint) also removed entirely.

**Future path (when re-implemented):**
1. Users submit the proof as an EVM transaction (via a precompile at a dedicated address).
2. The precompile delegates heavy verification (RLP, MPT) to native code (similar to `ecrecover`).
3. Block producers validate proofs during block execution, state change committed atomically with the block.

---

## Test Status

- `cargo test -p call-light-client` — 26 tests covering MPT compact encode/decode roundtrip, single leaf proof, extension+leaf proof, branch proof, key not found, tampered hash rejection, header chain submission (valid, wrong parent, before anchor, duplicate, gap with buffer flush, multi-block chain), gap buffer flush, buffer full rejection, consensus verification, reorg unwind, anchor advancement, header pruning
- `cargo check --workspace --features eth-sync` — compiles with sync feature
- Missing: real-network sync tests, long-running memory pressure tests
