# Callchain Consensus Architecture

## Overview

Callchain uses **Commonware Simplex BFT** (`commonware-consensus` v2026.4.0) as its
production consensus engine. The engine runs in a dedicated background OS thread with
its own `commonware-p2p` network, bridged to the main tokio runtime via channels.

The old hand-rolled QC-based BFT consensus has been removed. `SimplexConsensus`
still manages validator state (staking, slashing, rewards, proposer subsets) but
does not handle vote aggregation or certificate construction — those duties are
delegated to the Commonware engine.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Background OS Thread (commonware_runtime::tokio::Runner)   │
│                                                             │
│  ┌──────────────┐   ┌──────────┐   ┌──────────┐            │
│  │ CallAutomaton│   │CallRelay │   │CallReporter│           │
│  │  (propose)   │   │(broadcast│   │(finalize) │           │
│  │  (verify)    │   │ block)   │   │            │           │
│  └──────┬───────┘   └────┬─────┘   └─────┬──────┘           │
│         │                 │                │                 │
│  ┌──────▼─────────────────▼────────────────▼──────┐          │
│  │          simplex::Engine                        │          │
│  │  - vote aggregation                             │          │
│  │  - certificate construction                     │          │
│  │  - fault proof generation                       │          │
│  └──────┬─────────────────────────────────────────┘          │
│         │                                                    │
│  ┌──────▼──────┐  ┌──────────┐  ┌──────────┐               │
│  │ vote channel│  │cert chan │  │resolve ch│               │
│  │    (ch=1)   │  │  (ch=2)  │  │  (ch=3)  │               │
│  └──────┬──────┘  └────┬─────┘  └────┬─────┘               │
│         └───────────────┴─────────────┘                     │
│                    consensus-p2p network                    │
│              (port = app-p2p-port + 1)                      │
└─────────────────────────────────────────────────────────────┘
                              │
         mpsc/oneshot channels│
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    Tokio Runtime (main)                     │
│                                                             │
│  ┌──────────────────────────────────────────┐              │
│  │          bft_event_loop                   │              │
│  │  - propose_rx  → build block from mempool │              │
│  │  - verify_rx   → execute block, reply ok │              │
│  │  - finalize_rx → commit, persist, notify│              │
│  │  - broadcast_rx→ send over app P2P      │              │
│  └──────────────────────────────────────────┘              │
│                                                             │
│  BlockCache: ConsensusDigest → Block (shared)               │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. ConsensusDigest (`crates/consensus/src/digest.rs`)

A 32-byte wrapper implementing `commonware_cryptography::Digest` and all
prerequisite codec traits (`FixedSize`, `Write`, `Read`, `Span`, `Array`, `Random`).

- Bidirectionally convertible with `call_primitives::BlockHash`
- Serves as the `Digest` type parameter for the entire Simplex engine

### 2. BlockCache (`crates/consensus/src/block_cache.rs`)

In-memory LRU cache mapping `ConsensusDigest → Block`. Shared between:

- `start_network` P2P receive loop — inserts relayed full `Block` messages from `BLOCK_CHANNEL`
- `CallAutomaton::propose` — inserts block after building
- `CallRelay::broadcast` — serializes block for P2P relay
- `bft_event_loop::verify` — looks up block by digest (with retry loop for network latency)
- `bft_event_loop::finalize` — removes block after persistence

### 3. CallAutomaton (`crates/consensus/src/bft.rs`)

Implements `CertifiableAutomaton` (extends `Automaton`):

- `genesis(epoch)` → returns `ConsensusDigest::EMPTY`
- `propose(context)` → sends `(context, reply_tx)` to tokio; tokio builds a block,
  stores it in cache, returns digest via oneshot
- `verify(context, digest)` → sends `(context, digest, reply_tx)` to tokio; tokio
  looks up block in cache, executes it, returns `true/false`

### 4. CallRelay (`crates/consensus/src/bft.rs`)

Implements `Relay`:

- `broadcast(digest, plan)` → looks up full block in cache, serializes to JSON,
  sends bytes to tokio via `broadcast_tx` for app-P2P transmission

### 5. CallReporter (`crates/consensus/src/bft.rs`)

Implements `Reporter`:

- `report(activity)` → on `Activity::Finalization`, sends `FinalizationInfo`
  (digest, round, view) to tokio via `finalize_tx`
- Other activities (notarizations, faults) are currently ignored

### 6. BFT Event Loop (`crates/node/src/lib.rs`)

Tokio task spawned by `CallNode::start_bft_engine()`. Handles four channels:

| Channel | Action |
|---------|--------|
| `propose_rx` | Select txs from mempool, build block, execute, cache, return digest |
| `verify_rx` | Look up block in cache (retry 5×100ms if missing), execute, return validity bool |
| `finalize_rx` | Load block from cache, commit to `SimplexConsensus`, persist state, broadcast announcement |
| `broadcast_rx` | Forward serialized block bytes over the app P2P network |

### 7. Boot Sequence (`crates/node/src/boot.rs`)

For **Validator** mode:
1. Load or derive an ed25519 private key from `identity_key` config (64 or 128 hex chars)
2. Load validator signing key (keystore / plaintext / AWS KMS / HashiVault)
3. Generate BLS12-381 keypair for aggregated vote signing
4. Register BLS pubkey with validator state
5. Start app P2P network
6. Start RPC servers
7. **Start BFT engine** (`start_bft_engine(ed25519_key, consensus_p2p_port)`)

For **Full / Archive** mode:
- Steps 1-6 as above
- Step 7 runs the old `block_production_loop()` (single-node, no BFT)

---

## Signing Scheme

Uses `commonware_consensus::simplex::scheme::ed25519`:

- Simplest scheme, fully attributable signatures
- Compatible with existing ed25519 validator keys
- No DKG or threshold setup required
- Configured with a static `Set<ed25519::PublicKey>` snapshotted at startup

The elector is `RoundRobin::<Sha256>::default()`.

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | ~~**Verify path lacks block receipt from relay**~~ | **Resolved** | Fixed: `block_cache` is now a shared field on `CallNode` (created in `new`). `start_network` inserts incoming full `Block` messages from `BLOCK_CHANNEL` into the cache. `bft_event_loop::verify` looks up from the shared cache with a retry loop (5×100ms) to handle network latency. |
| 2 | ~~**Oracle round missing from BFT propose**~~ | **Resolved** | Fixed: `bft_event_loop::propose` now broadcasts `OraclePriceRequest` at `ORACLE_UPDATE_INTERVAL` boundaries with delay for responses. Post-execution, it advances the oracle period, slashes outliers, distributes rewards, and clears tracking — mirroring the old `block_production_loop`. `finalize` also handles oracle period transitions for non-proposing validators. |
| 3 | ~~**Dynamic validator set not propagated**~~ | **Resolved** | Fixed: `bft_event_loop` tracks the validator set count after each finalize. When a change is detected, it logs a warning and exits the event loop, signaling the caller (via completed `JoinHandle`) that the BFT engine needs respawn with the new participant set. |
| 4 | ~~**BFT journal state not persisted**~~ | **Resolved** | Fixed: `RuntimeConfig` now uses `.with_storage_directory(data_dir.join("bft_journal"))` instead of the default temp dir. The Commonware engine's journal (notarizations, finalizations, activity buffer) now persists to disk and survives restarts. |
| 5 | **No integration tests exercise BFT engine** | **Medium** | All tests (including E2E) manually build and commit blocks. None spawn `start_bft_engine` or run the background thread. Fix: add multi-node integration tests that start BFT engines and verify cross-node finalization. |
| 6 | **VRF proposer selection replaced by RoundRobin** | **Low/Med** | Callchain spec uses VRF-based proposer subset selection (21 of 216 validators). The BFT engine uses Commonware's `RoundRobin` for leader election. This changes the security model from VRF to deterministic round-robin. If VRF is required, implement a custom `Elector`. |

---

## File Map

| File | Role |
|------|------|
| `crates/consensus/src/digest.rs` | `ConsensusDigest` — Digest trait bridge |
| `crates/consensus/src/block_cache.rs` | `BlockCache` — digest → block mapping |
| `crates/consensus/src/bft.rs` | `CallAutomaton`, `CallRelay`, `CallReporter` |
| `crates/consensus/src/simplex.rs` | `SimplexConsensus` — validator state, block lifecycle |
| `crates/node/src/lib.rs` | `bft_event_loop`, `start_bft_engine`, persistence |
| `crates/node/src/boot.rs` | Boot sequence, key loading, engine startup |
| `crates/network/src/p2p.rs` | App P2P network (block announcements, sync, oracle) |

---

## Test Status

- `cargo test -p call-consensus` — 56 passed, 0 failed
- `cargo test -p call-network` — 38 passed, 0 failed
- `cargo test -p call-node` — 53 passed, 3 failed (pre-existing telemetry integration tests)

No tests currently exercise the actual BFT engine background thread.

---

## See Also

- [Commonware Simplex BFT docs](https://docs.rs/commonware-consensus/2026.4.0/commonware_consensus/simplex/index.html)
- `docs/spec.md` §2.3, §2.4, §2.5, §12.6
