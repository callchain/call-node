# Callchain Light Client

## Overview

The Light Client (`crates/light-client`) provides trustless verification of Ethereum block headers and transaction/receipt inclusion proofs. It enables the Callchain bridge to validate cross-chain deposits without relying on validator multi-signatures.

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

## Architecture

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

| File | Role |
|------|------|
| `lib.rs` | Crate root, re-exports |
| `ethereum.rs` | `EthLightClient`, header verification with gap buffer and reorg, tx/receipt verification, bridge event parsing, anchor advancement |
| `verifier.rs` | `verify_mpt_proof()`, compact encode/decode, RLP parsing |
| `types.rs` | `EthHeader`, `TxInclusionProof`, `ReceiptProof`, `BridgeEvent`, `GenesisState`, `LightClientError` |
| `sync.rs` | Ethereum header sync (feature `eth-sync`) |

---

## Production Readiness Assessment

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
| Integration | 🟢 Ready | `call_lightClientBridgeDeposit` RPC fully wired |

---

## Production Readiness Gaps

### Resolved Gaps

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

## Test Status

- `cargo test -p call-light-client` — 26 tests covering MPT compact encode/decode roundtrip, single leaf proof, extension+leaf proof, branch proof, key not found, tampered hash rejection, header chain submission (valid, wrong parent, before anchor, duplicate, gap with buffer flush, multi-block chain), gap buffer flush, buffer full rejection, consensus verification, reorg unwind, anchor advancement, header pruning
- `cargo check --workspace --features eth-sync` — compiles with sync feature
- Missing: real-network sync tests, long-running memory pressure tests
