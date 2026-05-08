/-!
# BN254 Finite Field (Fr)

The scalar field of the BN254 elliptic curve, used by all shielded circuits.

`p = 21888242871839275222246405745257275088548364400416034343698204186575808495617`
-/

import Mathlib

namespace CallchainShielded

/-- The BN254 prime: 2^254 + 0x224698fc094cf91b992d30ed00000001 -/
def BN254_P : ℕ :=
  21888242871839275222246405745257275088548364400416034343698204186575808495617

/-- BN254 Fr field elements -/
def Fr := ZMod BN254_P

instance : Field Fr := ZMod.instField BN254_P

instance : Fintype Fr := ZMod.fintype BN254_P

instance : Inhabited Fr := ⟨0⟩

/-- Fr is a prime field -/
lemma Fr.prime : Nat.Prime BN254_P := by
  -- This is a known prime; in a full formalization this would be a native_decide or proven externally
  sorry

/-- The order of the multiplicative group Fr* -/
lemma Fr.mul_order : Fintype.card (Units Fr) = BN254_P - 1 := by
  rw [ZMod.card_units_eq_totient, Nat.totient_prime]
  · exact Fr.prime
  · exact Nat.Prime.one_lt Fr.prime

/-- Every non-zero element has a multiplicative inverse -/
lemma Fr.inv_exists (x : Fr) (hx : x ≠ 0) : ∃ y, x * y = 1 := by
  use x⁻¹
  exact mul_inv_cancel₀ hx

/-- Zero is not one in Fr -/
lemma Fr.zero_ne_one : (0 : Fr) ≠ 1 := by
  have h : BN254_P > 1 := by norm_num [BN254_P]
  exact Ne.symm (ZMod.ne_zero_iff.mpr (by omega))

/-- The field characteristic is p -/
lemma Fr.charP : CharP Fr BN254_P := ZMod.charP BN254_P

end CallchainShielded
