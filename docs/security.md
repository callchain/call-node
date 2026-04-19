# Callchain Security & Threat Model

## Overview

The Security layer (`crates/protocol/src/security.rs`, `crates/network/src/limits.rs`) provides block limits, mempool attack prevention, shielded pool defense, P2P rate limiting, consensus double-sign detection, and MEV protection primitives. However, many security controls exist as library code without being wired into the actual production execution paths, and critical authentication/authorization gaps exist across RPC, consensus, and bridge layers.

**Security model assumptions:**
- Honest majority of validators (2/3+ for BFT safety)
- Ed25519 signatures for consensus messages
- secp256k1 signatures for protocol transactions and EVM transactions
- Network peers authenticated via libp2p Noise + PeerId

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Security Controls                                           │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ Block Limits       │  │ Mempool Defense              │  │
│  │ - max_tx_size      │  │ - rate_limiter               │  │
│  │ - max_instructions │  │ - replay_protector           │  │
│  │ - max_batch_size   │  │ - address_saturation         │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ Shielded Defense   │  │ Consensus Defense            │  │
│  │ - per-block limit  │  │ - double-sign detection      │  │
│  │ - nullifier set    │  │ - slashing tracker           │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ P2P Defense        │  │ MEV Protection               │  │
│  │ - peer rate limits │  │ - commit-reveal              │  │
│  │ - msg size caps    │  │ - builder registry           │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │ Cross-Cutting Gaps (RPC auth, signature verification,   ││
│  │ consensus verification, persistence)                    ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Block Limits (`security.rs`)

`BlockLimits` enforces:
- Max tx size: 64 KB
- Max instructions per tx: 256
- Max batch recipients: 100
- Max shielded proofs per block: 50
- Max txs per block: 10,000
- Max block size: 4 MB

**Production ready:** Yes. The struct is well-defined and tested. However, it is not clear whether `validate_tx()` is called in the actual block production pipeline for all transaction types.

### 2. Mempool Defense (`security.rs`)

`MempoolDefense` combines:
- `RateLimiter`: per-address sliding window (configurable max requests / window ms)
- `ReplayProtector`: bounded HashSet of seen tx hashes, evicts ~25% when over limit
- Address saturation: max txs per address per window

**Production ready:** Partial. The library works, but it is not integrated into the RPC transaction submission path (`call_sendPayment`, `eth_sendRawTransaction`). The RPC layer accepts transactions without rate limiting.

### 3. Shielded Pool Defense (`security.rs`)

`ShieldedDefense` enforces:
- Per-block shielded tx limit
- Global nullifier set (never expires) for double-spend prevention

**Production ready:** Partial. The nullifier set prevents double-spends within the same process, but without persistent storage, a restarted node would lose all nullifier state and accept replays.

### 4. P2P Defense (`security.rs`)

`P2PDefense` provides:
- Per-peer message rate limiting
- Maximum message size enforcement

**Production ready:** Partial. The library exists but it is not wired into the actual P2P message handling path in `crates/network/src/p2p.rs`. The `NetworkLimits` struct in `limits.rs` defines defaults (50 peers, 100 msg/sec, 10 MB max) but actual enforcement is unverified.

### 5. Consensus Defense (`security.rs`)

`ConsensusDefense` detects double-signing:
- Tracks `(round, validator_id) -> block_hash` mappings
- Returns `DoubleSignEvidence` when a validator signs two different blocks at the same round
- Maintains `slashed_validators` HashSet

**Production ready:** Partial. Detection logic is correct, but:
- There is no evidence that slashing actually reduces stake or removes the validator from the active set. The `slashed_validators` set is in-memory only.
- `cleanup_old_rounds()` must be called manually; there is no automatic pruning.
- No penalty is applied beyond being added to the slashed set.

### 6. MEV Protection (`security.rs`)

`MevProtection` implements commit-reveal for Proposer-Builder Separation:
- `commit_tx()`: stores commitment hash
- `reveal_tx()`: verifies commitment matches revealed data via keccak256

**Production ready:** Not ready. The PBS system exists as a library but is not integrated into block production. No builder registration flow is wired.

---

## Threat Model & Cross-Module Gaps

### Critical Severity

| # | Gap | Module | Details |
|---|-----|--------|---------|
| 1 | **Protocol instruction signatures not verified** | Protocol | `execute_protocol_instructions()` skips secp256k1 signature verification. Anyone can craft a valid-looking transaction and have it executed. |
| 2 | **RPC has no authentication/authorization** | RPC | No API keys, JWT, or IP allowlist. Anyone can call governance, emergency pause, oracle submit, bridge deposit. |
| 3 | **Oracle signatures not verified** | RPC/Oracle | `call_oracleSubmitPrice` accepts any 64-byte blob. Fake price submissions pass. |
| 4 | **Bridge deposit signatures not verified** | RPC/Bridge | `verify_bridge_signatures` counts signatures but does not cryptographically verify them. Fake deposits pass. |
| 5 | **Light client block header signatures not verified** | RPC/Light | `call_lightVerifyBlockHeader` counts Ed25519 signatures without verification. Fake headers pass. |
| 6 | **Light client balance proofs are fake** | RPC/Light | `call_lightGetBalanceProof` hashes a string instead of computing a real Merkle proof. |
| 7 | **Storage snapshot signatures not verified** | Storage | `verify_snapshot()` counts validator signatures but does not verify Ed25519 signatures. Fake snapshots pass. |
| 8 | **Agent grant does not deduct from owner** | Agent | `grant_funds()` credits agent balance without reducing owner balance. Phantom money creation. |

### High Severity

| # | Gap | Module | Details |
|---|-----|--------|---------|
| 9 | **RPC has no TLS/HTTPS** | RPC | All traffic over plain HTTP. Sensitive operations exposed unencrypted. |
| 10 | **RPC has no rate limiting** | RPC | `max_connections` only caps connections. No per-client request throttling. DoS vector. |
| 11 | **Default agent permissions are wide open** | Agent | `allowed_assets = []` means ALL assets allowed. `daily_limit = MAX`. Unlimited scope by default. |
| 12 | **Domain verification is format-only** | Agent | `verify_domain_proof()` only checks URL syntax. No DNS or HTTP verification. |
| 13 | **Governance execute signature binding broken** | RPC/Gov | `call_governanceExecute` verifies signature format against `Address::default()`, not the actual proposer. |
| 14 | **Rollback signatures are replayable** | Upgrade | No nonce or height bound in rollback message. Same signature can be replayed indefinitely. |
| 15 | **Emergency rollback not executed** | Upgrade | `submit_rollback_signature()` returns result but no code acts on it. No state reversion. |
| 16 | **Shielded nullifiers are in-memory only** | Shielded | Nullifier set lost on restart. Double-spends possible after node restart. |
| 17 | **No network-wide upgrade sync** | Upgrade | Each node has its own ForkManager. No gossip ensures all nodes have same schedule. |

### Medium Severity

| # | Gap | Module | Details |
|---|-----|--------|---------|
| 18 | **Mempool defense not wired to RPC** | Security | `MempoolDefense` library exists but RPC endpoints do not use it. |
| 19 | **P2P defense not wired to network layer** | Security | `P2PDefense` exists but not integrated into actual P2P message handling. |
| 20 | **Slashing does not reduce stake** | Consensus | `ConsensusDefense` detects double-signs but does not economically penalize. |
| 21 | **No agent revocation/removal** | Agent | Once registered, an agent cannot be deregistered. Compromised agents remain valid. |
| 22 | **Asset registration unpermissioned** | RPC/Protocol | Anyone can register assets without fee or issuer verification. |
| 23 | **No registration fee or stake for agents** | Agent | Zero-cost agent creation enables spam. |
| 24 | **Payment signature scheme is non-standard** | RPC | Uses raw keccak256 concatenation, not EIP-191 or EIP-712. Wallet integration friction. |

---

## File Map

| File | Role |
|------|------|
| `crates/protocol/src/security.rs` | Block limits, mempool defense, shielded defense, P2P defense, consensus defense, MEV protection |
| `crates/network/src/limits.rs` | `NetworkLimits`, `NetworkError` — P2P connection/message limits |
| `crates/protocol/src/transaction.rs` | Transaction signing and auth schemes |
| `crates/crypto/src/signer.rs` | Signature verification primitives |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Block limits | Ready | Well-defined, tested, but integration into block production unverified |
| Mempool defense | Partial | Library works, not wired to RPC submission path |
| Shielded defense | Partial | Nullifier tracking works, but in-memory only |
| P2P defense | Partial | Library exists, not wired to actual P2P handlers |
| Consensus defense (double-sign) | Partial | Detection works, no economic penalty or persistence |
| MEV protection | Not ready | Commit-reveal library exists, not integrated |
| RPC security | Not ready | No TLS, no auth, no rate limiting |
| Signature verification (protocol) | Not ready | `execute_protocol_instructions` skips verification |
| Signature verification (oracle) | Not ready | Oracle submissions not verified |
| Signature verification (bridge) | Not ready | Bridge signatures counted but not verified |
| Signature verification (light client) | Not ready | Block header signatures not verified |
| Signature verification (snapshot) | Not ready | Snapshot validator signatures not verified |

---

## Test Status

- `cargo test -p call-protocol` (security tests) — covers block limits, mempool rate limiting, address saturation, replay protection, shielded per-block limits, nullifier double-spend, P2P rate limiting, consensus double-sign detection
- `cargo test -p call-network` (limits tests) — covers default limits, custom limits, validation bounds
- Missing: integration of defense layers into actual RPC/network paths, economic slashing tests, MEV commit-reveal integration tests, auth/TLS tests, signature verification tests for oracle/bridge/light-client paths
