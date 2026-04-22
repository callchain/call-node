# Callchain Shielded Pool

## Overview

The Shielded Pool (`crates/shielded`) provides privacy-preserving transactions for Callchain's Protocol Payment Layer. It uses a **note-based UTXO model** with zero-knowledge proofs to hide transaction amounts, senders, and recipients while guaranteeing value conservation and preventing double-spends.

**Key features:**
- Incremental Merkle Tree (depth 32) for note commitment tracking
- Nullifier-based double-spend detection with BitSet compression
- ChaCha20-Poly1305 note encryption with viewing keys
- Groth16 ZK proofs (when `real-prover` feature is enabled)
- Per-block shielded transaction limit (50)
- Shielded transaction receipts for tracing
- Shielded pool balance audit capability
- Periodic spent note pruning for memory management

**Important:** The default build (`cargo build`) does **not** include real ZK proof verification. All shielded transactions pass structural validation only. Production deployments **must** enable `--features real-prover`.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Shielded Transaction Types                                  │
│                                                             │
│  ShieldedDeposit ───────┐                                   │
│  ShieldedWithdraw ──────┤  → process_deposit()              │
│  ShieldedTransfer ──────┤  → process_withdraw()             │
│                         │  → process_transfer()             │
│                         └───────────────────────────────────┤
│                                                             │
│  ┌──────────────────────┐  ┌─────────────────────────────┐ │
│  │  ZK Proof Validation │  │  Nullifier Double-Spend     │ │
│  │  (Groth16 / Struct)  │  │  Check                      │ │
│  └──────────────────────┘  └─────────────────────────────┘ │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐│
│  │  ShieldedState                                           ││
│  │  ├── merkle_tree: IncrementalMerkleTree (depth 32)      ││
│  │  ├── nullifier_set: NullifierSet (HashSet + BitSet)     ││
│  │  ├── note_registry: HashMap<Commitment, Note>           ││
│  │  ├── shielded_receipts: Vec<ShieldedReceipt>            ││
│  │  └── prune_spent_notes() → bounds memory usage          ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Notes (`notes.rs`)

A `Note` represents a shielded UTXO:

```rust
pub struct Note {
    pub value: Balance,           // amount
    pub asset_id: AssetId,        // asset type
    rcm: [u8; 32],                // random commitment mask
    recipient_ivk: [u8; 32],      // incoming viewing key
    rho: [u8; 32],                // unique nullifier seed
}
```

**Commitment:** `keccak256(value || asset_id || rcm || rho)` — inserted into Merkle tree.
**Nullifier:** Derived from `full_view_key || rho` via `keccak256` — marks note as spent.

**Viewing key derivation:** Uses domain-separated prefixes with length encoding:
```
ivk = keccak256(b"call/shielded/ivk" || spending_key || len(spending_key))
fvk = keccak256(b"call/shielded/fvk" || spending_key || len(spending_key))
```
The `call/shielded/` domain prefix prevents cross-protocol key reuse and the length encoding prevents ambiguity attacks.

**Note encryption:** ChaCha20-Poly1305 with `ivk` as the symmetric key. The `to_encrypted_bytes()` method serializes the note plaintext; `encrypt_note()` encrypts this plaintext for transmission. `try_decrypt_note()` attempts decryption and returns `true` on success, used by `ViewingKey::can_decrypt()` for actual decryption verification.

### 2. Merkle Tree (`merkle.rs`)

- **Depth:** 32 (supports ~4.2 billion leaves)
- **Hash function:** `keccak256(left || right)`
- **Empty leaf:** `Hash::repeat_byte(0)`
- **Insertion:** O(log n), append-only
- **Proof generation:** `proof_for_index()` returns sibling hashes + direction
- **`contains(leaf)`:** checks if a leaf hash exists in the tree

**Serialization behavior:** Custom `Serialize`/`Deserialize` implementations write the raw `note_registry` data and reconstruct the Merkle tree from scratch on deserialization. No explicit rebuild step is needed — the tree is always consistent with the registry after deserialization.

### 3. Nullifier Set (`nullifiers.rs`)

Dual-structure design:
- **Primary:** `HashSet<Nullifier>` — exact membership test
- **Compression:** `Vec<u64>` BitSet — 64 buckets, 64 bits each (~4KB for 200 nullifiers vs 6.4KB for HashSet)

**BitSet bucketing:**
```
bucket = first_8_bytes(hash) % 64
bit_index = next_8_bytes(hash) % 64
```

**`maybe_spent()`:** Fast approximate check using BitSet. No false negatives, possible false positives (requires `HashSet` confirmation).

**Production ready:** Yes. The nullifier set is straightforward and well-tested.

### 4. ZK Proof Verification (`lib.rs`)

**Default build (no `real-prover`):**

`verify_shielded_proof()` returns an error requiring the `real-prover` feature. Production deployments must use `--features real-prover` for Groth16 verification.

**With `real-prover` feature:**
- `RealProver::global()` singleton (trusted setup is expensive)
- Circuit types: `"deposit"`, `"withdraw"`, `"transfer"`
- Public inputs: nullifiers + commitments concatenated
- Verification: Groth16 via `ark-groth16`

**Structural validation:** `verify_zk_proof()` checks:
- `proof_data` is non-empty and ≤ 512 bytes
- At least one nullifier or commitment is present
- No duplicate nullifiers
- Nullifier/commitment count matches expected circuit inputs/outputs (circuit-specific)
- Asset ID consistency between input/output notes

**`ShieldedTransfer::validate_structure()`:** Additional structural validation before processing, checking proof size, duplicate nullifiers, and asset ID consistency across all input/output notes.

### 5. ShieldedState (`lib.rs`)

**Process transfer flow:**
1. Validate structure (`validate_structure()`)
2. Verify ZK proof (structural or Groth16)
3. Check nullifiers not already spent
4. Merkle inclusion check: verify input note commitments exist in tree
5. Check value conservation (`output_sum ≤ input_sum`, difference is intentional fee burn)
6. Mark nullifiers spent
7. Insert new commitments into Merkle tree
8. Register output notes
9. Record `ShieldedReceipt` for tracing

**Process deposit flow:**
1. Deduct from transparent balance (protocol layer)
2. Zero-value check — deposits must have `value > 0`
3. Insert commitment into Merkle tree
4. Register note
5. Record `ShieldedReceipt`

**Process withdraw flow:**
1. Check nullifier not spent
2. Mark nullifier spent
3. Credit transparent balance (protocol layer)

**Value conservation:** `output_sum <= input_sum` — the difference is intentionally burned as a protocol fee. `value_conserved_exact()` is available for strict equality checks when fee burn should be disallowed.

**Balance audit:** `shielded_pool_supply()` returns total shielded value per asset. `verify_pool_integrity()` compares shielded supply against transparent balance locks to detect inflation.

**Note pruning:** `prune_spent_notes()` removes notes whose nullifiers have been marked spent, bounding memory growth. Should be called periodically during state finalization.

### 6. Per-Block Limits

`ShieldedBlockTracker::MAX_PER_BLOCK = 50`

Each block can contain at most 50 shielded transactions. This limits the computational cost of ZK verification per block.

**Production ready:** Yes. Simple and effective rate limiting.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `ShieldedState`, `ZkProof`, `ShieldedTransfer`, viewing keys, proof verification dispatch, `ShieldedReceipt`, balance audit, note pruning |
| `notes.rs` | `Note` (UTXO), commitment/nullifier derivation, ChaCha20-Poly1305 encryption |
| `merkle.rs` | `IncrementalMerkleTree` (depth 32, keccak256), proof generation/verification |
| `nullifiers.rs` | `NullifierSet` (HashSet + BitSet compression) |
| `circuit.rs` | ZK circuit abstractions (stub without `real-prover`) |
| `prover.rs` | `RealProver` singleton, Groth16 verification (gated by `real-prover`) |
| `compliance.rs` | Shielded compliance modes (viewing key disclosure) |
| `keygen.rs` | Key generation (gated by `real-prover`) |
| `poseidon.rs` | Poseidon hash for circuits (gated by `real-prover`) |
| `merkle_poseidon.rs` | Poseidon Merkle tree (gated by `real-prover`) |
| `circuit_*.rs` | Deposit/withdraw/transfer circuits (gated by `real-prover`) |
| `proof_ser.rs` | Proof serialization (gated by `real-prover`) |
| `ceremony.rs` | Trusted setup ceremony (gated by `production-keys`) |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Note/commitment/nullifier | 🟢 Ready | Deterministic, well-tested, ChaCha20-Poly1305 encryption works |
| Viewing key derivation | 🟢 Ready | Domain-separated KDF with `call/shielded/` prefix and length encoding |
| Merkle tree | 🟢 Ready | Auto-rebuilds on deserialization, Merkle inclusion check in execution |
| Nullifier set | 🟢 Ready | HashSet + BitSet, no false negatives |
| ZK proof verification | 🟢 Ready | Groth16 via `ark-groth16` with `real-prover` feature; ceremony keys supported |
| Structural validation | 🟢 Ready | Circuit-specific count checks, asset consistency, `validate_structure()` |
| ShieldedState | 🟢 Ready | Value conservation documented, balance audit, note pruning, receipts |
| can_decrypt | 🟢 Ready | Attempts actual decryption via `try_decrypt_note()` |

---

---

## Test Status

- `cargo test -p call-shielded` — unit tests cover note creation, commitment/nullifier determinism, encryption/decryption, Merkle tree operations, nullifier set, BitSet compression, block tracker limits
- Missing: ZK proof verification tests (require `real-prover` feature), Merkle proof verification in execution, balance audit tests, deserialization round-trip tests
