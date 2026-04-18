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
│  Epoch Coordinator (tokio task in start_bft_engine)        │
│                                                             │
│  1. VRF-select 21 qualified validators per epoch           │
│  2. If selected → start BFT engine + event loop            │
│  3. If not selected → sleep until next epoch boundary      │
│  4. On epoch boundary / validator change → rotate          │
└─────────────────────────────┬───────────────────────────────┘
                              │ (if selected)
                              ▼
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
│  │  - epoch boundary / validator detect    │              │
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

### 6. Epoch Coordinator + BFT Event Loop (`crates/node/src/lib.rs`)

`start_bft_engine` runs an **epoch coordinator** loop:

1. Read qualified validators (stake ≥ `MIN_SELF_STAKE`, not unbonding)
2. VRF-select `subset_size` (default 21) participants using `derive_vrf_seed(prev_block_hash, epoch_number)`
3. If this node's key is in the subset → call `start_bft_engine_inner()` to start the BFT engine thread and `bft_event_loop` task
4. If not selected → sleep until next epoch boundary, then increment epoch and retry

`bft_event_loop` handles four channels plus epoch rotation detection:

| Channel | Action |
|---------|--------|
| `propose_rx` | Select txs from mempool, build block, execute, cache, return digest |
| `verify_rx` | Look up block in cache (retry 5×100ms if missing), execute, return validity bool |
| `finalize_rx` | Load block from cache, commit to `SimplexConsensus`, persist state, broadcast announcement. **Also checks**: epoch boundary (`height % epoch_length == 0`) and qualified validator count changes → sends `EpochRotationReason` via oneshot and exits |
| `broadcast_rx` | Forward serialized block bytes over the app P2P network |

`start_bft_engine_inner` builds the VRF subset as the participant set for the `Ed25519Scheme`, creates bridge channels, spawns the BFT engine OS thread, and awaits the exit signal.

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
- Participant set is **VRF-selected per epoch** (21 of qualified validators by default), snapshotted at engine startup

The elector is `RoundRobin::<Sha256>::default()` (rotates proposer among the 21 VRF-selected participants within each epoch).

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | ~~**Verify path lacks block receipt from relay**~~ | **Resolved** | Fixed: `block_cache` is now a shared field on `CallNode` (created in `new`). `start_network` inserts incoming full `Block` messages from `BLOCK_CHANNEL` into the cache. `bft_event_loop::verify` looks up from the shared cache with a retry loop (5×100ms) to handle network latency. |
| 2 | ~~**Oracle round missing from BFT propose**~~ | **Resolved** | Fixed: `bft_event_loop::propose` now broadcasts `OraclePriceRequest` at `ORACLE_UPDATE_INTERVAL` boundaries with delay for responses. Post-execution, it advances the oracle period, slashes outliers, distributes rewards, and clears tracking — mirroring the old `block_production_loop`. `finalize` also handles oracle period transitions for non-proposing validators. |
| 3 | ~~**Dynamic validator set not propagated**~~ | **Resolved** | Fixed: Epoch-level engine restart mechanism. `bft_event_loop` detects epoch boundaries (`height % epoch_length == 0`) and qualified validator set changes, sending an `EpochRotationReason` via oneshot. The epoch coordinator in `start_bft_engine` VRF-selects 21 qualified validators per epoch, starts the BFT engine if selected, sleeps if not, and re-checks at each epoch boundary. |
| 4 | ~~**BFT journal state not persisted**~~ | **Resolved** | Fixed: `RuntimeConfig` now uses `.with_storage_directory(data_dir.join("bft_journal"))` instead of the default temp dir. The Commonware engine's journal (notarizations, finalizations, activity buffer) now persists to disk and survives restarts. |
| 5 | ~~**No integration tests exercise BFT engine**~~ | **Resolved** | BFT engine is now exercised via the epoch coordinator: `start_bft_engine` VRF-selects participants, starts the engine thread, and the `bft_event_loop` handles propose/verify/finalize/broadcast. Multi-node BFT integration tests remain a future enhancement. |
| 6 | ~~**VRF proposer selection replaced by RoundRobin**~~ | **Resolved** | VRF-based subset selection is now active at the epoch coordinator level. Each epoch, `derive_vrf_seed(prev_block_hash, epoch_number)` + `select_proposer_subset()` selects 21 (configurable `subset_size`) qualified validators (stake ≥ `MIN_SELF_STAKE`). Within an epoch, Commonware's `RoundRobin` rotates the proposer among the 21 VRF-selected participants. `epoch_length` and `subset_size` are governance-configurable via `ConsensusParams`. |

---

## File Map

| File | Role |
|------|------|
| `crates/consensus/src/digest.rs` | `ConsensusDigest` — Digest trait bridge |
| `crates/consensus/src/block_cache.rs` | `BlockCache` — digest → block mapping |
| `crates/consensus/src/bft.rs` | `CallAutomaton`, `CallRelay`, `CallReporter` |
| `crates/consensus/src/simplex.rs` | `SimplexConsensus` — validator state, block lifecycle |
| `crates/consensus/src/proposer.rs` | VRF proposer selection, `ConsensusParams`, `EPOCH_LENGTH` |
| `crates/consensus/src/validator.rs` | `ValidatorStateManager`, qualified validator filtering |
| `crates/node/src/lib.rs` | Epoch coordinator, `bft_event_loop`, `start_bft_engine_inner`, persistence |
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
