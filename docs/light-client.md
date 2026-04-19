# Callchain Light Client

## Overview

The Light Client (`crates/light-client`) provides trustless verification of Ethereum block headers and transaction/receipt inclusion proofs. It enables the Callchain bridge to validate cross-chain deposits without relying on validator multi-signatures.

**Security model:**
- Starts from a trusted anchor (known-good finalized Ethereum block)
- Verifies each subsequent header links to the anchor via `parent_hash` chain
- Transaction inclusion proven via Merkle-Patricia Trie (MPT) proofs against `transactions_root`
- Receipt inclusion proven via MPT proofs against `receipts_root`
- Bridge events parsed from verified receipt logs

**Important:** This light client does NOT verify Ethereum's consensus layer (BLS signatures from beacon chain). It relies on the parent_hash chain for header validity.

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
│                                                             │
│  submit_header() ──→ verify parent_hash chain               │
│       │                                                     │
│       └──→ verify_tx_inclusion() ──→ MPT proof vs tx_root   │
│       └──→ verify_receipt_and_parse_bridge_event()          │
│                └──→ MPT proof vs receipts_root              │
│                └──→ parse receipt logs                      │
│                └──→ extract BridgeEvent                     │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Header Verification (`ethereum.rs`)

`EthLightClient::submit_header()` verifies:
1. Block number > anchor block
2. No duplicate header at this block number
3. `parent_hash` matches a previously verified header
4. Block hash matches keccak256(RLP encoding)

**Gap #1 — No Ethereum consensus verification:** The light client only checks `parent_hash` continuity. It does not verify PoW (pre-Merge) or BLS signatures (post-Merge). A malicious fork of Ethereum could potentially feed headers that follow the parent chain but were not finalized by consensus.

**Gap #2 — No reorg handling:** Once a header is accepted, it is never removed. If Ethereum experiences a reorg, the light client continues to trust the orphaned chain. There is no mechanism to unwind and follow the new canonical chain.

**Gap #3 — Memory grows unbounded:** All verified headers are stored in a `HashMap<u64, EthHeader>`. There is no pruning or snapshotting. At 12-second Ethereum block times, this accumulates ~2.6M headers per year.

**Gap #4 — No gap tolerance:** `submit_header` requires the parent block to be already verified. If block N+2 arrives before N+1, it is rejected. There is no buffering or catch-up mechanism.

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
3. Verifies MPT proof
4. Parses receipt RLP to extract logs
5. Finds bridge deposit event in logs

**Gap #5 — MPT proof uses empty key for receipts:** The function calls `verify_mpt_proof(receipts_root, &[], &proof_rlps)` with an empty key. In Ethereum's receipts trie, the key is the RLP-encoded receipt index (transaction position), not empty. This means the proof verification will likely fail or return incorrect data for real Ethereum receipts.

**Gap #6 — Bridge event parsing accepts any log structure:** `parse_bridge_event_from_logs()` does not check the event signature hash (topics[0]). Any log with the right number of fields and data structure would be accepted as a bridge deposit event, even if it came from an unrelated contract.

**Gap #7 — Receipt parsing assumes legacy format:** `parse_receipt_logs()` handles the legacy receipt RLP format but does not support EIP-2718 typed transactions (Type 0x01, 0x02), which are now dominant on Ethereum.

### 5. Light Client Bridge Deposit (`rpc/src/callchain.rs`)

The `call_lightClientBridgeDeposit` RPC endpoint (feature-gated by `light-client-bridge`) accepts an Ethereum header RLP, tx proof, and receipt proof, then processes a bridge deposit.

**Gap #8 — Light client bridge is not integrated into normal block production:** The endpoint is available via RPC but the light client verification path is not used during consensus block execution. Bridge deposits in blocks still rely on validator signatures.

**Gap #9 — No anchor update mechanism:** The genesis anchor is fixed at initialization. There is no mechanism to advance the anchor to a more recent finalized block, which would allow pruning old headers.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | Crate root, re-exports |
| `ethereum.rs` | `EthLightClient`, header verification, tx/receipt verification, bridge event parsing |
| `verifier.rs` | `verify_mpt_proof()`, compact encode/decode, RLP parsing |
| `types.rs` | `EthHeader`, `TxInclusionProof`, `ReceiptProof`, `BridgeEvent`, `GenesisState` |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Header chain verification | 🟡 Partial | Parent hash chain works, no consensus verification, no reorg handling |
| MPT proof verifier | 🟢 Ready | Well-tested, handles all node types |
| Transaction inclusion | 🟢 Ready | Correct key and root usage |
| Receipt inclusion | 🔴 Not ready | Empty key bug, typed receipt format not supported |
| Bridge event parsing | 🔴 Not ready | No event signature check, accepts any log structure |
| Memory management | 🔴 Not ready | Unbounded header storage, no pruning |
| Integration | 🔴 Not ready | Not used in consensus block production |

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | **No Ethereum consensus verification** | Critical | Only parent_hash chain is checked. No BLS/PoW verification. |
| 2 | **No reorg handling** | Critical | Orphaned headers are never removed. Light client can follow wrong chain. |
| 3 | **Unbounded memory growth** | High | All headers stored forever. No pruning or snapshotting. |
| 4 | **No gap tolerance** | Medium | Missing intermediate blocks cause rejection. No catch-up. |
| 5 | **Receipt proof uses empty key** | Critical | `verify_mpt_proof` called with `&[]` as key. Real receipts need index key. |
| 6 | **No event signature check** | High | Any log with right structure is accepted as bridge event. |
| 7 | **Typed receipts not supported** | High | EIP-2718 typed transactions (dominant on Ethereum) not parsed. |
| 8 | **Not integrated into block production** | High | Light client path not used during consensus. Validator sigs still required. |
| 9 | **No anchor advancement** | Medium | Genesis anchor is fixed. Cannot advance to newer finalized blocks. |
| 10 | **No sync from Ethereum** | Medium | Headers must be submitted manually. No automated sync. |

---

## Test Status

- `cargo test -p call-light-client` — tests cover MPT compact encode/decode roundtrip, single leaf proof, extension+leaf proof, branch proof, key not found, tampered hash rejection, header chain submission (valid, wrong parent, before anchor, duplicate, gap, multi-block chain)
- Missing: receipt proof with real index key, typed receipt parsing, bridge event signature validation, reorg handling tests, memory pressure tests, consensus integration tests
