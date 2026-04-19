# Callchain Shielded Pool

## Overview

The Shielded Pool (`crates/shielded`) provides privacy-preserving transactions for Callchain's Protocol Payment Layer. It uses a **note-based UTXO model** with zero-knowledge proofs to hide transaction amounts, senders, and recipients while guaranteeing value conservation and preventing double-spends.

**Key features:**
- Incremental Merkle Tree (depth 32) for note commitment tracking
- Nullifier-based double-spend detection with BitSet compression
- ChaCha20-Poly1305 note encryption with viewing keys
- Groth16 ZK proofs (when `real-prover` feature is enabled)
- Per-block shielded transaction limit (50)

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
│  │  └── note_registry: HashMap<Commitment, Note>           ││
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

**Viewing key derivation:**
```
ivk = keccak256(b"ivk" || spending_key)
fvk = keccak256(b"fvk" || spending_key)
```

**Note encryption:** ChaCha20-Poly1305 with `ivk` as the symmetric key. The `to_encrypted_bytes()` method serializes the note plaintext; `encrypt_note()` encrypts this plaintext for transmission.

**Gap #1 — Viewing key derivation uses non-standard KDF:** The viewing key is derived by simple `keccak256` concatenation without HKDF or PBKDF2. This does not meet cryptographic best practices for key derivation and could weaken privacy guarantees if the spending key has low entropy.

### 2. Merkle Tree (`merkle.rs`)

- **Depth:** 32 (supports ~4.2 billion leaves)
- **Hash function:** `keccak256(left || right)`
- **Empty leaf:** `Hash::repeat_byte(0)`
- **Insertion:** O(log n), append-only
- **Proof generation:** `proof_for_index()` returns sibling hashes + direction

**Serialization behavior:** The Merkle tree is **not serialized**. `serialize_merkle` writes an empty byte array; `deserialize_merkle` returns a fresh empty tree. After deserialization, `rebuild_merkle_tree()` must be called to reconstruct the tree from `note_registry`.

**Gap #2 — Deserialization requires explicit rebuild:** If `deserialize_and_rebuild()` is not used (or if someone deserializes directly), the Merkle tree will be empty while `note_registry` contains entries. This causes `merkle_root()` to return the empty-tree root, breaking any code that depends on the root for verification.

**Gap #3 — No Merkle proof verification in instruction execution:** The `ShieldedTransfer` instruction verifies ZK proofs and checks nullifiers, but it does **not** verify that the spent notes' commitments exist in the Merkle tree. A forged proof could claim to spend a note that was never deposited.

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
```rust
pub fn verify_shielded_proof(_proof: &ZkProof, _circuit_type: &str) -> Result<bool, String> {
    Err("ZK proof verification requires the `real-prover` feature")
}
```

**With `real-prover` feature:**
- `RealProver::global()` singleton (trusted setup is expensive)
- Circuit types: `"deposit"`, `"withdraw"`, `"transfer"`
- Public inputs: nullifiers + commitments concatenated
- Verification: Groth16 via `ark-groth16`

**Gap #4 — Default build accepts any shielded transaction:** Without `--features real-prover`, `verify_zk_proof()` only does structural checks (non-empty proof data, size < 512, no duplicate nullifiers). An attacker can submit arbitrary `proof_data` and have the transaction accepted. This is **documented** in the code but is a critical deployment gap.

**Gap #5 — Structural validation is insufficient:** `verify_zk_proof()` checks:
- `proof_data` is non-empty and ≤ 512 bytes
- At least one nullifier or commitment is present
- No duplicate nullifiers

It does **not** check:
- Number of nullifiers matches expected circuit inputs
- Number of commitments matches expected outputs
- Asset ID consistency between proof and instruction
- Nullifiers are not already spent (this is checked separately)

### 5. ShieldedState (`lib.rs`)

**Process transfer flow:**
1. Verify ZK proof (structural or Groth16)
2. Check nullifiers not already spent
3. Check value conservation (`output_sum ≤ input_sum`)
4. Mark nullifiers spent
5. Insert new commitments into Merkle tree
6. Register output notes

**Process deposit flow:**
1. Deduct from transparent balance (protocol layer)
2. Insert commitment into Merkle tree
3. Register note

**Process withdraw flow:**
1. Check nullifier not spent
2. Mark nullifier spent
3. Credit transparent balance (protocol layer)

**Gap #6 — Value conservation allows implicit fees:** `ShieldedTransfer::value_conservable()` checks `output_sum <= input_sum`, not `==`. The difference (`input_sum - output_sum`) is effectively burned (no recipient gets it). This could be intentional (miner fee) but is undocumented and could be exploited.

**Gap #7 — `process_transfer` skips value conservation for deposits:** When `input_notes.is_empty()` (which happens for shielded deposits that are processed as transfers), value conservation is skipped entirely. The comment says "enforced by ZK circuit" but without `real-prover`, there is no ZK enforcement.

**Gap #8 — No per-asset shielded balance tracking:** There is no mechanism to verify that the total value in the shielded pool matches the total transparent value locked. An attacker who bypasses ZK verification could inflate shielded balances without corresponding transparent deposits.

### 6. Per-Block Limits

`ShieldedBlockTracker::MAX_PER_BLOCK = 50`

Each block can contain at most 50 shielded transactions. This limits the computational cost of ZK verification per block.

**Production ready:** Yes. Simple and effective rate limiting.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `ShieldedState`, `ZkProof`, `ShieldedTransfer`, viewing keys, proof verification dispatch |
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
| Merkle tree | 🟡 Partial | Serialization requires explicit rebuild, no proof verification in execution |
| Nullifier set | 🟢 Ready | HashSet + BitSet, no false negatives |
| ZK proof verification | 🔴 Not ready | Default build has no verification; `real-prover` feature required |
| ShieldedState | 🟡 Partial | Value conservation gaps, no per-asset balance audit |
| Viewing keys | 🟡 Partial | Non-standard KDF, `can_decrypt` is a stub |

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | **Non-standard viewing key KDF** | Medium | `keccak256(b"ivk" \|\| spending_key)` is not HKDF or PBKDF2. Weak spending keys are directly exposed. |
| 2 | **Merkle tree deserialization requires explicit rebuild** | High | Default serde deserialization produces an empty tree. `deserialize_and_rebuild()` must be used. If forgotten, Merkle root will be incorrect. |
| 3 | **No Merkle inclusion proof in instruction execution** | High | `ShieldedTransfer` does not verify that spent notes exist in the Merkle tree. Forged proofs can claim non-existent notes. |
| 4 | **Default build has no ZK verification** | Critical | Without `--features real-prover`, any `proof_data` passes validation. All shielded security guarantees are void. |
| 5 | **Structural ZK validation insufficient** | High | `verify_zk_proof()` does not validate input/output counts, asset consistency, or circuit-specific constraints. |
| 6 | **Value conservation allows implicit burn** | Medium | `output_sum <= input_sum` allows value to disappear. This may be intentional (fees) but is undocumented. |
| 7 | **Deposits skip value conservation** | High | When `input_notes.is_empty()`, value conservation is skipped. Without `real-prover`, deposit amounts are unverified. |
| 8 | **No shielded balance audit** | High | No mechanism verifies that total shielded value equals total transparent value locked. Inflation attacks possible without ZK. |
| 9 | **`can_decrypt` is a stub** | Low | `ViewingKey::can_decrypt()` only checks non-zero bytes and length. It does not actually attempt decryption. |
| 10 | **No trusted setup persistence** | Medium | `RealProver::global()` loads proving/verification keys from disk but there is no documented setup ceremony output or key distribution mechanism. |
| 11 | **Note registry grows unbounded** | Medium | `note_registry` is a `HashMap` that only grows. Spent notes are never pruned. At scale, this will consume unbounded memory. |
| 12 | **No shielded transaction receipt** | Medium | Shielded transactions do not produce receipts or event logs. Users cannot trace transaction status without scanning blocks. |

---

## Test Status

- `cargo test -p call-shielded` — unit tests cover note creation, commitment/nullifier determinism, encryption/decryption, Merkle tree operations, nullifier set, BitSet compression, block tracker limits
- Missing: ZK proof verification tests (require `real-prover` feature), Merkle proof verification in execution, balance audit tests, deserialization safety tests
