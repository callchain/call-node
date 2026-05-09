import CallchainShielded.Fr
import CallchainShielded.Poseidon
import CallchainShielded.R1CS

namespace CallchainShielded

-- ============================================================================
-- High-Level Witness and Validity
-- ============================================================================

/-- A deposit witness contains the private data needed to create a deposit proof -/
structure DepositWitness where
  value : Nat
  rcm : Fr
  ivk : Fr
  rho : Fr
  h_value_pos : value > 0
  h_value_range : value < 2 ^ 128

namespace DepositWitness

/-- Compute the note commitment: H(value, asset_id, rcm, rho) -/
def commitment (w : DepositWitness) (asset_id : Fr) : Fr :=
  let v_fr : Fr := w.value
  poseidonHash [v_fr, asset_id, w.rcm, w.rho]
    (by simp)

/-- Check RCM determinism: H_tag("rcm", ivk, value, asset_id, rho) = rcm -/
def rcmValid (w : DepositWitness) (asset_id : Fr) : Prop :=
  let v_fr : Fr := w.value
  poseidonHashTagged "rcm" [w.ivk, v_fr, asset_id, w.rho]
    (by simp)
    = w.rcm

end DepositWitness

/-- High-level validity predicate for a deposit -/
def ValidDeposit (w : DepositWitness) (commitment asset_id : Fr) : Prop :=
  w.commitment asset_id = commitment ∧ w.rcmValid asset_id

-- ============================================================================
-- R1CS Model
-- ============================================================================

/- Witness layout for DepositCircuit:
   [1, commitment, asset_id, value, rcm, ivk, rho, inv_val]
   public inputs: commitment (index 0), asset_id (index 1)
   private witnesses: value (index 0), rcm (index 1), ivk (index 2), rho (index 3), inv_val (index 4)

   Constraints:
   C1: poseidon_hash([value, asset_id, rcm, rho]) = commitment   (TODO: expand Poseidon gadget)
   C2: value * inv_val = 1                                         (non-zero via inverse witness)
   C3: value < 2^128                                               (TODO: bit decomposition)
   C4: poseidon_hash_tagged("rcm", [ivk, value, asset_id, rho]) = rcm  (TODO: expand Poseidon gadget)
-/

/-- C2 constraint: value * inv_val = 1
    Witness indices: value = 3, inv_val = 7, constant 1 = 0 -/
def c2Constraint : R1CSConstraint :=
  { a := [0, 0, 0, 1, 0, 0, 0, 0]   -- selects value (index 3)
  , b := [0, 0, 0, 0, 0, 0, 0, 1]   -- selects inv_val (index 7)
  , c := [1, 0, 0, 0, 0, 0, 0, 0]   -- selects constant 1 (index 0)
  }

/-- Semantic constraints for DepositCircuit:
    C1: poseidon_hash([value, asset_id, rcm, rho]) = commitment
    C3: value < 2^128
    C4: poseidon_hash_tagged("rcm", [ivk, value, asset_id, rho]) = rcm -/
def depositSemantic (commitment asset_id : Fr) (a : Assignment 2 5) : Prop :=
  let value_fr := a.private 0 (by decide)
  let value_nat := value_fr.val
  let rcm := a.private 1 (by decide)
  let ivk := a.private 2 (by decide)
  let rho := a.private 3 (by decide)
  poseidonHash [value_fr, asset_id, rcm, rho]
      (by simp)
    = commitment
  ∧ value_nat < 2 ^ 128
  ∧ poseidonHashTagged "rcm" [ivk, value_fr, asset_id, rho]
      (by simp)
    = rcm

/-- The DepositCircuit R1CS instance.
    C2 is a native R1CS constraint; C1, C3, C4 are semantic constraints
    that would expand into hundreds of R1CS gates in a full implementation. -/
def DepositCircuit (commitment asset_id : Fr) : R1CS 2 5 :=
  { constraints := [c2Constraint]
  , semantic := depositSemantic commitment asset_id
  }

/-- Encode a deposit witness as an R1CS assignment.
    Computes inv_val = Fr.inv(value) to satisfy C2. -/
def encodeDeposit (w : DepositWitness) (commitment asset_id : Fr) :
    Assignment 2 5 :=
  let v_fr : Fr := w.value
  let inv_val := Fr.inv v_fr
  encodeDepositWitness commitment asset_id v_fr w.rcm w.ivk w.rho inv_val

-- ============================================================================
-- Helper lemmas
-- ============================================================================

/-- If value > 0 and value < BN254_P, then value as Fr is non-zero. -/
theorem value_nonzero {w : DepositWitness} :
  w.value > 0 → w.value < BN254_P → (w.value : Fr).val % BN254_P ≠ 0 := by
  intro h_pos h_lt
  have h : (w.value : Fr).val = w.value := by
    simp [Fr.fromNat]
    rw [Nat.mod_eq_of_lt]
    exact h_lt
  rw [h]
  intro h_contra
  have h_zero : w.value = 0 := by
    have h_mod : w.value % BN254_P = w.value := Nat.mod_eq_of_lt h_lt
    rw [←h_mod]
    exact h_contra
  rw [h_zero] at h_pos
  exact Nat.lt_irrefl 0 h_pos

/-- If value < 2^128, then value < BN254_P (since 2^128 << BN254_P). -/
theorem value_lt_p {w : DepositWitness} :
  w.value < 2 ^ 128 → w.value < BN254_P := by
  intro h
  have h2 : 2 ^ 128 < BN254_P := by decide
  exact Nat.lt_trans h h2

-- ============================================================================
-- evalLC computation lemmas for C2 constraint
-- ============================================================================

theorem evalLC_c2_a (w0 w1 w2 w3 w4 w5 w6 w7 : Fr) :
  evalLC [0, 0, 0, 1, 0, 0, 0, 0] [w0, w1, w2, w3, w4, w5, w6, w7] = Fr.fromNat w3.val := by
  repeat rw [evalLC]
  have h0 : (0 : Fr) * w7 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h0]
  have h1 : (0 : Fr) * w6 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h1]
  have h2 : (0 : Fr) * w5 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h2]
  have h3 : (0 : Fr) * w4 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h3]
  have h4 : (1 : Fr) * w3 + (0 : Fr) = Fr.fromNat w3.val := by
    simp only [Fr.one_mul', Fr.add_zero', Fr.fromNat_val]
  rw [h4]
  have h5 : (0 : Fr) * w2 + Fr.fromNat w3.val = Fr.fromNat w3.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h5]
  have h6 : (0 : Fr) * w1 + Fr.fromNat w3.val = Fr.fromNat w3.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h6]
  have h7 : (0 : Fr) * w0 + Fr.fromNat w3.val = Fr.fromNat w3.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h7]

theorem evalLC_c2_b (w0 w1 w2 w3 w4 w5 w6 w7 : Fr) :
  evalLC [0, 0, 0, 0, 0, 0, 0, 1] [w0, w1, w2, w3, w4, w5, w6, w7] = Fr.fromNat w7.val := by
  repeat rw [evalLC]
  have h0 : (1 : Fr) * w7 + (0 : Fr) = Fr.fromNat w7.val := by
    simp only [Fr.one_mul', Fr.add_zero', Fr.fromNat_val]
  rw [h0]
  have h1 : (0 : Fr) * w6 + Fr.fromNat w7.val = Fr.fromNat w7.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h1]
  have h2 : (0 : Fr) * w5 + Fr.fromNat w7.val = Fr.fromNat w7.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h2]
  have h3 : (0 : Fr) * w4 + Fr.fromNat w7.val = Fr.fromNat w7.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h3]
  have h4 : (0 : Fr) * w3 + Fr.fromNat w7.val = Fr.fromNat w7.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h4]
  have h5 : (0 : Fr) * w2 + Fr.fromNat w7.val = Fr.fromNat w7.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h5]
  have h6 : (0 : Fr) * w1 + Fr.fromNat w7.val = Fr.fromNat w7.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h6]
  have h7 : (0 : Fr) * w0 + Fr.fromNat w7.val = Fr.fromNat w7.val := by
    simp only [Fr.zero_mul, Fr.zero_add', Fr.fromNat_val]
  rw [h7]

theorem evalLC_c2_c (w0 w1 w2 w3 w4 w5 w6 w7 : Fr) :
  evalLC [1, 0, 0, 0, 0, 0, 0, 0] [w0, w1, w2, w3, w4, w5, w6, w7] = Fr.fromNat w0.val := by
  repeat rw [evalLC]
  have h0 : (0 : Fr) * w7 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h0]
  have h1 : (0 : Fr) * w6 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h1]
  have h2 : (0 : Fr) * w5 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h2]
  have h3 : (0 : Fr) * w4 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h3]
  have h4 : (0 : Fr) * w3 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h4]
  have h5 : (0 : Fr) * w2 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h5]
  have h6 : (0 : Fr) * w1 + (0 : Fr) = (0 : Fr) := by
    simp only [Fr.zero_mul, Fr.add_zero', Fr.val_zero, Fr.fromNat_zero]
  rw [h6]
  have h7 : (1 : Fr) * w0 + (0 : Fr) = Fr.fromNat w0.val := by
    simp only [Fr.one_mul', Fr.add_zero', Fr.fromNat_val]
  rw [h7]

-- ============================================================================
-- Completeness Theorem
-- ============================================================================

/-- **Completeness**: Every valid deposit witness satisfies the R1CS constraints.

    If a witness satisfies the high-level validity predicate, then the R1CS
    constraints generated by DepositCircuit are satisfied. -/
theorem DepositCircuit.completeness :
  ∀ (w : DepositWitness) (commitment asset_id : Fr),
  ValidDeposit w commitment asset_id →
  (DepositCircuit commitment asset_id).satisfied (encodeDeposit w commitment asset_id) := by
  intro w commitment asset_id h_valid
  -- Unpack validity into C1 and C4
  rcases h_valid with ⟨h_commit, h_rcm⟩
  constructor
  · -- Prove all low-level R1CS constraints are satisfied (just C2)
    intro c hc
    have hc2 : c = c2Constraint := by
      simp [DepositCircuit] at hc
      exact hc
    rw [hc2]
    simp [constraintSatisfied, c2Constraint, encodeDeposit, encodeDepositWitness]
    let v_fr : Fr := w.value
    let inv_val := Fr.inv v_fr
    rw [evalLC_c2_a 1 commitment asset_id v_fr w.rcm w.ivk w.rho inv_val]
    rw [evalLC_c2_b 1 commitment asset_id v_fr w.rcm w.ivk w.rho inv_val]
    rw [evalLC_c2_c 1 commitment asset_id v_fr w.rcm w.ivk w.rho inv_val]
    have h_pos : w.value > 0 := w.h_value_pos
    have h_range : w.value < 2 ^ 128 := w.h_value_range
    have h_lt_p : w.value < BN254_P := value_lt_p h_range
    have h_nz : (w.value : Fr).val % BN254_P ≠ 0 := value_nonzero h_pos h_lt_p
    have h_mul : Fr.fromNat v_fr.val * Fr.fromNat (Fr.inv v_fr).val = Fr.fromNat (v_fr.val * (Fr.inv v_fr).val) := by
      rw [← Fr.mul_fromNat v_fr.val (Fr.inv v_fr).val]
    have h_inv : Fr.fromNat (v_fr.val * (Fr.inv v_fr).val) = 1 := by
      have h : Fr.fromNat (v_fr.val * (Fr.inv v_fr).val) = v_fr * Fr.inv v_fr := rfl
      rw [h]
      exact Fr.inv_mul v_fr h_nz
    rw [h_mul, h_inv]
    rfl
  · -- Prove semantic constraints (C1, C3, C4)
    have h_sem : depositSemantic commitment asset_id (encodeDeposit w commitment asset_id) := by
      simp [depositSemantic, encodeDeposit, encodeDepositWitness, Assignment.private]
      constructor
      · exact h_commit
      constructor
      · -- C3: value < 2^128
        have h_val : (Fr.fromNat w.value).val = w.value := by
          simp [Fr.fromNat]
          rw [Nat.mod_eq_of_lt]
          exact value_lt_p w.h_value_range
        rw [h_val]
        exact w.h_value_range
      · exact h_rcm
    exact h_sem

-- ============================================================================
-- Soundness Theorem
-- ============================================================================

/-- **Soundness**: Every R1CS-satisfying assignment corresponds to a valid witness.

    If an assignment satisfies the DepositCircuit R1CS constraints, then there
    exists a valid deposit witness that produces those public inputs.

    The public input equality assumptions reflect the fact that R1CS verifiers
    check public inputs externally; the constraint system itself does not
    enforce them. -/
theorem DepositCircuit.soundness :
  ∀ (assignment : Assignment 2 5) (commitment asset_id : Fr),
  (DepositCircuit commitment asset_id).satisfied assignment →
  assignment.one = 1 →
  assignment.public 0 (by decide) = commitment →
  assignment.public 1 (by decide) = asset_id →
  ∃ (w : DepositWitness),
    ValidDeposit w commitment asset_id ∧
    encodeDeposit w commitment asset_id = assignment := by
  intro assignment commitment asset_id h_sat h_one h_pub0 h_pub1
  -- Decode the assignment into witness components
  let value_fr := assignment.private 0 (by decide)
  let value_nat := value_fr.val
  let rcm := assignment.private 1 (by decide)
  let ivk := assignment.private 2 (by decide)
  let rho := assignment.private 3 (by decide)
  let inv_val := assignment.private 4 (by decide)

  -- Extract low-level constraints and semantic assertions from satisfaction
  rcases h_sat with ⟨h_c2_sat, h_semantic⟩

  -- Helper: decompose a list of length 8
  have h_len : assignment.values.length = 8 := by rw [assignment.h_size]

  have h_exists : ∃ w0 w1 w2 w3 w4 w5 w6 w7,
      assignment.values = [w0, w1, w2, w3, w4, w5, w6, w7] := by
    generalize h_vals : assignment.values = vals
    have h8 : vals.length = 8 := by rw [←h_vals, h_len]
    clear h_vals
    have h_rec : ∀ (l : List Fr) (h : l.length = 8),
        ∃ w0 w1 w2 w3 w4 w5 w6 w7, l = [w0, w1, w2, w3, w4, w5, w6, w7] := by
      intro l hl
      match l with
      | [] =>
        simp at hl
        all_goals try { contradiction }
      | w0 :: l1 =>
        simp at hl
        match l1 with
        | [] =>
          simp at hl
          all_goals try { contradiction }
        | w1 :: l2 =>
          simp at hl
          match l2 with
          | [] =>
            simp at hl
            all_goals try { contradiction }
          | w2 :: l3 =>
            simp at hl
            match l3 with
            | [] =>
              simp at hl
              all_goals try { contradiction }
            | w3 :: l4 =>
              simp at hl
              match l4 with
              | [] =>
                simp at hl
                all_goals try { contradiction }
              | w4 :: l5 =>
                simp at hl
                match l5 with
                | [] =>
                  simp at hl
                  all_goals try { contradiction }
                | w5 :: l6 =>
                  simp at hl
                  match l6 with
                  | [] =>
                    simp at hl
                    all_goals try { contradiction }
                  | w6 :: l7 =>
                    simp at hl
                    match l7 with
                    | [] =>
                      simp at hl
                      all_goals try { contradiction }
                    | w7 :: t =>
                      simp at hl
                      have ht : t = [] := by
                        simp_all [List.length_eq_zero]
                      exact ⟨w0, w1, w2, w3, w4, w5, w6, w7, by simp [ht]⟩
    exact h_rec vals h8

  rcases h_exists with ⟨w0, w1, w2, w3, w4, w5, w6, w7, hw_eq⟩

  -- Apply evalLC lemmas using the decomposition
  have h_eval_a : evalLC [0, 0, 0, 1, 0, 0, 0, 0] assignment.values = Fr.fromNat value_fr.val := by
    rw [hw_eq]
    rw [evalLC_c2_a w0 w1 w2 w3 w4 w5 w6 w7]
    have h3 : value_fr = w3 := by
      simp [value_fr, Assignment.private]
      rw [hw_eq]
      rfl
    rw [h3]

  have h_eval_b : evalLC [0, 0, 0, 0, 0, 0, 0, 1] assignment.values = Fr.fromNat inv_val.val := by
    rw [hw_eq]
    rw [evalLC_c2_b w0 w1 w2 w3 w4 w5 w6 w7]
    have h7 : inv_val = w7 := by
      simp [inv_val, Assignment.private]
      rw [hw_eq]
      rfl
    rw [h7]

  have h_eval_c : evalLC [1, 0, 0, 0, 0, 0, 0, 0] assignment.values = Fr.fromNat assignment.one.val := by
    rw [hw_eq]
    rw [evalLC_c2_c w0 w1 w2 w3 w4 w5 w6 w7]
    have h0 : assignment.one = w0 := by
      simp [Assignment.one]
      rw [hw_eq]
      rfl
    rw [h0]

  -- Extract specific C2 constraint satisfaction from the universal quantifier
  have h_c2 : constraintSatisfied c2Constraint assignment.values := by
    have h_mem : c2Constraint ∈ (DepositCircuit commitment asset_id).constraints := by simp [DepositCircuit]
    exact h_c2_sat c2Constraint h_mem

  -- From C2: Fr.fromNat value_fr.val * Fr.fromNat inv_val.val = Fr.fromNat assignment.one.val
  have h_c2_fr : Fr.fromNat value_fr.val * Fr.fromNat inv_val.val = Fr.fromNat assignment.one.val := by
    simp [constraintSatisfied, c2Constraint] at h_c2
    rw [h_eval_a, h_eval_b, h_eval_c] at h_c2
    exact h_c2

  -- Since assignment.one = 1, the right side is Fr.fromNat 1 = 1
  have h_c2_rhs : Fr.fromNat assignment.one.val = 1 := by
    rw [h_one]
    rfl

  rw [h_c2_rhs] at h_c2_fr

  -- Convert to the product in Fr: value_fr * inv_val = 1
  have h_c2 : value_fr * inv_val = 1 := by
    have h_mul : Fr.fromNat value_fr.val * Fr.fromNat inv_val.val = Fr.fromNat (value_fr.val * inv_val.val) := by
      rw [← Fr.mul_fromNat value_fr.val inv_val.val]
    rw [h_mul] at h_c2_fr
    have h_val : value_fr * inv_val = Fr.fromNat (value_fr.val * inv_val.val) := rfl
    rw [h_val]
    exact h_c2_fr

  -- value_fr.val % BN254_P ≠ 0: if it were 0, then value_fr * inv_val = 0 ≠ 1
  have h_nz_mod : value_fr.val % BN254_P ≠ 0 := by
    intro h_zero
    have h1 : (value_fr.val * inv_val.val) % BN254_P = 0 := by
      rw [Nat.mul_mod]
      rw [h_zero]
      simp
    have h2 : Fr.fromNat (value_fr.val * inv_val.val) = Fr.fromNat 0 := by
      rw [Fr.fromNat_eq_iff]
      exact h1
    have h3 : value_fr * inv_val = 0 := by
      have h_val : value_fr * inv_val = Fr.fromNat (value_fr.val * inv_val.val) := rfl
      rw [h_val, h2]
      rfl
    rw [h3] at h_c2
    have h4 : (0 : Fr) = 1 := h_c2
    have h5 : (0 : Fr).val = (1 : Fr).val := by rw [h4]
    simp at h5

  -- Since value_nat = value_fr.val and value_fr.val % BN254_P ≠ 0,
  -- we have value_nat > 0 (because 0 % p = 0)
  have h_pos : value_nat > 0 := by
    have h_nz : value_nat ≠ 0 := by
      intro h_zero
      have h_val_zero : value_fr.val = 0 := by
        have h1 : value_fr.val = value_nat := by rfl
        rw [h1, h_zero]
      rw [h_val_zero] at h_nz_mod
      simp at h_nz_mod
    exact Nat.pos_of_ne_zero h_nz

  -- C3: value_nat < 2^128 from semantic constraints
  have h_range : value_nat < 2 ^ 128 := by
    simp [DepositCircuit, depositSemantic] at h_semantic
    rcases h_semantic with ⟨_, h_range, _⟩
    exact h_range

  -- Derive value bound needed for encoding equality
  have h_val_lt_p : value_fr.val < BN254_P := by
    have h2 : 2 ^ 128 < BN254_P := by decide
    have h3 : value_nat < BN254_P := Nat.lt_trans h_range h2
    have h4 : value_fr.val = value_nat := rfl
    rw [h4]
    exact h3

  -- value_fr = Fr.fromNat value_nat (since value_fr.val < BN254_P)
  have h_val_eq : Fr.fromNat value_nat = value_fr := by
    have h1 : value_nat = value_fr.val := rfl
    rw [h1]
    exact Fr.fromNat_eq_of_lt value_fr h_val_lt_p

  -- inv_val = Fr.inv (Fr.fromNat value_nat) (by inverse uniqueness)
  have h_inv_eq : inv_val = Fr.inv (Fr.fromNat value_nat) := by
    have h : Fr.inv (Fr.fromNat value_nat) = Fr.inv value_fr := by
      rw [h_val_eq]
    rw [h]
    exact Fr.inv_unique value_fr inv_val h_nz_mod h_c2

  -- Public input and witness component equalities
  have h_w0 : w0 = 1 := by
    have h : assignment.one = w0 := by
      simp [Assignment.one]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_one

  have h_w1 : w1 = commitment := by
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

  have h_w3 : w3 = Fr.fromNat value_nat := by
    have h : value_fr = w3 := by
      simp [value_fr, Assignment.private]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_val_eq.symm

  have h_w4 : w4 = rcm := by
    have h : rcm = w4 := by
      simp [rcm, Assignment.private]
      rw [hw_eq]
      rfl
    exact h.symm

  have h_w5 : w5 = ivk := by
    have h : ivk = w5 := by
      simp [ivk, Assignment.private]
      rw [hw_eq]
      rfl
    exact h.symm

  have h_w6 : w6 = rho := by
    have h : rho = w6 := by
      simp [rho, Assignment.private]
      rw [hw_eq]
      rfl
    exact h.symm

  have h_w7 : w7 = Fr.inv (Fr.fromNat value_nat) := by
    have h : inv_val = w7 := by
      simp [inv_val, Assignment.private]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_inv_eq

  -- Extract C1 and C4 hash equalities from semantic constraints
  have h_c1 : poseidonHash [value_fr, asset_id, rcm, rho]
        (by simp)
      = commitment := by
    simp [DepositCircuit, depositSemantic] at h_semantic
    rcases h_semantic with ⟨h_c1, _, _⟩
    exact h_c1

  have h_c4 : poseidonHashTagged "rcm" [ivk, value_fr, asset_id, rho]
        (by simp)
      = rcm := by
    simp [DepositCircuit, depositSemantic] at h_semantic
    rcases h_semantic with ⟨_, _, h_c4⟩
    exact h_c4

  -- Construct the witness
  refine ⟨⟨value_nat, rcm, ivk, rho, h_pos, h_range⟩, ⟨?_, ?_⟩, ?_⟩
  · -- C1: commitment = H(value, asset_id, rcm, rho)
    simp [DepositWitness.commitment]
    rw [h_val_eq]
    exact h_c1
  · -- C4: rcm = H_tag("rcm", ivk, value, asset_id, rho)
    simp [DepositWitness.rcmValid]
    rw [h_val_eq]
    exact h_c4
  · -- Encoding equality: show that encodeDeposit reproduces the assignment
    have h_list :
        [1, commitment, asset_id, Fr.fromNat value_nat, rcm, ivk, rho, Fr.inv (Fr.fromNat value_nat)]
        = [w0, w1, w2, w3, w4, w5, w6, w7] := by
      rw [h_w0, h_w1, h_w2, h_w3, h_w4, h_w5, h_w6, h_w7]
    generalize h_enc : encodeDeposit ⟨value_nat, rcm, ivk, rho, h_pos, h_range⟩ commitment asset_id = enc
    cases enc with | mk enc_vals enc_h =>
    cases assignment with | mk vals h_size =>
    simp at hw_eq
    have h_vals : enc_vals = vals := by
      have h1 : (encodeDeposit ⟨value_nat, rcm, ivk, rho, h_pos, h_range⟩ commitment asset_id).values = enc_vals := by
        rw [h_enc]
      have h2 : (encodeDeposit ⟨value_nat, rcm, ivk, rho, h_pos, h_range⟩ commitment asset_id).values = vals := by
        simp [encodeDeposit, encodeDepositWitness]
        rw [hw_eq]
        exact h_list
      rw [←h1]
      exact h2
    simp [h_vals]

end CallchainShielded
