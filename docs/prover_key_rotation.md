# Prover Key Rotation

## Overview

The CallChain shielded pool uses Groth16 zero-knowledge proofs secured by a trusted setup ceremony. Over time, these proving/verifying keys may need to be rotated — for example, after a compromised ceremony participant is detected, or as part of a planned security upgrade. Prover key rotation enables the network to adopt new keys without restarting nodes or halting the chain.

**Key features:**
- Governance-driven rotation via on-chain proposal
- Monotonic version numbering prevents downgrade attacks
- Sunset grace period keeps old keys valid for in-flight transactions
- Runtime registration — no node restart required
- Proofs are tagged with the key version used to generate them

**Crates involved:**
- `crates/shielded/src/key_registry.rs` — `ProverRegistry`
- `crates/shielded/src/prover.rs` — `RealProver::for_version()`
- `crates/shielded/src/lib.rs` — `ZkProof.key_version`
- `crates/governance/src/types.rs` — `ProposalType::ProverKeyRotation`
- `crates/governance/src/precompile.rs` — execution logic

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Governance Proposal                           │
│  ProposalType::ProverKeyRotation                                     │
│  ├─ key_version: u32 (must be > current)                            │
│  ├─ transfer_vk_hash: [u8; 32]                                       │
│  ├─ deposit_vk_hash: [u8; 32]                                        │
│  ├─ withdraw_vk_hash: [u8; 32]                                       │
│  └─ sunset_timestamp: u64 (Unix timestamp)                          │
└────────────────────────────┬────────────────────────────────────────┘
                             │
                             ▼
              ┌──────────────────────────────┐
              │ Governance Precompile (0x203)  │
              │ execute_proposal(type=10)      │
              │ Stores rotation metadata in    │
              │ EVM storage slots              │
              └──────────────┬─────────────────┘
                             │
              ┌──────────────┴─────────────────┐
              │   Node observes pending flag    │
              │   Loads new VKs from disk       │
              │   Calls ProverRegistry::register│
              └──────────────┬─────────────────┘
                             │
                             ▼
              ┌──────────────────────────────┐
              │      ProverRegistry            │
              │  RwLock<HashMap<u32, Keys>>    │
              │  ├─ version 0 (genesis)        │
              │  ├─ version 1 (rotation #1)    │
              │  ├─ version 2 (rotation #2)    │
              │  └─ ...                        │
              │                                │
              │  current_version: RwLock<u32>   │
              │  (used for generating proofs)   │
              └──────────────┬─────────────────┘
                             │
              ┌──────────────┴─────────────────┐
              │      RealProver::for_version()  │
              │  ├─ key_version=0 → genesis VKs │
              │  ├─ key_version=1 → rotation VKs│
              │  └─ missing → verification fail │
              └─────────────────────────────────┘
```

---

## Lifecycle

### 1. Genesis Load (Boot)

At node startup, `ProverRegistry::global()` attempts to load production keys from `/var/lib/callchain/shielded_keys` and registers them as **version 0**. This is the genesis key set.

```rust
// crates/shielded/src/key_registry.rs
pub fn global() -> &'static Self {
    static INSTANCE: OnceLock<ProverRegistry> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        // Load from /var/lib/callchain/shielded_keys as version 0
        ...
    })
}
```

If the directory is missing or empty, the registry starts empty and proofs cannot be verified until keys are registered.

### 2. Rotation Proposal

A validator submits a `ProverKeyRotation` governance proposal:

```rust
// crates/governance/src/types.rs
ProposalType::ProverKeyRotation {
    key_version: u32,            // Must be > current version
    transfer_vk_hash: [u8; 32],  // Hash of new transfer circuit VK
    deposit_vk_hash: [u8; 32],   // Hash of new deposit circuit VK
    withdraw_vk_hash: [u8; 32],  // Hash of new withdraw circuit VK
    sunset_timestamp: u64,       // When old keys can be retired
}
```

This proposal type requires **validator voting** (`is_validator_proposal()` returns `true`). It follows the standard governance flow:

```
Pending → Active (after review period) → Passed (if quorum + majority)
    ↓
Queued (timelock) → Executed → Precompile stores metadata in EVM state
```

### 3. Node Execution

When the proposal executes, the governance precompile stores rotation metadata in EVM storage:

```rust
// crates/governance/src/precompile.rs (proposal type 10)
self.backend.store(
    GOVERNANCE_ADDRESS,
    storage_slot(&[b"prover_key_rotation", b"version"]),
    U256::from(key_version),
);
self.backend.store(
    GOVERNANCE_ADDRESS,
    storage_slot(&[b"prover_key_rotation", b"pending"]),
    U256::from(1u8),
);
```

Nodes monitor the pending flag. When detected, operators place the new ceremony output in the standard key directory and trigger registration:

```rust
// Node-side (not yet fully automated)
let new_keys = ProductionKeys::load("/var/lib/callchain/shielded_keys_v2")?;
ProverRegistry::global().register(2, new_keys)?;
```

### 4. Proof Generation

New proofs are always generated with the **current** key version:

```rust
// Proving server uses current keys automatically
let prover = RealProver::global(); // uses ProverRegistry::current()
let proof = prover.prove(&circuit)?;
// ZkProof.key_version is set to current version
```

### 5. Proof Verification

Validators look up the correct verifying key by the proof's tagged version:

```rust
// crates/shielded/src/lib.rs
pub fn verify_shielded_proof(proof: &ZkProof) -> bool {
    let prover = RealProver::for_version(proof.key_version)
        .expect("verifying key for version not found");
    prover.verify(proof).unwrap_or(false)
}
```

If `key_version` is missing or 0 (default for old proofs), it falls back to genesis keys.

### 6. Sunset & Cleanup

Old versions are retained for a configurable grace period so that in-flight proofs (submitted before rotation but not yet mined) remain verifiable. After the sunset period, operators call:

```rust
let removed = ProverRegistry::global()
    .sunset_older_than(Duration::from_secs(86400 * 7))?; // 7 days
```

The **current version is never removed**, even if it exceeds the age.

---

## Security Properties

| Property | Implementation |
|----------|---------------|
| **Downgrade prevention** | `register()` rejects `version <= current` (unless registry is empty). Prevents an attacker from forcing the network to accept an older, potentially compromised key set. |
| **Monotonic versions** | `KeyVersion` is a `u32` that only increases. There is no mechanism to roll back. |
| **Proof version binding** | Every `ZkProof` includes `key_version`. A proof generated with version N cannot be verified with version M (M ≠ N). |
| **Backward compatibility** | Missing or zero `key_version` defaults to genesis keys. Old proofs submitted before the first rotation remain valid. |
| **Current version protection** | `sunset_older_than()` never removes the current version, even if it is older than the threshold. |
| **Validator-only voting** | `ProverKeyRotation` is classified as a validator proposal, not a balance-weighted vote. This prevents a wealthy token holder from unilaterally changing the ZK keys. |

---

## Key Version Semantics

| Version | Meaning |
|---------|---------|
| `0` | Genesis key set loaded at boot. Default for proofs without a version tag. |
| `1` | First rotation. All new proofs use this version after registration. |
| `N` | Nth rotation. The current version is always the highest registered. |

The version is embedded in the serialized `ZkProof` as a 4-byte little-endian `u32` (added to `serialized_size()`).

---

## Operational Guide

### Performing a Key Rotation

1. **Conduct a new trusted setup ceremony** for transfer, deposit, and withdraw circuits.
2. **Place new keys on all validator nodes** at `/var/lib/callchain/shielded_keys` (or a versioned subdirectory).
3. **Submit a governance proposal** with:
   - `key_version` = current + 1
   - VK hashes from the ceremony output
   - `sunset_timestamp` = now + grace period (e.g., 7 days)
4. **Wait for proposal to pass** validator vote and timelock.
5. **Upon execution**, each node operator calls `ProverRegistry::register()` with the new keys.
6. **Monitor** — new proofs will use the new version; old proofs remain verifiable until sunset.
7. **After sunset**, call `sunset_older_than()` to free memory.

### Sunset Policy Recommendations

| Network Stage | Recommended Sunset Period |
|--------------|--------------------------|
| Devnet | 0 (immediate cleanup acceptable) |
| Testnet | 24 hours |
| Mainnet | 7–14 days |

A longer grace period reduces the risk of in-flight proofs being rejected, but increases memory usage (each retained version holds 3 VKs + 3 PKs).

---

## Future Work

The governance precompile stores rotation metadata in EVM state, but **automatic node pickup** is not yet implemented. Currently, node operators must manually trigger `ProverRegistry::register()` after observing the pending flag. A future improvement would be:

- A background task in `LightClientService` that polls the governance storage for pending rotations.
- Automatic key loading and registration when a new version is detected.
- This would make the rotation fully automatic from proposal to activation.

This is the only remaining piece for fully governance-driven key rotation; the infrastructure (registry, versioning, proof tagging, sunset) is complete.
