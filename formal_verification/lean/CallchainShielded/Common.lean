import CallchainShielded.Fr
import CallchainShielded.Poseidon
import CallchainShielded.R1CS

namespace CallchainShielded

-- ============================================================================
-- Generalized value bounds (reusable across all circuits)
-- ============================================================================

/-- If n > 0 and n < BN254_P, then n as Fr is non-zero. -/
theorem nat_nonzero_to_fr {n : Nat} (h_pos : n > 0) (h_lt : n < BN254_P) :
    (n : Fr).val % BN254_P ≠ 0 := by
  have h : (n : Fr).val = n := by
    simp [Fr.fromNat]
    rw [Nat.mod_eq_of_lt]
    exact h_lt
  rw [h]
  intro h_contra
  have h_zero : n = 0 := by
    have h_mod : n % BN254_P = n := Nat.mod_eq_of_lt h_lt
    rw [←h_mod]
    exact h_contra
  rw [h_zero] at h_pos
  exact Nat.lt_irrefl 0 h_pos

/-- If n < 2^128, then n < BN254_P (since 2^128 << BN254_P). -/
theorem nat_lt_128_to_lt_p {n : Nat} (h : n < 2 ^ 128) : n < BN254_P := by
  have h2 : 2 ^ 128 < BN254_P := by decide
  exact Nat.lt_trans h h2

-- ============================================================================
-- evalLC helper lemmas
-- ============================================================================

@[simp]
theorem evalLC_nil_left (ws : List Fr) : evalLC [] ws = 0 := by
  rw [evalLC]

@[simp]
theorem evalLC_nil_right (cs : List Fr) : evalLC cs [] = 0 := by
  cases cs <;> simp [evalLC]

theorem evalLC_cons (c : Fr) (cs : List Fr) (w : Fr) (ws : List Fr) :
    evalLC (c :: cs) (w :: ws) = c * w + evalLC cs ws := by
  rw [evalLC]

/-- evalLC with zero coefficient -/
theorem evalLC_zero (cs : List Fr) (w : Fr) (ws : List Fr) :
    evalLC (0 :: cs) (w :: ws) = Fr.fromNat (evalLC cs ws).val := by
  rw [evalLC_cons]
  simp [Fr.zero_mul, Fr.add_zero']

/-- evalLC with one coefficient -/
theorem evalLC_one (cs : List Fr) (w : Fr) (ws : List Fr) :
    evalLC (1 :: cs) (w :: ws) = Fr.fromNat w.val + evalLC cs ws := by
  rw [evalLC_cons]
  simp [Fr.one_mul']

-- ============================================================================
-- List extensionality (for soundness proofs)
-- ============================================================================

/-- Two lists are equal if they have the same length and equal elements. -/
theorem list_eq_of_elements {α : Type} [Inhabited α] (l1 : List α) :
    ∀ (l2 : List α), l1.length = l2.length →
    (∀ i, i < l1.length → l1[i]! = l2[i]!) → l1 = l2 := by
  induction l1 with
  | nil =>
    intro l2 h_len h
    cases l2 with
    | nil => rfl
    | cons y ys => simp at h_len
  | cons x xs ih =>
    intro l2 h_len h
    cases l2 with
    | nil => simp at h_len
    | cons y ys =>
      have h0 : x = y := by
        specialize h 0 (by simp)
        simp at h
        exact h
      have h_rest : xs = ys := by
        have h_len' : xs.length = ys.length := by simp at h_len; exact h_len
        have h' : ∀ i, i < xs.length → xs[i]! = ys[i]! := by
          intro i hi
          specialize h (i + 1) (by simp; exact hi)
          simp at h
          exact h
        exact ih ys h_len' h'
      rw [h0, h_rest]

-- ============================================================================
-- Note Commitment
-- ============================================================================

/-- Compute a note commitment: H(value, asset_id, rcm, rho) -/
def noteCommitment (value : Nat) (asset_id rcm rho : Fr) : Fr :=
  let v_fr : Fr := value
  poseidonHash [v_fr, asset_id, rcm, rho]
    (by simp)

-- ============================================================================
-- Nullifier Derivation Chain
-- ============================================================================

/-- Derive IVK from spending key: H_tag("call/shielded/ivk", [sk]) -/
def ivkFromSK (sk : Fr) : Fr :=
  poseidonHashTagged "call/shielded/ivk" [sk]
    (by simp)

/-- Derive FVK from IVK: H_tag("fvk_from_ivk", [ivk]) -/
def fvkFromIVK (ivk : Fr) : Fr :=
  poseidonHashTagged "fvk_from_ivk" [ivk]
    (by simp)

/-- Derive nullifier from FVK and rho: H([fvk, rho]) -/
def nullifierFromFVK (fvk rho : Fr) : Fr :=
  poseidonHash [fvk, rho]
    (by simp)

/-- Full nullifier derivation from spending key and rho -/
def nullifierFromSK (sk rho : Fr) : Fr :=
  let ivk := ivkFromSK sk
  let fvk := fvkFromIVK ivk
  nullifierFromFVK fvk rho

-- ============================================================================
-- Merkle Tree Path Verification
-- ============================================================================

/-- A Merkle path is a list of (sibling_hash, is_right_child) pairs.
    is_right = true means the current node is the right child. -/
def MerklePath := List (Fr × Bool)

/-- Verify a Merkle path: fold from leaf to root using Poseidon hashing.
    If is_right is true, hash(sibling, current); otherwise hash(current, sibling). -/
def verifyMerklePath (leaf : Fr) (path : MerklePath) : Fr :=
  path.foldl (fun current sibling_pair =>
    let sibling := sibling_pair.1
    let isRight := sibling_pair.2
    if isRight then
      poseidonHash [sibling, current]
        (by simp)
    else
      poseidonHash [current, sibling]
        (by simp)
  ) leaf

-- ============================================================================
-- Non-zero derivation from multiplicative inverse
-- ============================================================================

/-- If a * b = 1 in Fr, then a.val % p ≠ 0 -/
theorem Fr.nonzero_of_mul_eq_one (a b : Fr) (h : a * b = 1) : a.val % BN254_P ≠ 0 := by
  intro h_zero
  have h1 : (a.val * b.val) % BN254_P = 0 := by
    rw [Nat.mul_mod a.val b.val BN254_P (by decide)]
    rw [h_zero]
    simp
  have h2 : Fr.fromNat (a.val * b.val) = Fr.fromNat 0 := by
    rw [Fr.fromNat_eq_iff]
    exact h1
  have h3 : a * b = 0 := by
    have h_val : a * b = Fr.fromNat (a.val * b.val) := rfl
    rw [h_val, h2]
    rfl
  rw [h3] at h
  have h4 : (0 : Fr) = 1 := h
  have h5 : (0 : Fr).val = (1 : Fr).val := by rw [h4]
  simp at h5

/-- If n % p ≠ 0, then n > 0 (since 0 % p = 0) -/
theorem Nat.pos_of_mod_ne_zero {n : Nat} (h : n % BN254_P ≠ 0) : n > 0 := by
  have h_nz : n ≠ 0 := by
    intro h_zero
    rw [h_zero] at h
    simp at h
  exact Nat.pos_of_ne_zero h_nz

end CallchainShielded
