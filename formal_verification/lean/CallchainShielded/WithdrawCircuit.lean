import CallchainShielded.Fr
import CallchainShielded.Poseidon
import CallchainShielded.R1CS
import CallchainShielded.Common

namespace CallchainShielded

set_option maxRecDepth 10000

-- ============================================================================
-- High-Level Witness and Validity
-- ============================================================================

/-- A withdraw witness contains the private data needed to spend a note. -/
structure WithdrawWitness where
  value : Nat
  rcm : Fr
  ivk : Fr
  rho : Fr
  sk : Fr
  merkle_path : MerklePath
  h_value_pos : value > 0
  h_value_range : value < 2 ^ 128

namespace WithdrawWitness

/-- Compute the note commitment: H(value, asset_id, rcm, rho) -/
def commitment (w : WithdrawWitness) (asset_id : Fr) : Fr :=
  noteCommitment w.value asset_id w.rcm w.rho

end WithdrawWitness

/-- High-level validity predicate for a withdraw.

    W1: Nullifier derivation
    W2: Merkle path validity
    W3: Value matches public value
    W4: Spending rights (IVK matches SK derivation) -/
def ValidWithdraw (w : WithdrawWitness)
    (nullifier asset_id value merkle_root : Fr) : Prop :=
  -- W1: Nullifier derivation
  nullifierFromSK w.sk w.rho = nullifier
  -- W2: Merkle path validity
  ∧ verifyMerklePath (w.commitment asset_id) w.merkle_path = merkle_root
  -- W3: Value match
  ∧ w.value = value.val
  -- W4: Spending rights
  ∧ ivkFromSK w.sk = w.ivk

-- ============================================================================
-- R1CS Model
-- ============================================================================

/- Witness layout for WithdrawCircuit (10 elements):
   [1,
    nullifier, asset_id, value, merkle_root,  -- public (indices 1-4)
    rcm, ivk, rho, sk, inv_value]             -- private 0-4

   Constraints:
   C2: value * inv_value = 1

   Semantic constraints:
   W1-W4 (see ValidWithdraw above)
   Plus range check: value < 2^128
-/

/-- Flat encoding of a withdraw witness into an R1CS assignment. -/
def encodeWithdrawWitness (nullifier asset_id value merkle_root : Fr)
    (rcm ivk rho sk inv_value : Fr) : Assignment 4 5 :=
  ⟨[1, nullifier, asset_id, value, merkle_root,
    rcm, ivk, rho, sk, inv_value],
   by simp⟩

/-- Encode a withdraw witness into an R1CS assignment.
    Computes inv_value = Fr.inv(value) to satisfy C2. -/
def encodeWithdraw (w : WithdrawWitness) (nullifier asset_id value merkle_root : Fr) :
    Assignment 4 5 :=
  let v_fr : Fr := w.value
  let inv_val := Fr.inv v_fr
  encodeWithdrawWitness nullifier asset_id value merkle_root
    w.rcm w.ivk w.rho w.sk inv_val

/-- C2 constraint: value * inv_value = 1
    Witness indices: value = 3, inv_value = 9, constant 1 = 0 -/
def c2WithdrawConstraint : R1CSConstraint :=
  { a := [0, 0, 0, 1, 0, 0, 0, 0, 0, 0]
  , b := [0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
  , c := [1, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  }

/-- Semantic constraints for WithdrawCircuit.

    The value is a PUBLIC input (index 2), so we reference it via
    `a.public 2`. The semantic constraints ensure the private witnesses
    are consistent with the public inputs. -/
def withdrawSemantic (nullifier asset_id value merkle_root : Fr)
    (merkle_path : MerklePath) (a : Assignment 4 5) : Prop :=
  let val_fr := a.public 2 (show 2 < 4 by decide)
  let val_nat := val_fr.val
  let rcm := a.private 0 (show 0 < 5 by decide)
  let ivk := a.private 1 (show 1 < 5 by decide)
  let rho := a.private 2 (show 2 < 5 by decide)
  let sk := a.private 3 (show 3 < 5 by decide)
  -- W1: Nullifier derivation
  nullifierFromSK sk rho = nullifier
  -- W2: Merkle path validity
  ∧ verifyMerklePath (noteCommitment val_nat asset_id rcm rho) merkle_path = merkle_root
  -- W3: Value match (public value = witness value)
  ∧ val_nat = value.val
  -- W4: Spending rights
  ∧ ivkFromSK sk = ivk
  -- Range check
  ∧ val_nat < 2 ^ 128

/-- The WithdrawCircuit R1CS instance. -/
def WithdrawCircuit (nullifier asset_id value merkle_root : Fr)
    (merkle_path : MerklePath) : R1CS 4 5 :=
  { constraints := [c2WithdrawConstraint]
  , semantic := withdrawSemantic nullifier asset_id value merkle_root merkle_path
  }

-- ============================================================================
-- Accessor lemmas for encodeWithdraw
-- ============================================================================

theorem encodeWithdraw_rcm (w : WithdrawWitness) (nullifier asset_id value merkle_root : Fr) :
    (encodeWithdraw w nullifier asset_id value merkle_root).private 0 (show 0 < 5 by decide) = w.rcm := rfl

theorem encodeWithdraw_ivk (w : WithdrawWitness) (nullifier asset_id value merkle_root : Fr) :
    (encodeWithdraw w nullifier asset_id value merkle_root).private 1 (show 1 < 5 by decide) = w.ivk := rfl

theorem encodeWithdraw_rho (w : WithdrawWitness) (nullifier asset_id value merkle_root : Fr) :
    (encodeWithdraw w nullifier asset_id value merkle_root).private 2 (show 2 < 5 by decide) = w.rho := rfl

theorem encodeWithdraw_sk (w : WithdrawWitness) (nullifier asset_id value merkle_root : Fr) :
    (encodeWithdraw w nullifier asset_id value merkle_root).private 3 (show 3 < 5 by decide) = w.sk := rfl

-- ============================================================================
-- evalLC selector lemmas
-- ============================================================================

/-- Selector at index 3 (value) in 10-element assignment -/
theorem evalLC_sel_3_10 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 : Fr) :
    evalLC [0, 0, 0, 1, 0, 0, 0, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9]
    = Fr.fromNat w3.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 9 (inv_value) in 10-element assignment -/
theorem evalLC_sel_9_10 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 : Fr) :
    evalLC [0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9]
    = Fr.fromNat w9.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 0 (constant 1) in 10-element assignment -/
theorem evalLC_sel_0_10 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 : Fr) :
    evalLC [1, 0, 0, 0, 0, 0, 0, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9]
    = Fr.fromNat w0.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

-- ============================================================================
-- Helper: list decomposition for 10 elements
-- ============================================================================

theorem list_decompose_10 (vals : List Fr) (h : vals.length = 10) :
    ∃ w0 w1 w2 w3 w4 w5 w6 w7 w8 w9,
      vals = [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9] := by
  match vals with
  | [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9] => exact ⟨w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, rfl⟩

-- ============================================================================
-- Completeness Theorem
-- ============================================================================

/-- **Completeness**: Every valid withdraw witness satisfies the R1CS constraints. -/
theorem WithdrawCircuit.completeness :
  ∀ (w : WithdrawWitness) (nullifier asset_id value merkle_root : Fr),
  ValidWithdraw w nullifier asset_id value merkle_root →
  (WithdrawCircuit nullifier asset_id value merkle_root w.merkle_path).satisfied
    (encodeWithdraw w nullifier asset_id value merkle_root) := by
  intro w nullifier asset_id value merkle_root h_valid
  rcases h_valid with ⟨h_n1, h_m1, h_vm, h_s1⟩
  constructor
  · -- Prove C2 constraint: value * inv_value = 1
    intro c hc
    have hc2 : c = c2WithdrawConstraint := by
      simp [WithdrawCircuit] at hc
      exact hc
    rw [hc2]
    simp [constraintSatisfied, c2WithdrawConstraint, encodeWithdraw, encodeWithdrawWitness]
    let v_fr : Fr := w.value
    let inv_val := Fr.inv v_fr
    rw [evalLC_sel_3_10 1 nullifier asset_id value merkle_root w.rcm w.ivk w.rho w.sk inv_val]
    rw [evalLC_sel_9_10 1 nullifier asset_id value merkle_root w.rcm w.ivk w.rho w.sk inv_val]
    rw [evalLC_sel_0_10 1 nullifier asset_id value merkle_root w.rcm w.ivk w.rho w.sk inv_val]
    have h_pos : w.value > 0 := w.h_value_pos
    have h_range : w.value < 2 ^ 128 := w.h_value_range
    have h_lt_p : w.value < BN254_P := nat_lt_128_to_lt_p h_range
    have h_nz : (w.value : Fr).val % BN254_P ≠ 0 := nat_nonzero_to_fr h_pos h_lt_p
    have h_val_eq : value = v_fr := by
      have h1 : v_fr = Fr.fromNat w.value := rfl
      have h2 : value.val < BN254_P := by
        have h3 : value.val = w.value := by rw [h_vm]
        rw [h3]
        exact h_lt_p
      have h3 : value = Fr.fromNat value.val := (Fr.fromNat_eq_of_lt value h2).symm
      rw [h1, h3]
      rw [h_vm]
    rw [h_val_eq]
    have h_mul : Fr.fromNat v_fr.val * Fr.fromNat (Fr.inv v_fr).val = Fr.fromNat (v_fr.val * (Fr.inv v_fr).val) := by
      rw [← Fr.mul_fromNat v_fr.val (Fr.inv v_fr).val]
    have h_inv : Fr.fromNat (v_fr.val * (Fr.inv v_fr).val) = 1 := by
      have h : Fr.fromNat (v_fr.val * (Fr.inv v_fr).val) = v_fr * Fr.inv v_fr := rfl
      rw [h]
      exact Fr.inv_mul v_fr h_nz
    rw [h_mul, h_inv]
    rfl
  · -- Prove semantic constraints (W1-W4 + range)
    dsimp only [WithdrawCircuit, withdrawSemantic]
    have h_sk : (encodeWithdraw w nullifier asset_id value merkle_root).private 3 (show 3 < 5 by decide) = w.sk := rfl
    have h_rho : (encodeWithdraw w nullifier asset_id value merkle_root).private 2 (show 2 < 5 by decide) = w.rho := rfl
    have h_rcm : (encodeWithdraw w nullifier asset_id value merkle_root).private 0 (show 0 < 5 by decide) = w.rcm := rfl
    have h_ivk : (encodeWithdraw w nullifier asset_id value merkle_root).private 1 (show 1 < 5 by decide) = w.ivk := rfl
    have h_val : ((encodeWithdraw w nullifier asset_id value merkle_root).public 2 (show 2 < 4 by decide)).val = w.value := by
      simp [encodeWithdraw, encodeWithdrawWitness, Assignment.public]
      rw [h_vm]
    constructor
    · rw [h_sk, h_rho]; exact h_n1
    constructor
    · rw [h_val, h_rcm, h_rho]; exact h_m1
    constructor
    · rw [h_val]; exact h_vm
    constructor
    · rw [h_sk, h_ivk]; exact h_s1
    · rw [h_val]; exact w.h_value_range

-- ============================================================================
-- Soundness Theorem
-- ============================================================================

/-- **Soundness**: Every R1CS-satisfying assignment corresponds to a valid witness. -/
theorem WithdrawCircuit.soundness :
  ∀ (assignment : Assignment 4 5) (nullifier asset_id value merkle_root : Fr)
    (merkle_path : MerklePath),
  (WithdrawCircuit nullifier asset_id value merkle_root merkle_path).satisfied assignment →
  assignment.one = 1 →
  assignment.public 0 (by decide) = nullifier →
  assignment.public 1 (by decide) = asset_id →
  assignment.public 2 (by decide) = value →
  assignment.public 3 (by decide) = merkle_root →
  ∃ (w : WithdrawWitness),
    ValidWithdraw w nullifier asset_id value merkle_root ∧
    encodeWithdraw w nullifier asset_id value merkle_root = assignment := by
  intro assignment nullifier asset_id value merkle_root merkle_path
    h_sat h_one h_pub0 h_pub1 h_pub2 h_pub3

  -- Decode assignment components
  let val_fr := assignment.public 2 (show 2 < 4 by decide)
  let val_nat := val_fr.val
  let rcm := assignment.private 0 (show 0 < 5 by decide)
  let ivk := assignment.private 1 (show 1 < 5 by decide)
  let rho := assignment.private 2 (show 2 < 5 by decide)
  let sk := assignment.private 3 (show 3 < 5 by decide)
  let inv_val := assignment.private 4 (show 4 < 5 by decide)

  rcases h_sat with ⟨h_c2_sat, h_semantic⟩

  -- Decompose assignment.values into 10 elements
  have h_len : assignment.values.length = 10 := by rw [assignment.h_size]
  rcases list_decompose_10 assignment.values h_len with ⟨w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, hw_eq⟩

  -- Prove evalLC results
  have h_eval_a : evalLC c2WithdrawConstraint.a assignment.values = Fr.fromNat val_fr.val := by
    rw [hw_eq]
    simp [c2WithdrawConstraint]
    rw [evalLC_sel_3_10 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9]
    have h3 : val_fr = w3 := by
      simp [val_fr, Assignment.public]
      rw [hw_eq]
      rfl
    rw [h3]

  have h_eval_b : evalLC c2WithdrawConstraint.b assignment.values = Fr.fromNat inv_val.val := by
    rw [hw_eq]
    simp [c2WithdrawConstraint]
    rw [evalLC_sel_9_10 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9]
    have h9 : inv_val = w9 := by
      simp [inv_val, Assignment.private]
      rw [hw_eq]
      rfl
    rw [h9]

  have h_eval_c : evalLC c2WithdrawConstraint.c assignment.values = Fr.fromNat assignment.one.val := by
    rw [hw_eq]
    simp [c2WithdrawConstraint]
    rw [evalLC_sel_0_10 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9]
    have h0 : assignment.one = w0 := by
      simp [Assignment.one]
      rw [hw_eq]
      rfl
    rw [h0]

  -- Extract C2 constraint satisfaction
  have h_c2 : constraintSatisfied c2WithdrawConstraint assignment.values := by
    have h_mem : c2WithdrawConstraint ∈ (WithdrawCircuit nullifier asset_id value merkle_root merkle_path).constraints := by simp [WithdrawCircuit]
    exact h_c2_sat c2WithdrawConstraint h_mem

  have h_c2_eq : Fr.fromNat val_fr.val * Fr.fromNat inv_val.val = Fr.fromNat assignment.one.val := by
    simp only [constraintSatisfied] at h_c2
    rw [h_eval_a, h_eval_b, h_eval_c] at h_c2
    exact h_c2

  have h_rhs : Fr.fromNat assignment.one.val = 1 := by
    rw [h_one]
    rfl

  rw [h_rhs] at h_c2_eq

  -- Convert to Fr product = 1
  have h_val_mul : val_fr * inv_val = 1 := by
    have h : Fr.fromNat val_fr.val * Fr.fromNat inv_val.val = Fr.fromNat (val_fr.val * inv_val.val) := by
      rw [← Fr.mul_fromNat val_fr.val inv_val.val]
    rw [h] at h_c2_eq
    have h_val : val_fr * inv_val = Fr.fromNat (val_fr.val * inv_val.val) := rfl
    rw [h_val]
    exact h_c2_eq

  -- Derive value > 0 from C2
  have h_nz_mod : val_fr.val % BN254_P ≠ 0 := Fr.nonzero_of_mul_eq_one val_fr inv_val h_val_mul

  have h_pos : val_nat > 0 := Nat.pos_of_mod_ne_zero h_nz_mod

  -- Extract semantic constraints
  simp [WithdrawCircuit, withdrawSemantic] at h_semantic
  rcases h_semantic with ⟨h_sem_n1, h_sem_m1, h_sem_vm, h_sem_s1, h_sem_range⟩

  have h_range : val_nat < 2 ^ 128 := h_sem_range
  have h_lt_p : val_nat < BN254_P := nat_lt_128_to_lt_p h_range

  -- val_fr = Fr.fromNat val_nat (since val_nat < BN254_P)
  have h_val_eq : Fr.fromNat val_nat = val_fr := by
    have h1 : val_nat = val_fr.val := rfl
    rw [h1]
    exact Fr.fromNat_eq_of_lt val_fr h_lt_p

  -- inv_val = Fr.inv (Fr.fromNat val_nat)
  have h_inv_eq : inv_val = Fr.inv (Fr.fromNat val_nat) := by
    have h : Fr.inv (Fr.fromNat val_nat) = Fr.inv val_fr := by
      rw [h_val_eq]
    rw [h]
    exact Fr.inv_unique val_fr inv_val h_nz_mod h_val_mul

  -- Witness component equalities
  have h_w0 : w0 = 1 := by
    have h : assignment.one = w0 := by
      simp [Assignment.one]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_one

  have h_w1 : w1 = nullifier := by
    have h : assignment.public 0 (by decide) = w1 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub0

  have h_w2 : w2 = asset_id := by
    have h : assignment.public 1 (by decide) = w2 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub1

  have h_w3 : w3 = value := by
    have h : assignment.public 2 (by decide) = w3 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub2

  have h_w4 : w4 = merkle_root := by
    have h : assignment.public 3 (by decide) = w4 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub3

  have h_w5 : w5 = rcm := by
    have h : rcm = w5 := by
      simp [rcm, Assignment.private]
      rw [hw_eq]
      rfl
    exact h.symm

  have h_w6 : w6 = ivk := by
    have h : ivk = w6 := by
      simp [ivk, Assignment.private]
      rw [hw_eq]
      rfl
    exact h.symm

  have h_w7 : w7 = rho := by
    have h : rho = w7 := by
      simp [rho, Assignment.private]
      rw [hw_eq]
      rfl
    exact h.symm

  have h_w8 : w8 = sk := by
    have h : sk = w8 := by
      simp [sk, Assignment.private]
      rw [hw_eq]
      rfl
    exact h.symm

  have h_w9 : w9 = Fr.inv (Fr.fromNat val_nat) := by
    have h : inv_val = w9 := by
      simp [inv_val, Assignment.private]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_inv_eq

  -- Construct witness
  let w : WithdrawWitness := ⟨val_nat, rcm, ivk, rho, sk, merkle_path, h_pos, h_range⟩

  refine ⟨w, ⟨?_, ?_, ?_, ?_⟩, ?_⟩
  · -- W1: nullifier
    exact h_sem_n1
  · -- W2: Merkle path
    exact h_sem_m1
  · -- W3: value match
    exact h_sem_vm
  · -- W4: spending rights
    exact h_sem_s1
  · -- Encoding equality
    have h_list :
        [1, nullifier, asset_id, value, merkle_root,
         rcm, ivk, rho, sk, Fr.inv (Fr.fromNat val_nat)]
        = [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9] := by
      rw [h_w0, h_w1, h_w2, h_w3, h_w4, h_w5, h_w6, h_w7, h_w8, h_w9]
    generalize h_enc : encodeWithdraw w nullifier asset_id value merkle_root = enc
    cases enc with | mk enc_vals enc_h =>
    cases assignment with | mk vals h_size =>
    simp at hw_eq
    have h_vals : enc_vals = vals := by
      have h1 : (encodeWithdraw w nullifier asset_id value merkle_root).values = enc_vals := by
        rw [h_enc]
      have h2 : (encodeWithdraw w nullifier asset_id value merkle_root).values = vals := by
        simp [encodeWithdraw, encodeWithdrawWitness]
        rw [hw_eq]
        exact h_list
      rw [←h1]
      exact h2
    simp [h_vals]

end CallchainShielded
