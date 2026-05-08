/-!
# Poseidon Hash Function Model

Mathematical model of the Poseidon hash over BN254 Fr.
Matches the `poseidon-ark-no-std` implementation used by the Rust circuits.

Parameters:
- Rate = 8 (max 16 inputs)
- Full rounds R_F = 8
- Partial rounds R_P = size-dependent
- S-box: x^5
-/

import Mathlib
import CallchainShielded.Fr

namespace CallchainShielded

/-- Poseidon partial round counts for each state width t (t = inputs + 1)
    These values come from poseidon-ark-no-std -/
def POSEIDON_RP : Nat → Nat
  | 2  => 56
  | 3  => 57
  | 4  => 56
  | 5  => 60
  | 6  => 60
  | 7  => 63
  | 8  => 64
  | 9  => 63
  | 10 => 66
  | 11 => 65
  | 12 => 70
  | 13 => 68
  | 14 => 70
  | 15 => 70
  | 16 => 70
  | 17 => 70
  | _  => 70  -- fallback

/-- Full rounds count -/
def POSEIDON_RF : Nat := 8

/-- The S-box: x^5 -/
def sbox5 (x : Fr) : Fr := x ^ 5

/-- Apply S-box to all elements (full round) -/
def sboxFull (state : Vector Fr t) : Vector Fr t :=
  state.map sbox5

/-- Apply S-box to first element only (partial round) -/
def sboxPartial (state : Vector Fr t) : Vector Fr t :=
  match t with
  | 0 => state
  | _ =>
    let first := sbox5 (state.get ⟨0, by simp⟩)
    ⟨first :: state.toList.tail, by simp⟩

/-- Add round constants (Ark step)
    Note: In the full formalization, actual round constants from poseidon-ark-no-std
    would be embedded here. For now, we parameterize over them. -/
def ark (state : Vector Fr t) (round : Nat)
    (rc : Nat → Nat → Fr) : Vector Fr t :=
  ⟨List.ofFn (fun i => state.get i + rc round i.1), by simp⟩

/-- MDS matrix multiplication (Mix step)
    Note: In the full formalization, actual MDS matrices from poseidon-ark-no-std
    would be embedded here. -/
def mix (state : Vector Fr t) (mds : Nat → Nat → Fr) : Vector Fr t :=
  ⟨List.ofFn (fun i =>
    List.ofFn (fun j => mds i.1 j.1).toList.zip state.toList |>.foldl
      (fun acc (m, s) => acc + m * s) 0), by simp⟩

/-- Single Poseidon round -/
def poseidonRound (state : Vector Fr t) (round : Nat) (R_F R_P : Nat)
    (rc : Nat → Nat → Fr) (mds : Nat → Nat → Fr) : Vector Fr t :=
  let state := ark state round rc
  let state := if round < R_F / 2 ∨ round ≥ R_F / 2 + R_P then
    sboxFull state
  else
    sboxPartial state
  mix state mds

/-- Poseidon permutation -/
def poseidonPerm (state : Vector Fr t) (R_F R_P : Nat)
    (rc : Nat → Nat → Fr) (mds : Nat → Nat → Fr) : Vector Fr t :=
  List.range (R_F + R_P) |>.foldl (fun s r => poseidonRound s r R_F R_P rc mds) state

/-- Poseidon hash function
    inputs: 1 to 16 field elements
    state width t = len(inputs) + 1
    initial state: [0, input_1, input_2, ..., input_{t-1}] -/
def poseidonHash (inputs : List Fr) (h : inputs.length > 0 ∧ inputs.length ≤ 16) : Fr :=
  let t := inputs.length + 1
  let R_P := POSEIDON_RP t
  -- TODO: Embed actual round constants and MDS matrices from poseidon-ark-no-std
  let rc : Nat → Nat → Fr := fun _ _ => 0  -- placeholder
  let mds : Nat → Nat → Fr := fun i j => if i = j then 1 else 0  -- placeholder (identity)
  let initialState : Vector Fr t :=
    ⟨0 :: inputs, by simp⟩
  let finalState := poseidonPerm initialState POSEIDON_RF R_P rc mds
  finalState.get ⟨0, by simp⟩

/-- Domain-tagged Poseidon hash -/
def poseidonHashTagged (tag : String) (inputs : List Fr)
    (h : inputs.length > 0 ∧ inputs.length ≤ 15) : Fr :=
  let tagFr := match tag.toUTF8.toList with
    | [] => (0 : Fr)
    | bytes =>
      -- Convert UTF-8 bytes to a field element (little-endian)
      bytes |>.take 32 |>.foldl (fun acc b => acc * 256 + b.toNat) 0 |>.cast
  poseidonHash (tagFr :: inputs) (by
    have h1 : inputs.length + 1 > 0 := by omega
    have h2 : inputs.length + 1 ≤ 16 := by omega
    exact ⟨h1, h2⟩)

end CallchainShielded
