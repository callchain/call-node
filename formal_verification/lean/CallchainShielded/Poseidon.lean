import CallchainShielded.Fr
import CallchainShielded.PoseidonConstants

namespace CallchainShielded

/-- The S-box: x^5 -/
def sbox5 (x : Fr) : Fr := x ^ 5

/-- Apply S-box to all elements (full round) -/
def sboxFull (state : List Fr) : List Fr :=
  state.map sbox5

/-- Apply S-box to first element only (partial round) -/
def sboxPartial (state : List Fr) : List Fr :=
  match state with
  | [] => []
  | x :: xs => sbox5 x :: xs

/-- Add round constants (Ark step) -/
def ark (state : List Fr) (round : Nat) (t : Nat)
    (rc : List Fr) : List Fr :=
  List.map (fun i => state[i]! + rc[round * t + i]!)
    (List.range t)

/-- MDS matrix multiplication (Mix step) -/
def mix (state : List Fr) (t : Nat) (mds : List (List Fr)) : List Fr :=
  List.map (fun i =>
    let row := List.map (fun j => mds[i]![j]!) (List.range t)
    let pairs := row.zip state
    pairs.foldl (fun acc (m, s) => acc + m * s) 0)
    (List.range t)

/-- Single Poseidon round -/
def poseidonRound (state : List Fr) (round : Nat) (t : Nat) (R_F R_P : Nat)
    (rc : List Fr) (mds : List (List Fr)) : List Fr :=
  let state := ark state round t rc
  let state := if round < R_F / 2 || round ≥ R_F / 2 + R_P then
    sboxFull state
  else
    sboxPartial state
  mix state t mds

/-- Poseidon permutation -/
def poseidonPerm (state : List Fr) (t : Nat) (R_F R_P : Nat)
    (rc : List Fr) (mds : List (List Fr)) : List Fr :=
  List.foldl (fun s r => poseidonRound s r t R_F R_P rc mds) state (List.range (R_F + R_P))

/-- Get round constants for a given state width t -/
def getRC (t : Nat) : List Fr :=
  POSEIDON_RC[t - 2]!

/-- Get MDS matrix for a given state width t -/
def getMDS (t : Nat) : List (List Fr) :=
  POSEIDON_MDS[t - 2]!

/-- Get partial rounds count for a given state width t -/
def getRP (t : Nat) : Nat :=
  POSEIDON_RP_LIST[t - 2]!

/-- Poseidon hash function
    inputs: 1 to 16 field elements
    state width t = len(inputs) + 1
    initial state: [0, input_1, input_2, ..., input_{t-1}] -/
def poseidonHash (inputs : List Fr) (h : inputs.length > 0 ∧ inputs.length ≤ 16) : Fr :=
  let t := inputs.length + 1
  let R_P := getRP t
  let rc := getRC t
  let mds := getMDS t
  let initialState : List Fr := 0 :: inputs
  let finalState := poseidonPerm initialState t POSEIDON_RF R_P rc mds
  finalState[0]!

/-- Domain-tagged Poseidon hash -/
def poseidonHashTagged (tag : String) (inputs : List Fr)
    (h : inputs.length > 0 ∧ inputs.length ≤ 15) : Fr :=
  let tagBytes := tag.toUTF8.toList
  let tagNats := tagBytes.map (fun b => b.toNat)
  let tagNat : Nat := tagNats.foldl (fun acc b => acc * 256 + b) 0
  let tagFr : Fr := tagNat
  poseidonHash (tagFr :: inputs)
    (by
      have h1 : (tagFr :: inputs).length > 0 := by simp
      have h2 : (tagFr :: inputs).length ≤ 16 := by
        simp
        omega
      exact ⟨h1, h2⟩)

end CallchainShielded
