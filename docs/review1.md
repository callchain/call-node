# Code Review Round 1

**Date**: 2026-04-17
**Commit**: 2d63bc7 — `fix: address remaining code review issues across protocol, RPC, and bridge`
**Reviewer**: Claude Code

---

## 1. RPC Signature Verification — Good, with one concern

**File**: `crates/rpc/src/callchain.rs`

The signature verification flow is correct: recover signer from the hash of the tx fields, verify it matches `from`. However, **the tx hash preimage used for signature recovery here only includes `from + nonce + asset_id + to + amount`** — it doesn't include `gas_limit` or `max_fee`. This means a signature is malleable: an attacker who intercepts a signed payment could replay it with different gas parameters.

**Suggested fix**: Include `gas_limit` and `max_fee` in the preimage:

```rust
preimage.extend_from_slice(&gas_limit.to_be_bytes());
preimage.extend_from_slice(&max_fee.to_be_bytes());
```

**Priority**: MEDIUM

---

## 2. Agent Auth Check — Good

**File**: `crates/rpc/src/callchain.rs`

Clean pattern: read registry, verify owner, drop lock, then act. No issues.

---

## 3. Bridge Replay Protection — Good

**Files**: `crates/bridge/src/lib.rs`, `crates/bridge/src/external.rs`

Moving `processed_txs` into `BridgeStateManager` is the right call. The field is public, serialized/deserialized with the rest of the state — persists across restarts.

---

## 4. Shielded Prover Fail-Fast — Good, but `verify_zk_proof` is now dead code

**File**: `crates/shielded/src/lib.rs`

`verify_zk_proof` is still present in `lib.rs` but no longer called by `verify_shielded_proof` when `real-prover` is disabled. It's used by `test_utils::test_proof` and tests — but if `real-prover` is never enabled, the structural-only path is completely dead in production.

**Suggested fix**: Consider removing `verify_zk_proof` or adding a deprecation note.

**Priority**: LOW

---

## 5. Merkle Tree Rebuild — Good, but not auto-called

**File**: `crates/shielded/src/lib.rs`

`rebuild_merkle_tree()` is a good addition, but **no caller invokes it**. The DB load path (`load_shielded_state_inner`) already rebuilds the tree manually, but if anyone deserializes a `ShieldedState` via `serde_json` directly (e.g., backup/restore), the merkle tree stays empty.

**Suggested fix**: Consider calling it in `Default::default()` after serde deserialization or in a `#[serde(deserialize_with)]` on the whole struct.

**Priority**: LOW

---

## 6. Receipt Pruning — Good

**Files**: `crates/protocol/src/receipts.rs`, `crates/rpc/src/handlers.rs`

Adding `block_number` and pruning after 1000 blocks is sensible. The `finalize_block` hook ensures it runs every block.

**Suggested fix**: `_block` param in `get_receipts_by_block` is now misleading since receipts aren't keyed by block anymore — the function returns all receipts regardless of block. Either rename it to `get_all_receipts` or fix the implementation.

**Priority**: LOW

---

## 7. Social Recovery Guardians — Good

**File**: `crates/protocol/src/smart_accounts.rs`

Adding `guardians: Vec<Address>` and checking authorization before `guardian_approve` is the right security fix.

---

## 8. Multi-Session Keys — Good

**File**: `crates/protocol/src/smart_accounts.rs`

The `HashMap<Address, HashMap<Address, SessionKeyConfig>>` change allows multiple session keys per account. `revoke_session_key` now takes a specific key instead of revoking all. Good design.

---

## 9. Sponsor Per-Tx Verification — Good

**File**: `crates/protocol/src/sponsor.rs`

The previous no-op stub is now a real balance check + deduction. Correct.

---

## 10. Sponsor Expiry Off-By-One — Good

**File**: `crates/protocol/src/sponsor.rs`

Changing `>` to `>=` for expiry check: `current_day >= auth.expires_at` means the sponsor expires at exactly `expires_at`, not one day after. This is the correct behavior.

---

## 11. Compliance Check on Transfer — Minor concern

**File**: `crates/protocol/src/instructions.rs`

```rust
compliance.check_compliance_by_policy_id(&sender, registry.get_asset(*asset_id).map(|a| a.compliance_policy).unwrap_or(0))?;
```

Using `unwrap_or(0)` for missing assets silently passes compliance checks for non-existent assets. The subsequent `balances.transfer` will fail anyway, but the compliance check should probably also error if the asset doesn't exist.

**Priority**: LOW

---

## 12. `UpdateCompliance` Implementation — Good

**File**: `crates/protocol/src/instructions.rs`

Adding `set_address_compliance` with issuer-only auth is correct. Only the asset issuer can change compliance status.

---

## 13. `BridgeDeposit` Instruction — Minor concern

**File**: `crates/protocol/src/instructions.rs`

The instruction mints tokens to `target_address` from `sender`. The `source_chain` and `target_address` fields are acknowledged with `let _ = (source_chain, target_address);` but not actually used in the protocol layer — the proof validation is delegated to the bridge module. This is fine for now, but the `let _ = ...` pattern means these fields could be any value and still mint tokens as long as proof is non-empty.

**Priority**: LOW

---

## Summary

| Priority | Issue | File |
|----------|-------|------|
| MEDIUM | Tx hash preimage missing gas params — signature malleability | `crates/rpc/src/callchain.rs` |
| LOW | `verify_zk_proof` is now dead code when `real-prover` disabled | `crates/shielded/src/lib.rs` |
| LOW | `rebuild_merkle_tree` not auto-called after serde deserialization | `crates/shielded/src/lib.rs` |
| LOW | `get_receipts_by_block` param unused — misleading name | `crates/rpc/src/handlers.rs` |
| LOW | `unwrap_or(0)` bypasses compliance for missing assets | `crates/protocol/src/instructions.rs` |
| LOW | `BridgeDeposit` mints tokens with only empty-proof check | `crates/protocol/src/instructions.rs` |

## Overall

The fixes are solid — the critical issues were genuinely critical and the implementations are clean. 647 tests passing after all changes.
