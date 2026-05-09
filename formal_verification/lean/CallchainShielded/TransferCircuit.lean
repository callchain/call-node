import CallchainShielded.Fr
import CallchainShielded.Poseidon
import CallchainShielded.R1CS
import CallchainShielded.Common

namespace CallchainShielded

set_option maxRecDepth 10000

-- ============================================================================
-- High-Level Witness and Validity
-- ============================================================================

/-- An input note witness for TransferCircuit (N=2 inputs) -/
structure InputNoteWitness where
  value : Nat
  rcm : Fr
  ivk : Fr
  rho : Fr
  sk : Fr
  merkle_path : MerklePath
  h_value_pos : value > 0
  h_value_range : value < 2 ^ 128

/-- An output note witness for TransferCircuit (M=2 outputs) -/
structure OutputNoteWitness where
  value : Nat
  rcm : Fr
  rho : Fr
  h_value_pos : value > 0
  h_value_range : value < 2 ^ 128

/-- Full transfer witness (2 inputs, 2 outputs) -/
structure TransferWitness where
  input1 : InputNoteWitness
  input2 : InputNoteWitness
  output1 : OutputNoteWitness
  output2 : OutputNoteWitness

namespace InputNoteWitness

/-- Compute the note commitment for an input note -/
def commitment (w : InputNoteWitness) (asset_id : Fr) : Fr :=
  noteCommitment w.value asset_id w.rcm w.rho

end InputNoteWitness

namespace OutputNoteWitness

/-- Compute the note commitment for an output note -/
def commitment (w : OutputNoteWitness) (asset_id : Fr) : Fr :=
  noteCommitment w.value asset_id w.rcm w.rho

end OutputNoteWitness

/-- High-level validity predicate for a transfer.

    T1: Nullifier derivation for both inputs
    T2: Merkle path validity for both inputs
    T3: Spending rights (IVK matches SK derivation)
    T4: Value conservation (outputs <= inputs)
    T5: Output commitments -/
def ValidTransfer (w : TransferWitness)
    (nullifiers : Fr × Fr) (commitments : Fr × Fr)
    (asset_id merkle_root : Fr) : Prop :=
  -- T1: Nullifier derivation
  nullifierFromSK w.input1.sk w.input1.rho = nullifiers.1
  ∧ nullifierFromSK w.input2.sk w.input2.rho = nullifiers.2
  -- T2: Merkle path validity
  ∧ verifyMerklePath (w.input1.commitment asset_id) w.input1.merkle_path = merkle_root
  ∧ verifyMerklePath (w.input2.commitment asset_id) w.input2.merkle_path = merkle_root
  -- T3: Spending rights
  ∧ ivkFromSK w.input1.sk = w.input1.ivk
  ∧ ivkFromSK w.input2.sk = w.input2.ivk
  -- T4: Value conservation
  ∧ w.output1.value + w.output2.value ≤ w.input1.value + w.input2.value
  -- T5: Output commitments
  ∧ w.output1.commitment asset_id = commitments.1
  ∧ w.output2.commitment asset_id = commitments.2

-- ============================================================================
-- R1CS Model
-- ============================================================================

/- Witness layout for TransferCircuit (27 elements):
   [1,
    nullifier1, nullifier2, commitment1, commitment2, asset_id, merkle_root,  -- public (indices 1-6)
    input1_value, input1_rcm, input1_ivk, input1_rho, input1_sk, inv_input1,  -- private 0-5
    input2_value, input2_rcm, input2_ivk, input2_rho, input2_sk, inv_input2,  -- private 6-11
    output1_value, output1_rcm, output1_rho, inv_output1,                      -- private 12-15
    output2_value, output2_rcm, output2_rho, inv_output2]                      -- private 16-19

   Constraints:
   C2_in1: input1_value * inv_input1 = 1
   C2_in2: input2_value * inv_input2 = 1
   C2_out1: output1_value * inv_output1 = 1
   C2_out2: output2_value * inv_output2 = 1

   Semantic constraints:
   T1-T5 (see ValidTransfer above)
   Plus range checks: all values < 2^128
-/

/-- Flat encoding of a transfer witness into an R1CS assignment. -/
def encodeTransferWitness (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr)
    (in1_val in1_rcm in1_ivk in1_rho in1_sk inv_in1 : Fr)
    (in2_val in2_rcm in2_ivk in2_rho in2_sk inv_in2 : Fr)
    (out1_val out1_rcm out1_rho inv_out1 : Fr)
    (out2_val out2_rcm out2_rho inv_out2 : Fr) : Assignment 6 20 :=
  ⟨[1, nullifiers.1, nullifiers.2, commitments.1, commitments.2, asset_id, merkle_root,
    in1_val, in1_rcm, in1_ivk, in1_rho, in1_sk, inv_in1,
    in2_val, in2_rcm, in2_ivk, in2_rho, in2_sk, inv_in2,
    out1_val, out1_rcm, out1_rho, inv_out1,
    out2_val, out2_rcm, out2_rho, inv_out2],
   by simp⟩

/-- Encode a transfer witness into an R1CS assignment.
    Computes inverse witnesses for all non-zero values. -/
def encodeTransfer (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) : Assignment 6 20 :=
  let in1_val : Fr := w.input1.value
  let in2_val : Fr := w.input2.value
  let out1_val : Fr := w.output1.value
  let out2_val : Fr := w.output2.value
  encodeTransferWitness nullifiers commitments asset_id merkle_root
    in1_val w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv in1_val)
    in2_val w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv in2_val)
    out1_val w.output1.rcm w.output1.rho (Fr.inv out1_val)
    out2_val w.output2.rcm w.output2.rho (Fr.inv out2_val)

-- ============================================================================
-- Accessor lemmas for encodeTransfer (avoid simp recursion on large lists)
-- ============================================================================

theorem encodeTransfer_in1_val (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    ((encodeTransfer w nullifiers commitments asset_id merkle_root).private 0 (show 0 < 20 by decide)).val = w.input1.value := by
  have h : (encodeTransfer w nullifiers commitments asset_id merkle_root).private 0 (show 0 < 20 by decide) = (w.input1.value : Fr) := rfl
  rw [h]
  simp [Fr.fromNat]
  rw [Nat.mod_eq_of_lt]
  exact nat_lt_128_to_lt_p w.input1.h_value_range

theorem encodeTransfer_in1_rcm (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 1 (show 1 < 20 by decide) = w.input1.rcm := rfl

theorem encodeTransfer_in1_ivk (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 2 (show 2 < 20 by decide) = w.input1.ivk := rfl

theorem encodeTransfer_in1_rho (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 3 (show 3 < 20 by decide) = w.input1.rho := rfl

theorem encodeTransfer_in1_sk (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 4 (show 4 < 20 by decide) = w.input1.sk := rfl

theorem encodeTransfer_in2_val (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    ((encodeTransfer w nullifiers commitments asset_id merkle_root).private 6 (show 6 < 20 by decide)).val = w.input2.value := by
  have h : (encodeTransfer w nullifiers commitments asset_id merkle_root).private 6 (show 6 < 20 by decide) = (w.input2.value : Fr) := rfl
  rw [h]
  simp [Fr.fromNat]
  rw [Nat.mod_eq_of_lt]
  exact nat_lt_128_to_lt_p w.input2.h_value_range

theorem encodeTransfer_in2_rcm (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 7 (show 7 < 20 by decide) = w.input2.rcm := rfl

theorem encodeTransfer_in2_ivk (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 8 (show 8 < 20 by decide) = w.input2.ivk := rfl

theorem encodeTransfer_in2_rho (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 9 (show 9 < 20 by decide) = w.input2.rho := rfl

theorem encodeTransfer_in2_sk (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 10 (show 10 < 20 by decide) = w.input2.sk := rfl

theorem encodeTransfer_out1_val (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    ((encodeTransfer w nullifiers commitments asset_id merkle_root).private 12 (show 12 < 20 by decide)).val = w.output1.value := by
  have h : (encodeTransfer w nullifiers commitments asset_id merkle_root).private 12 (show 12 < 20 by decide) = (w.output1.value : Fr) := rfl
  rw [h]
  simp [Fr.fromNat]
  rw [Nat.mod_eq_of_lt]
  exact nat_lt_128_to_lt_p w.output1.h_value_range

theorem encodeTransfer_out1_rcm (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 13 (show 13 < 20 by decide) = w.output1.rcm := rfl

theorem encodeTransfer_out1_rho (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 14 (show 14 < 20 by decide) = w.output1.rho := rfl

theorem encodeTransfer_out2_val (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    ((encodeTransfer w nullifiers commitments asset_id merkle_root).private 16 (show 16 < 20 by decide)).val = w.output2.value := by
  have h : (encodeTransfer w nullifiers commitments asset_id merkle_root).private 16 (show 16 < 20 by decide) = (w.output2.value : Fr) := rfl
  rw [h]
  simp [Fr.fromNat]
  rw [Nat.mod_eq_of_lt]
  exact nat_lt_128_to_lt_p w.output2.h_value_range

theorem encodeTransfer_out2_rcm (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 17 (show 17 < 20 by decide) = w.output2.rcm := rfl

theorem encodeTransfer_out2_rho (w : TransferWitness) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) :
    (encodeTransfer w nullifiers commitments asset_id merkle_root).private 18 (show 18 < 20 by decide) = w.output2.rho := rfl

-- C2_in1: input1_value * inv_input1 = 1
def c2In1Constraint : R1CSConstraint :=
  { a := [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  , b := [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  , c := [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  }

-- C2_in2: input2_value * inv_input2 = 1
def c2In2Constraint : R1CSConstraint :=
  { a := [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  , b := [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]
  , c := [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  }

-- C2_out1: output1_value * inv_output1 = 1
def c2Out1Constraint : R1CSConstraint :=
  { a := [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]
  , b := [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]
  , c := [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  }

-- C2_out2: output2_value * inv_output2 = 1
def c2Out2Constraint : R1CSConstraint :=
  { a := [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]
  , b := [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
  , c := [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  }

/-- Semantic constraints for TransferCircuit.
    Merkle paths are passed as parameters since they are structured witness data
    not represented in the flat Fr assignment. -/
def transferSemantic (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr)
    (merkle_path1 merkle_path2 : MerklePath)
    (a : Assignment 6 20) : Prop :=
  let in1_val := a.private 0 (by decide)
  let in1_val_nat := in1_val.val
  let in1_rcm := a.private 1 (by decide)
  let in1_ivk := a.private 2 (by decide)
  let in1_rho := a.private 3 (by decide)
  let in1_sk := a.private 4 (by decide)
  let in2_val := a.private 6 (by decide)
  let in2_val_nat := in2_val.val
  let in2_rcm := a.private 7 (by decide)
  let in2_ivk := a.private 8 (by decide)
  let in2_rho := a.private 9 (by decide)
  let in2_sk := a.private 10 (by decide)
  let out1_val := a.private 12 (by decide)
  let out1_val_nat := out1_val.val
  let out1_rcm := a.private 13 (by decide)
  let out1_rho := a.private 14 (by decide)
  let out2_val := a.private 16 (by decide)
  let out2_val_nat := out2_val.val
  let out2_rcm := a.private 17 (by decide)
  let out2_rho := a.private 18 (by decide)
  -- T1: Nullifier derivation
  nullifierFromSK in1_sk in1_rho = nullifiers.1
  ∧ nullifierFromSK in2_sk in2_rho = nullifiers.2
  -- T2: Merkle path validity
  ∧ verifyMerklePath (noteCommitment in1_val_nat asset_id in1_rcm in1_rho) merkle_path1 = merkle_root
  ∧ verifyMerklePath (noteCommitment in2_val_nat asset_id in2_rcm in2_rho) merkle_path2 = merkle_root
  -- T3: Spending rights
  ∧ ivkFromSK in1_sk = in1_ivk
  ∧ ivkFromSK in2_sk = in2_ivk
  -- T4: Value conservation
  ∧ out1_val_nat + out2_val_nat ≤ in1_val_nat + in2_val_nat
  -- T5: Output commitments
  ∧ noteCommitment out1_val_nat asset_id out1_rcm out1_rho = commitments.1
  ∧ noteCommitment out2_val_nat asset_id out2_rcm out2_rho = commitments.2
  -- Range checks
  ∧ in1_val_nat < 2 ^ 128
  ∧ in2_val_nat < 2 ^ 128
  ∧ out1_val_nat < 2 ^ 128
  ∧ out2_val_nat < 2 ^ 128

/-- The TransferCircuit R1CS instance.
    C2 constraints are native R1CS gates; all others are semantic assertions. -/
def TransferCircuit (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr)
    (merkle_path1 merkle_path2 : MerklePath) : R1CS 6 20 :=
  { constraints := [c2In1Constraint, c2In2Constraint, c2Out1Constraint, c2Out2Constraint]
  , semantic := transferSemantic nullifiers commitments asset_id merkle_root merkle_path1 merkle_path2
  }

-- ============================================================================
-- Helper lemmas
-- ============================================================================

/-- Decompose a list of length 27 into 27 named elements.
    Defined as a separate theorem because Lean's pattern matching exhaustiveness
    checker has difficulty with long list patterns on structure projections. -/
theorem list_decompose_27 (vals : List Fr) (h : vals.length = 27) :
    ∃ w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26,
    vals = [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26] := by
  match vals with
  | [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26] =>
    exact ⟨w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26, rfl⟩

-- ============================================================================
-- evalLC computation lemmas
-- ============================================================================

/-- Selector at index 0 (constant 1) -/
theorem evalLC_sel_0 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26 : Fr) :
    evalLC [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26]
    = Fr.fromNat w0.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 7 (input1_value) -/
theorem evalLC_sel_7 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26 : Fr) :
    evalLC [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26]
    = Fr.fromNat w7.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 12 (inv_input1) -/
theorem evalLC_sel_12 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26 : Fr) :
    evalLC [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26]
    = Fr.fromNat w12.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 13 (input2_value) -/
theorem evalLC_sel_13 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26 : Fr) :
    evalLC [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26]
    = Fr.fromNat w13.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 18 (inv_input2) -/
theorem evalLC_sel_18 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26 : Fr) :
    evalLC [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26]
    = Fr.fromNat w18.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 19 (output1_value) -/
theorem evalLC_sel_19 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26 : Fr) :
    evalLC [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26]
    = Fr.fromNat w19.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 22 (inv_output1) -/
theorem evalLC_sel_22 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26 : Fr) :
    evalLC [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26]
    = Fr.fromNat w22.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 23 (output2_value) -/
theorem evalLC_sel_23 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26 : Fr) :
    evalLC [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26]
    = Fr.fromNat w23.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

/-- Selector at index 26 (inv_output2) -/
theorem evalLC_sel_26 (w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26 : Fr) :
    evalLC [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
           [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26]
    = Fr.fromNat w26.val := by
  repeat rw [evalLC]
  simp only [Fr.zero_mul, Fr.add_zero', Fr.zero_add', Fr.one_mul', Fr.fromNat_val, Fr.val_zero, Fr.fromNat_zero]

-- ============================================================================
-- Completeness Theorem
-- ============================================================================

/-- **Completeness**: Every valid transfer witness satisfies the R1CS constraints. -/
theorem TransferCircuit.completeness :
  ∀ (w : TransferWitness) (nullifiers commitments : Fr × Fr) (asset_id merkle_root : Fr),
  ValidTransfer w nullifiers commitments asset_id merkle_root →
  (TransferCircuit nullifiers commitments asset_id merkle_root w.input1.merkle_path w.input2.merkle_path).satisfied
    (encodeTransfer w nullifiers commitments asset_id merkle_root) := by
  intro w nullifiers commitments asset_id merkle_root h_valid
  constructor
  · -- Prove all low-level C2 constraints are satisfied
    intro c hc
    simp [TransferCircuit] at hc
    rcases hc with (hc | hc | hc | hc)
    · -- C2_in1: input1_value * inv_input1 = 1
      rw [hc]
      simp [constraintSatisfied, c2In1Constraint, encodeTransfer, encodeTransferWitness]
      let in1_val : Fr := w.input1.value
      let inv_in1 := Fr.inv in1_val
      rw [evalLC_sel_7 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          in1_val w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk inv_in1
          w.input2.value w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv (w.input2.value : Fr))
          w.output1.value w.output1.rcm w.output1.rho (Fr.inv (w.output1.value : Fr))
          w.output2.value w.output2.rcm w.output2.rho (Fr.inv (w.output2.value : Fr))]
      rw [evalLC_sel_12 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          in1_val w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk inv_in1
          w.input2.value w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv (w.input2.value : Fr))
          w.output1.value w.output1.rcm w.output1.rho (Fr.inv (w.output1.value : Fr))
          w.output2.value w.output2.rcm w.output2.rho (Fr.inv (w.output2.value : Fr))]
      rw [evalLC_sel_0 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          in1_val w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk inv_in1
          w.input2.value w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv (w.input2.value : Fr))
          w.output1.value w.output1.rcm w.output1.rho (Fr.inv (w.output1.value : Fr))
          w.output2.value w.output2.rcm w.output2.rho (Fr.inv (w.output2.value : Fr))]
      have h_pos : w.input1.value > 0 := w.input1.h_value_pos
      have h_range : w.input1.value < 2 ^ 128 := w.input1.h_value_range
      have h_lt_p : w.input1.value < BN254_P := nat_lt_128_to_lt_p h_range
      have h_nz : (w.input1.value : Fr).val % BN254_P ≠ 0 := nat_nonzero_to_fr h_pos h_lt_p
      have h_mul : Fr.fromNat in1_val.val * Fr.fromNat (Fr.inv in1_val).val = Fr.fromNat (in1_val.val * (Fr.inv in1_val).val) := by
        rw [← Fr.mul_fromNat in1_val.val (Fr.inv in1_val).val]
      have h_inv : Fr.fromNat (in1_val.val * (Fr.inv in1_val).val) = 1 := by
        have h : Fr.fromNat (in1_val.val * (Fr.inv in1_val).val) = in1_val * Fr.inv in1_val := rfl
        rw [h]
        exact Fr.inv_mul in1_val h_nz
      rw [h_mul, h_inv]
      rfl
    · -- C2_in2: input2_value * inv_input2 = 1
      rw [hc]
      simp [constraintSatisfied, c2In2Constraint, encodeTransfer, encodeTransferWitness]
      let in2_val : Fr := w.input2.value
      let inv_in2 := Fr.inv in2_val
      rw [evalLC_sel_13 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          w.input1.value w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv (w.input1.value : Fr))
          in2_val w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk inv_in2
          w.output1.value w.output1.rcm w.output1.rho (Fr.inv (w.output1.value : Fr))
          w.output2.value w.output2.rcm w.output2.rho (Fr.inv (w.output2.value : Fr))]
      rw [evalLC_sel_18 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          w.input1.value w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv (w.input1.value : Fr))
          in2_val w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk inv_in2
          w.output1.value w.output1.rcm w.output1.rho (Fr.inv (w.output1.value : Fr))
          w.output2.value w.output2.rcm w.output2.rho (Fr.inv (w.output2.value : Fr))]
      rw [evalLC_sel_0 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          w.input1.value w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv (w.input1.value : Fr))
          in2_val w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk inv_in2
          w.output1.value w.output1.rcm w.output1.rho (Fr.inv (w.output1.value : Fr))
          w.output2.value w.output2.rcm w.output2.rho (Fr.inv (w.output2.value : Fr))]
      have h_pos : w.input2.value > 0 := w.input2.h_value_pos
      have h_range : w.input2.value < 2 ^ 128 := w.input2.h_value_range
      have h_lt_p : w.input2.value < BN254_P := nat_lt_128_to_lt_p h_range
      have h_nz : (w.input2.value : Fr).val % BN254_P ≠ 0 := nat_nonzero_to_fr h_pos h_lt_p
      have h_mul : Fr.fromNat in2_val.val * Fr.fromNat (Fr.inv in2_val).val = Fr.fromNat (in2_val.val * (Fr.inv in2_val).val) := by
        rw [← Fr.mul_fromNat in2_val.val (Fr.inv in2_val).val]
      have h_inv : Fr.fromNat (in2_val.val * (Fr.inv in2_val).val) = 1 := by
        have h : Fr.fromNat (in2_val.val * (Fr.inv in2_val).val) = in2_val * Fr.inv in2_val := rfl
        rw [h]
        exact Fr.inv_mul in2_val h_nz
      rw [h_mul, h_inv]
      rfl
    · -- C2_out1: output1_value * inv_output1 = 1
      rw [hc]
      simp [constraintSatisfied, c2Out1Constraint, encodeTransfer, encodeTransferWitness]
      let out1_val : Fr := w.output1.value
      let inv_out1 := Fr.inv out1_val
      rw [evalLC_sel_19 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          w.input1.value w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv (w.input1.value : Fr))
          w.input2.value w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv (w.input2.value : Fr))
          out1_val w.output1.rcm w.output1.rho inv_out1
          w.output2.value w.output2.rcm w.output2.rho (Fr.inv (w.output2.value : Fr))]
      rw [evalLC_sel_22 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          w.input1.value w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv (w.input1.value : Fr))
          w.input2.value w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv (w.input2.value : Fr))
          out1_val w.output1.rcm w.output1.rho inv_out1
          w.output2.value w.output2.rcm w.output2.rho (Fr.inv (w.output2.value : Fr))]
      rw [evalLC_sel_0 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          w.input1.value w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv (w.input1.value : Fr))
          w.input2.value w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv (w.input2.value : Fr))
          out1_val w.output1.rcm w.output1.rho inv_out1
          w.output2.value w.output2.rcm w.output2.rho (Fr.inv (w.output2.value : Fr))]
      have h_pos : w.output1.value > 0 := w.output1.h_value_pos
      have h_range : w.output1.value < 2 ^ 128 := w.output1.h_value_range
      have h_lt_p : w.output1.value < BN254_P := nat_lt_128_to_lt_p h_range
      have h_nz : (w.output1.value : Fr).val % BN254_P ≠ 0 := nat_nonzero_to_fr h_pos h_lt_p
      have h_mul : Fr.fromNat out1_val.val * Fr.fromNat (Fr.inv out1_val).val = Fr.fromNat (out1_val.val * (Fr.inv out1_val).val) := by
        rw [← Fr.mul_fromNat out1_val.val (Fr.inv out1_val).val]
      have h_inv : Fr.fromNat (out1_val.val * (Fr.inv out1_val).val) = 1 := by
        have h : Fr.fromNat (out1_val.val * (Fr.inv out1_val).val) = out1_val * Fr.inv out1_val := rfl
        rw [h]
        exact Fr.inv_mul out1_val h_nz
      rw [h_mul, h_inv]
      rfl
    · -- C2_out2: output2_value * inv_output2 = 1
      rw [hc]
      simp [constraintSatisfied, c2Out2Constraint, encodeTransfer, encodeTransferWitness]
      let out2_val : Fr := w.output2.value
      let inv_out2 := Fr.inv out2_val
      rw [evalLC_sel_23 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          w.input1.value w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv (w.input1.value : Fr))
          w.input2.value w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv (w.input2.value : Fr))
          w.output1.value w.output1.rcm w.output1.rho (Fr.inv (w.output1.value : Fr))
          out2_val w.output2.rcm w.output2.rho inv_out2]
      rw [evalLC_sel_26 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          w.input1.value w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv (w.input1.value : Fr))
          w.input2.value w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv (w.input2.value : Fr))
          w.output1.value w.output1.rcm w.output1.rho (Fr.inv (w.output1.value : Fr))
          out2_val w.output2.rcm w.output2.rho inv_out2]
      rw [evalLC_sel_0 1 nullifiers.1 nullifiers.2 commitments.1 commitments.2 asset_id merkle_root
          w.input1.value w.input1.rcm w.input1.ivk w.input1.rho w.input1.sk (Fr.inv (w.input1.value : Fr))
          w.input2.value w.input2.rcm w.input2.ivk w.input2.rho w.input2.sk (Fr.inv (w.input2.value : Fr))
          w.output1.value w.output1.rcm w.output1.rho (Fr.inv (w.output1.value : Fr))
          out2_val w.output2.rcm w.output2.rho inv_out2]
      have h_pos : w.output2.value > 0 := w.output2.h_value_pos
      have h_range : w.output2.value < 2 ^ 128 := w.output2.h_value_range
      have h_lt_p : w.output2.value < BN254_P := nat_lt_128_to_lt_p h_range
      have h_nz : (w.output2.value : Fr).val % BN254_P ≠ 0 := nat_nonzero_to_fr h_pos h_lt_p
      have h_mul : Fr.fromNat out2_val.val * Fr.fromNat (Fr.inv out2_val).val = Fr.fromNat (out2_val.val * (Fr.inv out2_val).val) := by
        rw [← Fr.mul_fromNat out2_val.val (Fr.inv out2_val).val]
      have h_inv : Fr.fromNat (out2_val.val * (Fr.inv out2_val).val) = 1 := by
        have h : Fr.fromNat (out2_val.val * (Fr.inv out2_val).val) = out2_val * Fr.inv out2_val := rfl
        rw [h]
        exact Fr.inv_mul out2_val h_nz
      rw [h_mul, h_inv]
      rfl
  · -- Prove semantic constraints (T1-T5 + range checks)
    rcases h_valid with ⟨h_n1, h_n2, h_m1, h_m2, h_s1, h_s2, h_vc, h_c1, h_c2⟩
    -- Unfold semantic definition so rw can see the assignment accessors
    simp only [TransferCircuit, transferSemantic]
    -- Rewrite assignment accessors to witness fields using explicit lemmas
    rw [encodeTransfer_in1_val, encodeTransfer_in1_rcm, encodeTransfer_in1_rho,
        encodeTransfer_in2_val, encodeTransfer_in2_rcm, encodeTransfer_in2_rho,
        encodeTransfer_out1_val, encodeTransfer_out1_rcm, encodeTransfer_out1_rho,
        encodeTransfer_out2_val, encodeTransfer_out2_rcm, encodeTransfer_out2_rho]
    constructor
    · exact h_n1
    constructor
    · exact h_n2
    constructor
    · exact h_m1
    constructor
    · exact h_m2
    constructor
    · exact h_s1
    constructor
    · exact h_s2
    constructor
    · exact h_vc
    constructor
    · exact h_c1
    constructor
    · exact h_c2
    constructor
    · exact w.input1.h_value_range
    constructor
    · exact w.input2.h_value_range
    constructor
    · exact w.output1.h_value_range
    · exact w.output2.h_value_range

-- ============================================================================
-- Soundness Theorem
-- ============================================================================

/-- **Soundness**: Every R1CS-satisfying assignment corresponds to a valid witness.

    If an assignment satisfies the TransferCircuit R1CS constraints, then there
    exists a valid transfer witness that produces those public inputs. -/
theorem TransferCircuit.soundness :
  ∀ (assignment : Assignment 6 20) (nullifiers commitments : Fr × Fr)
    (asset_id merkle_root : Fr) (merkle_path1 merkle_path2 : MerklePath),
  (TransferCircuit nullifiers commitments asset_id merkle_root merkle_path1 merkle_path2).satisfied assignment →
  assignment.one = 1 →
  assignment.public 0 (by decide) = nullifiers.1 →
  assignment.public 1 (by decide) = nullifiers.2 →
  assignment.public 2 (by decide) = commitments.1 →
  assignment.public 3 (by decide) = commitments.2 →
  assignment.public 4 (by decide) = asset_id →
  assignment.public 5 (by decide) = merkle_root →
  ∃ (w : TransferWitness),
    ValidTransfer w nullifiers commitments asset_id merkle_root ∧
    encodeTransfer w nullifiers commitments asset_id merkle_root = assignment := by
  intro assignment nullifiers commitments asset_id merkle_root merkle_path1 merkle_path2
    h_sat h_one h_pub0 h_pub1 h_pub2 h_pub3 h_pub4 h_pub5

  -- Decode assignment components
  let in1_val_fr := assignment.private 0 (by decide)
  let in1_val_nat := in1_val_fr.val
  let in1_rcm := assignment.private 1 (by decide)
  let in1_ivk := assignment.private 2 (by decide)
  let in1_rho := assignment.private 3 (by decide)
  let in1_sk := assignment.private 4 (by decide)
  let inv_in1 := assignment.private 5 (by decide)
  let in2_val_fr := assignment.private 6 (by decide)
  let in2_val_nat := in2_val_fr.val
  let in2_rcm := assignment.private 7 (by decide)
  let in2_ivk := assignment.private 8 (by decide)
  let in2_rho := assignment.private 9 (by decide)
  let in2_sk := assignment.private 10 (by decide)
  let inv_in2 := assignment.private 11 (by decide)
  let out1_val_fr := assignment.private 12 (by decide)
  let out1_val_nat := out1_val_fr.val
  let out1_rcm := assignment.private 13 (by decide)
  let out1_rho := assignment.private 14 (by decide)
  let inv_out1 := assignment.private 15 (by decide)
  let out2_val_fr := assignment.private 16 (by decide)
  let out2_val_nat := out2_val_fr.val
  let out2_rcm := assignment.private 17 (by decide)
  let out2_rho := assignment.private 18 (by decide)
  let inv_out2 := assignment.private 19 (by decide)

  rcases h_sat with ⟨h_constraints, h_semantic⟩

  -- Decompose assignment.values into 27 named elements
  have h_len : assignment.values.length = 27 := by rw [assignment.h_size]

  rcases list_decompose_27 assignment.values h_len with
    ⟨w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26, hw_eq⟩

  -- Prove each assignment component equals the corresponding witness element
  have h_in1_val : in1_val_fr = w7 := by
    simp [in1_val_fr, Assignment.private]
    rw [hw_eq]
    rfl

  have h_in1_rcm : in1_rcm = w8 := by
    simp [in1_rcm, Assignment.private]
    rw [hw_eq]
    rfl

  have h_in1_ivk : in1_ivk = w9 := by
    simp [in1_ivk, Assignment.private]
    rw [hw_eq]
    rfl

  have h_in1_rho : in1_rho = w10 := by
    simp [in1_rho, Assignment.private]
    rw [hw_eq]
    rfl

  have h_in1_sk : in1_sk = w11 := by
    simp [in1_sk, Assignment.private]
    rw [hw_eq]
    rfl

  have h_inv_in1 : inv_in1 = w12 := by
    simp [inv_in1, Assignment.private]
    rw [hw_eq]
    rfl

  have h_in2_val : in2_val_fr = w13 := by
    simp [in2_val_fr, Assignment.private]
    rw [hw_eq]
    rfl

  have h_in2_rcm : in2_rcm = w14 := by
    simp [in2_rcm, Assignment.private]
    rw [hw_eq]
    rfl

  have h_in2_ivk : in2_ivk = w15 := by
    simp [in2_ivk, Assignment.private]
    rw [hw_eq]
    rfl

  have h_in2_rho : in2_rho = w16 := by
    simp [in2_rho, Assignment.private]
    rw [hw_eq]
    rfl

  have h_in2_sk : in2_sk = w17 := by
    simp [in2_sk, Assignment.private]
    rw [hw_eq]
    rfl

  have h_inv_in2 : inv_in2 = w18 := by
    simp [inv_in2, Assignment.private]
    rw [hw_eq]
    rfl

  have h_out1_val : out1_val_fr = w19 := by
    simp [out1_val_fr, Assignment.private]
    rw [hw_eq]
    rfl

  have h_out1_rcm : out1_rcm = w20 := by
    simp [out1_rcm, Assignment.private]
    rw [hw_eq]
    rfl

  have h_out1_rho : out1_rho = w21 := by
    simp [out1_rho, Assignment.private]
    rw [hw_eq]
    rfl

  have h_inv_out1 : inv_out1 = w22 := by
    simp [inv_out1, Assignment.private]
    rw [hw_eq]
    rfl

  have h_out2_val : out2_val_fr = w23 := by
    simp [out2_val_fr, Assignment.private]
    rw [hw_eq]
    rfl

  have h_out2_rcm : out2_rcm = w24 := by
    simp [out2_rcm, Assignment.private]
    rw [hw_eq]
    rfl

  have h_out2_rho : out2_rho = w25 := by
    simp [out2_rho, Assignment.private]
    rw [hw_eq]
    rfl

  have h_inv_out2 : inv_out2 = w26 := by
    simp [inv_out2, Assignment.private]
    rw [hw_eq]
    rfl

  -- Prove evalLC results for each constraint
  have h_eval_c2_in1_a : evalLC c2In1Constraint.a assignment.values = Fr.fromNat in1_val_fr.val := by
    rw [hw_eq]
    simp [c2In1Constraint]
    rw [evalLC_sel_7 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    rw [h_in1_val]

  have h_eval_c2_in1_b : evalLC c2In1Constraint.b assignment.values = Fr.fromNat inv_in1.val := by
    rw [hw_eq]
    simp [c2In1Constraint]
    rw [evalLC_sel_12 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    rw [h_inv_in1]

  have h_eval_c2_in1_c : evalLC c2In1Constraint.c assignment.values = Fr.fromNat assignment.one.val := by
    rw [hw_eq]
    simp [c2In1Constraint]
    rw [evalLC_sel_0 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    have h0 : assignment.one = w0 := by
      simp [Assignment.one]
      rw [hw_eq]
      rfl
    rw [h0]

  have h_eval_c2_in2_a : evalLC c2In2Constraint.a assignment.values = Fr.fromNat in2_val_fr.val := by
    rw [hw_eq]
    simp [c2In2Constraint]
    rw [evalLC_sel_13 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    rw [h_in2_val]

  have h_eval_c2_in2_b : evalLC c2In2Constraint.b assignment.values = Fr.fromNat inv_in2.val := by
    rw [hw_eq]
    simp [c2In2Constraint]
    rw [evalLC_sel_18 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    rw [h_inv_in2]

  have h_eval_c2_in2_c : evalLC c2In2Constraint.c assignment.values = Fr.fromNat assignment.one.val := by
    rw [hw_eq]
    simp [c2In2Constraint]
    rw [evalLC_sel_0 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    have h0 : assignment.one = w0 := by
      simp [Assignment.one]
      rw [hw_eq]
      rfl
    rw [h0]

  have h_eval_c2_out1_a : evalLC c2Out1Constraint.a assignment.values = Fr.fromNat out1_val_fr.val := by
    rw [hw_eq]
    simp [c2Out1Constraint]
    rw [evalLC_sel_19 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    rw [h_out1_val]

  have h_eval_c2_out1_b : evalLC c2Out1Constraint.b assignment.values = Fr.fromNat inv_out1.val := by
    rw [hw_eq]
    simp [c2Out1Constraint]
    rw [evalLC_sel_22 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    rw [h_inv_out1]

  have h_eval_c2_out1_c : evalLC c2Out1Constraint.c assignment.values = Fr.fromNat assignment.one.val := by
    rw [hw_eq]
    simp [c2Out1Constraint]
    rw [evalLC_sel_0 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    have h0 : assignment.one = w0 := by
      simp [Assignment.one]
      rw [hw_eq]
      rfl
    rw [h0]

  have h_eval_c2_out2_a : evalLC c2Out2Constraint.a assignment.values = Fr.fromNat out2_val_fr.val := by
    rw [hw_eq]
    simp [c2Out2Constraint]
    rw [evalLC_sel_23 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    rw [h_out2_val]

  have h_eval_c2_out2_b : evalLC c2Out2Constraint.b assignment.values = Fr.fromNat inv_out2.val := by
    rw [hw_eq]
    simp [c2Out2Constraint]
    rw [evalLC_sel_26 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    rw [h_inv_out2]

  have h_eval_c2_out2_c : evalLC c2Out2Constraint.c assignment.values = Fr.fromNat assignment.one.val := by
    rw [hw_eq]
    simp [c2Out2Constraint]
    rw [evalLC_sel_0 w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 w13 w14 w15 w16 w17 w18 w19 w20 w21 w22 w23 w24 w25 w26]
    have h0 : assignment.one = w0 := by
      simp [Assignment.one]
      rw [hw_eq]
      rfl
    rw [h0]

  -- Extract C2 constraint satisfaction
  have h_c2_in1 : constraintSatisfied c2In1Constraint assignment.values := by
    have h_mem : c2In1Constraint ∈ (TransferCircuit nullifiers commitments asset_id merkle_root merkle_path1 merkle_path2).constraints := by simp [TransferCircuit]
    exact h_constraints c2In1Constraint h_mem

  have h_c2_in2 : constraintSatisfied c2In2Constraint assignment.values := by
    have h_mem : c2In2Constraint ∈ (TransferCircuit nullifiers commitments asset_id merkle_root merkle_path1 merkle_path2).constraints := by simp [TransferCircuit]
    exact h_constraints c2In2Constraint h_mem

  have h_c2_out1 : constraintSatisfied c2Out1Constraint assignment.values := by
    have h_mem : c2Out1Constraint ∈ (TransferCircuit nullifiers commitments asset_id merkle_root merkle_path1 merkle_path2).constraints := by simp [TransferCircuit]
    exact h_constraints c2Out1Constraint h_mem

  have h_c2_out2 : constraintSatisfied c2Out2Constraint assignment.values := by
    have h_mem : c2Out2Constraint ∈ (TransferCircuit nullifiers commitments asset_id merkle_root merkle_path1 merkle_path2).constraints := by simp [TransferCircuit]
    exact h_constraints c2Out2Constraint h_mem

  -- Derive non-zero from each C2 constraint
  have h_c2_in1_eq : Fr.fromNat in1_val_fr.val * Fr.fromNat inv_in1.val = Fr.fromNat assignment.one.val := by
    simp only [constraintSatisfied] at h_c2_in1
    rw [h_eval_c2_in1_a, h_eval_c2_in1_b, h_eval_c2_in1_c] at h_c2_in1
    exact h_c2_in1

  have h_c2_in2_eq : Fr.fromNat in2_val_fr.val * Fr.fromNat inv_in2.val = Fr.fromNat assignment.one.val := by
    simp only [constraintSatisfied] at h_c2_in2
    rw [h_eval_c2_in2_a, h_eval_c2_in2_b, h_eval_c2_in2_c] at h_c2_in2
    exact h_c2_in2

  have h_c2_out1_eq : Fr.fromNat out1_val_fr.val * Fr.fromNat inv_out1.val = Fr.fromNat assignment.one.val := by
    simp only [constraintSatisfied] at h_c2_out1
    rw [h_eval_c2_out1_a, h_eval_c2_out1_b, h_eval_c2_out1_c] at h_c2_out1
    exact h_c2_out1

  have h_c2_out2_eq : Fr.fromNat out2_val_fr.val * Fr.fromNat inv_out2.val = Fr.fromNat assignment.one.val := by
    simp only [constraintSatisfied] at h_c2_out2
    rw [h_eval_c2_out2_a, h_eval_c2_out2_b, h_eval_c2_out2_c] at h_c2_out2
    exact h_c2_out2

  have h_rhs : Fr.fromNat assignment.one.val = 1 := by
    rw [h_one]
    rfl

  rw [h_rhs] at h_c2_in1_eq h_c2_in2_eq h_c2_out1_eq h_c2_out2_eq

  -- Convert to Fr products = 1
  have h_in1_mul : in1_val_fr * inv_in1 = 1 := by
    have h : Fr.fromNat in1_val_fr.val * Fr.fromNat inv_in1.val = Fr.fromNat (in1_val_fr.val * inv_in1.val) := by
      rw [← Fr.mul_fromNat in1_val_fr.val inv_in1.val]
    rw [h] at h_c2_in1_eq
    have h_val : in1_val_fr * inv_in1 = Fr.fromNat (in1_val_fr.val * inv_in1.val) := rfl
    rw [h_val]
    exact h_c2_in1_eq

  have h_in2_mul : in2_val_fr * inv_in2 = 1 := by
    have h : Fr.fromNat in2_val_fr.val * Fr.fromNat inv_in2.val = Fr.fromNat (in2_val_fr.val * inv_in2.val) := by
      rw [← Fr.mul_fromNat in2_val_fr.val inv_in2.val]
    rw [h] at h_c2_in2_eq
    have h_val : in2_val_fr * inv_in2 = Fr.fromNat (in2_val_fr.val * inv_in2.val) := rfl
    rw [h_val]
    exact h_c2_in2_eq

  have h_out1_mul : out1_val_fr * inv_out1 = 1 := by
    have h : Fr.fromNat out1_val_fr.val * Fr.fromNat inv_out1.val = Fr.fromNat (out1_val_fr.val * inv_out1.val) := by
      rw [← Fr.mul_fromNat out1_val_fr.val inv_out1.val]
    rw [h] at h_c2_out1_eq
    have h_val : out1_val_fr * inv_out1 = Fr.fromNat (out1_val_fr.val * inv_out1.val) := rfl
    rw [h_val]
    exact h_c2_out1_eq

  have h_out2_mul : out2_val_fr * inv_out2 = 1 := by
    have h : Fr.fromNat out2_val_fr.val * Fr.fromNat inv_out2.val = Fr.fromNat (out2_val_fr.val * inv_out2.val) := by
      rw [← Fr.mul_fromNat out2_val_fr.val inv_out2.val]
    rw [h] at h_c2_out2_eq
    have h_val : out2_val_fr * inv_out2 = Fr.fromNat (out2_val_fr.val * inv_out2.val) := rfl
    rw [h_val]
    exact h_c2_out2_eq

  -- Derive non-zero mod p from each product = 1
  have h_in1_nz : in1_val_fr.val % BN254_P ≠ 0 := Fr.nonzero_of_mul_eq_one in1_val_fr inv_in1 h_in1_mul
  have h_in2_nz : in2_val_fr.val % BN254_P ≠ 0 := Fr.nonzero_of_mul_eq_one in2_val_fr inv_in2 h_in2_mul
  have h_out1_nz : out1_val_fr.val % BN254_P ≠ 0 := Fr.nonzero_of_mul_eq_one out1_val_fr inv_out1 h_out1_mul
  have h_out2_nz : out2_val_fr.val % BN254_P ≠ 0 := Fr.nonzero_of_mul_eq_one out2_val_fr inv_out2 h_out2_mul

  -- Derive value > 0 from non-zero mod p
  have h_in1_pos : in1_val_nat > 0 := by
    have h_nz : in1_val_nat ≠ 0 := by
      intro h_zero
      have h_val_zero : in1_val_fr.val = 0 := by
        have h1 : in1_val_fr.val = in1_val_nat := rfl
        rw [h1, h_zero]
      rw [h_val_zero] at h_in1_nz
      simp at h_in1_nz
    exact Nat.pos_of_ne_zero h_nz

  have h_in2_pos : in2_val_nat > 0 := by
    have h_nz : in2_val_nat ≠ 0 := by
      intro h_zero
      have h_val_zero : in2_val_fr.val = 0 := by
        have h1 : in2_val_fr.val = in2_val_nat := rfl
        rw [h1, h_zero]
      rw [h_val_zero] at h_in2_nz
      simp at h_in2_nz
    exact Nat.pos_of_ne_zero h_nz

  have h_out1_pos : out1_val_nat > 0 := by
    have h_nz : out1_val_nat ≠ 0 := by
      intro h_zero
      have h_val_zero : out1_val_fr.val = 0 := by
        have h1 : out1_val_fr.val = out1_val_nat := rfl
        rw [h1, h_zero]
      rw [h_val_zero] at h_out1_nz
      simp at h_out1_nz
    exact Nat.pos_of_ne_zero h_nz

  have h_out2_pos : out2_val_nat > 0 := by
    have h_nz : out2_val_nat ≠ 0 := by
      intro h_zero
      have h_val_zero : out2_val_fr.val = 0 := by
        have h1 : out2_val_fr.val = out2_val_nat := rfl
        rw [h1, h_zero]
      rw [h_val_zero] at h_out2_nz
      simp at h_out2_nz
    exact Nat.pos_of_ne_zero h_nz

  -- Extract semantic constraints
  simp [TransferCircuit, transferSemantic] at h_semantic
  rcases h_semantic with ⟨h_sem_n1, h_sem_n2, h_sem_m1, h_sem_m2, h_sem_s1, h_sem_s2,
    h_sem_vc, h_sem_c1, h_sem_c2, h_sem_r1, h_sem_r2, h_sem_r3, h_sem_r4⟩

  -- Derive range bounds
  have h_in1_range : in1_val_nat < 2 ^ 128 := h_sem_r1
  have h_in2_range : in2_val_nat < 2 ^ 128 := h_sem_r2
  have h_out1_range : out1_val_nat < 2 ^ 128 := h_sem_r3
  have h_out2_range : out2_val_nat < 2 ^ 128 := h_sem_r4

  -- Derive value < BN254_P from range bounds
  have h_in1_lt_p : in1_val_nat < BN254_P := nat_lt_128_to_lt_p h_in1_range
  have h_in2_lt_p : in2_val_nat < BN254_P := nat_lt_128_to_lt_p h_in2_range
  have h_out1_lt_p : out1_val_nat < BN254_P := nat_lt_128_to_lt_p h_out1_range
  have h_out2_lt_p : out2_val_nat < BN254_P := nat_lt_128_to_lt_p h_out2_range

  -- Prove Fr.fromNat value_nat = value_fr (since value < BN254_P)
  have h_in1_val_eq : Fr.fromNat in1_val_nat = in1_val_fr := by
    have h1 : in1_val_nat = in1_val_fr.val := rfl
    rw [h1]
    exact Fr.fromNat_eq_of_lt in1_val_fr h_in1_lt_p

  have h_in2_val_eq : Fr.fromNat in2_val_nat = in2_val_fr := by
    have h1 : in2_val_nat = in2_val_fr.val := rfl
    rw [h1]
    exact Fr.fromNat_eq_of_lt in2_val_fr h_in2_lt_p

  have h_out1_val_eq : Fr.fromNat out1_val_nat = out1_val_fr := by
    have h1 : out1_val_nat = out1_val_fr.val := rfl
    rw [h1]
    exact Fr.fromNat_eq_of_lt out1_val_fr h_out1_lt_p

  have h_out2_val_eq : Fr.fromNat out2_val_nat = out2_val_fr := by
    have h1 : out2_val_nat = out2_val_fr.val := rfl
    rw [h1]
    exact Fr.fromNat_eq_of_lt out2_val_fr h_out2_lt_p

  -- Derive inverse equalities
  have h_inv_in1_eq : inv_in1 = Fr.inv (Fr.fromNat in1_val_nat) := by
    have h : Fr.inv (Fr.fromNat in1_val_nat) = Fr.inv in1_val_fr := by rw [h_in1_val_eq]
    rw [h]
    exact Fr.inv_unique in1_val_fr inv_in1 h_in1_nz h_in1_mul

  have h_inv_in2_eq : inv_in2 = Fr.inv (Fr.fromNat in2_val_nat) := by
    have h : Fr.inv (Fr.fromNat in2_val_nat) = Fr.inv in2_val_fr := by rw [h_in2_val_eq]
    rw [h]
    exact Fr.inv_unique in2_val_fr inv_in2 h_in2_nz h_in2_mul

  have h_inv_out1_eq : inv_out1 = Fr.inv (Fr.fromNat out1_val_nat) := by
    have h : Fr.inv (Fr.fromNat out1_val_nat) = Fr.inv out1_val_fr := by rw [h_out1_val_eq]
    rw [h]
    exact Fr.inv_unique out1_val_fr inv_out1 h_out1_nz h_out1_mul

  have h_inv_out2_eq : inv_out2 = Fr.inv (Fr.fromNat out2_val_nat) := by
    have h : Fr.inv (Fr.fromNat out2_val_nat) = Fr.inv out2_val_fr := by rw [h_out2_val_eq]
    rw [h]
    exact Fr.inv_unique out2_val_fr inv_out2 h_out2_nz h_out2_mul

  -- Public input equalities with witness elements
  have h_w0 : w0 = 1 := by
    have h : assignment.one = w0 := by
      simp [Assignment.one]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_one

  have h_w1 : w1 = nullifiers.1 := by
    have h : assignment.public 0 (by decide) = w1 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub0

  have h_w2 : w2 = nullifiers.2 := by
    have h : assignment.public 1 (by decide) = w2 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub1

  have h_w3 : w3 = commitments.1 := by
    have h : assignment.public 2 (by decide) = w3 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub2

  have h_w4 : w4 = commitments.2 := by
    have h : assignment.public 3 (by decide) = w4 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub3

  have h_w5 : w5 = asset_id := by
    have h : assignment.public 4 (by decide) = w5 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub4

  have h_w6 : w6 = merkle_root := by
    have h : assignment.public 5 (by decide) = w6 := by
      simp [Assignment.public]
      rw [hw_eq]
      rfl
    rw [←h]
    exact h_pub5

  have h_w7 : w7 = Fr.fromNat in1_val_nat := by
    have h : in1_val_fr = w7 := h_in1_val
    rw [←h]
    exact h_in1_val_eq.symm

  have h_w8 : w8 = in1_rcm := by
    have h : in1_rcm = w8 := h_in1_rcm
    exact h.symm

  have h_w9 : w9 = in1_ivk := by
    have h : in1_ivk = w9 := h_in1_ivk
    exact h.symm

  have h_w10 : w10 = in1_rho := by
    have h : in1_rho = w10 := h_in1_rho
    exact h.symm

  have h_w11 : w11 = in1_sk := by
    have h : in1_sk = w11 := h_in1_sk
    exact h.symm

  have h_w12 : w12 = Fr.inv (Fr.fromNat in1_val_nat) := by
    have h : inv_in1 = w12 := h_inv_in1
    rw [←h]
    exact h_inv_in1_eq

  have h_w13 : w13 = Fr.fromNat in2_val_nat := by
    have h : in2_val_fr = w13 := h_in2_val
    rw [←h]
    exact h_in2_val_eq.symm

  have h_w14 : w14 = in2_rcm := by
    have h : in2_rcm = w14 := h_in2_rcm
    exact h.symm

  have h_w15 : w15 = in2_ivk := by
    have h : in2_ivk = w15 := h_in2_ivk
    exact h.symm

  have h_w16 : w16 = in2_rho := by
    have h : in2_rho = w16 := h_in2_rho
    exact h.symm

  have h_w17 : w17 = in2_sk := by
    have h : in2_sk = w17 := h_in2_sk
    exact h.symm

  have h_w18 : w18 = Fr.inv (Fr.fromNat in2_val_nat) := by
    have h : inv_in2 = w18 := h_inv_in2
    rw [←h]
    exact h_inv_in2_eq

  have h_w19 : w19 = Fr.fromNat out1_val_nat := by
    have h : out1_val_fr = w19 := h_out1_val
    rw [←h]
    exact h_out1_val_eq.symm

  have h_w20 : w20 = out1_rcm := by
    have h : out1_rcm = w20 := h_out1_rcm
    exact h.symm

  have h_w21 : w21 = out1_rho := by
    have h : out1_rho = w21 := h_out1_rho
    exact h.symm

  have h_w22 : w22 = Fr.inv (Fr.fromNat out1_val_nat) := by
    have h : inv_out1 = w22 := h_inv_out1
    rw [←h]
    exact h_inv_out1_eq

  have h_w23 : w23 = Fr.fromNat out2_val_nat := by
    have h : out2_val_fr = w23 := h_out2_val
    rw [←h]
    exact h_out2_val_eq.symm

  have h_w24 : w24 = out2_rcm := by
    have h : out2_rcm = w24 := h_out2_rcm
    exact h.symm

  have h_w25 : w25 = out2_rho := by
    have h : out2_rho = w25 := h_out2_rho
    exact h.symm

  have h_w26 : w26 = Fr.inv (Fr.fromNat out2_val_nat) := by
    have h : inv_out2 = w26 := h_inv_out2
    rw [←h]
    exact h_inv_out2_eq

  -- Construct the witness
  let input1 : InputNoteWitness := ⟨in1_val_nat, in1_rcm, in1_ivk, in1_rho, in1_sk, merkle_path1, h_in1_pos, h_in1_range⟩
  let input2 : InputNoteWitness := ⟨in2_val_nat, in2_rcm, in2_ivk, in2_rho, in2_sk, merkle_path2, h_in2_pos, h_in2_range⟩
  let output1 : OutputNoteWitness := ⟨out1_val_nat, out1_rcm, out1_rho, h_out1_pos, h_out1_range⟩
  let output2 : OutputNoteWitness := ⟨out2_val_nat, out2_rcm, out2_rho, h_out2_pos, h_out2_range⟩
  let w : TransferWitness := ⟨input1, input2, output1, output2⟩

  refine ⟨w, ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩, ?_⟩
  · -- T1a: nullifier1
    exact h_sem_n1
  · -- T1b: nullifier2
    exact h_sem_n2
  · -- T2a: Merkle path input1
    exact h_sem_m1
  · -- T2b: Merkle path input2
    exact h_sem_m2
  · -- T3a: spending rights input1
    exact h_sem_s1
  · -- T3b: spending rights input2
    exact h_sem_s2
  · -- T4: value conservation
    exact h_sem_vc
  · -- T5a: output commitment 1
    exact h_sem_c1
  · -- T5b: output commitment 2
    exact h_sem_c2
  · -- Encoding equality
    have h_list :
        [1, nullifiers.1, nullifiers.2, commitments.1, commitments.2, asset_id, merkle_root,
         Fr.fromNat in1_val_nat, in1_rcm, in1_ivk, in1_rho, in1_sk, Fr.inv (Fr.fromNat in1_val_nat),
         Fr.fromNat in2_val_nat, in2_rcm, in2_ivk, in2_rho, in2_sk, Fr.inv (Fr.fromNat in2_val_nat),
         Fr.fromNat out1_val_nat, out1_rcm, out1_rho, Fr.inv (Fr.fromNat out1_val_nat),
         Fr.fromNat out2_val_nat, out2_rcm, out2_rho, Fr.inv (Fr.fromNat out2_val_nat)]
        = [w0, w1, w2, w3, w4, w5, w6, w7, w8, w9, w10, w11, w12, w13, w14, w15, w16, w17, w18, w19, w20, w21, w22, w23, w24, w25, w26] := by
      rw [h_w0, h_w1, h_w2, h_w3, h_w4, h_w5, h_w6, h_w7, h_w8, h_w9, h_w10, h_w11, h_w12,
          h_w13, h_w14, h_w15, h_w16, h_w17, h_w18, h_w19, h_w20, h_w21, h_w22, h_w23, h_w24, h_w25, h_w26]
    generalize h_enc : encodeTransfer w nullifiers commitments asset_id merkle_root = enc
    cases enc with | mk enc_vals enc_h =>
    cases assignment with | mk vals h_size =>
    simp at hw_eq
    have h_vals : enc_vals = vals := by
      have h1 : (encodeTransfer w nullifiers commitments asset_id merkle_root).values = enc_vals := by
        rw [h_enc]
      have h2 : (encodeTransfer w nullifiers commitments asset_id merkle_root).values = vals := by
        simp [encodeTransfer, encodeTransferWitness]
        rw [hw_eq]
        exact h_list
      rw [←h1]
      exact h2
    simp [h_vals]

end CallchainShielded
