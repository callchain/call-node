import Mathlib.Data.ZMod.Basic

-- BN254 Finite Field (Fr) — Mathlib4 ZMod Integration
-- Uses Mathlib.Data.ZMod.Basic to define Fr as ZMod BN254_P.
-- This provides the Field instance, eliminating the two field axioms
-- (inv_mul, mul_cancel_left) in favor of a single primality axiom.

namespace CallchainShielded

/-- The BN254 prime -/
def BN254_P : Nat :=
  21888242871839275222246405745257275088548364400416034343698204186575808495617

/-- BN254_P is prime. Well-established cryptographic constant.
    Can be proven via native_decide when computation resources allow. -/
axiom BN254_P_prime : Nat.Prime BN254_P

instance : Fact (Nat.Prime BN254_P) := ⟨BN254_P_prime⟩
instance : Fact (1 < BN254_P) := ⟨Nat.Prime.one_lt BN254_P_prime⟩

/-- BN254 Fr field elements via mathlib ZMod -/
abbrev Fr := ZMod BN254_P

/-- Field instance for Fr -/
instance : Field Fr := ZMod.instField BN254_P

/-- Create Fr from natural number (same as nat cast) -/
def Fr.fromNat (n : Nat) : Fr := (n : Fr)

/-- Multiplicative inverse -/
def Fr.inv (a : Fr) : Fr := a⁻¹

-- ============================================================================
-- Cast and equality lemmas
-- ============================================================================

/-- Injectivity of nat cast into Fr modulo p -/
theorem Fr.fromNat_eq_iff (a b : Nat) : (a : Fr) = (b : Fr) ↔ a % BN254_P = b % BN254_P := by
  constructor
  · intro h
    have h1 : (a : Fr).val = (b : Fr).val := by rw [h]
    simp [ZMod.val_natCast] at h1
    exact h1
  · intro h
    apply ZMod.val_injective BN254_P
    simp [ZMod.val_natCast, h]

@[simp]
theorem Fr.fromNat_zero : Fr.fromNat 0 = 0 := rfl

@[simp]
theorem Fr.fromNat_one : Fr.fromNat 1 = 1 := rfl

@[simp]
theorem Fr.val_zero : (0 : Fr).val = 0 := ZMod.val_zero

@[simp]
theorem Fr.val_one : (1 : Fr).val = 1 := ZMod.val_one BN254_P

@[simp]
theorem Fr.fromNat_val (n : Nat) : Fr.fromNat (Fr.fromNat n).val = Fr.fromNat n := by
  simp [Fr.fromNat, ZMod.val_natCast]

/-- Fr.fromNat a.val = a for any a : Fr.
    Always true for ZMod since all values are canonical (< BN254_P). -/
@[simp]
theorem Fr.fromNat_eq_val (a : Fr) : Fr.fromNat a.val = a := by
  simp [Fr.fromNat]

/-- Nat multiplication distributes through fromNat -/
@[simp]
theorem Fr.mul_fromNat (a b : Nat) : Fr.fromNat (a * b) = Fr.fromNat a * Fr.fromNat b := by
  simp [Fr.fromNat]

/-- Nat addition distributes through fromNat -/
@[simp]
theorem Fr.add_fromNat (a b : Nat) : Fr.fromNat (a + b) = Fr.fromNat a + Fr.fromNat b := by
  simp [Fr.fromNat]

-- ============================================================================
-- Field arithmetic simplification lemmas
-- ============================================================================

@[simp]
theorem Fr.zero_mul (x : Fr) : (0 : Fr) * x = 0 := by
  simp

@[simp]
theorem Fr.mul_zero (x : Fr) : x * (0 : Fr) = 0 := by
  simp

@[simp]
theorem Fr.zero_add' (x : Fr) : (0 : Fr) + x = x := by
  simp

@[simp]
theorem Fr.add_zero' (x : Fr) : x + (0 : Fr) = x := by
  simp

@[simp]
theorem Fr.one_mul' (x : Fr) : (1 : Fr) * x = x := by
  simp

@[simp]
theorem Fr.mul_one' (x : Fr) : x * (1 : Fr) = x := by
  simp

@[simp]
theorem Fr.fromNat_zero_add (x : Fr) : Fr.fromNat (0 + x.val) = x := by
  rw [Nat.zero_add]
  simp [Fr.fromNat]

@[simp]
theorem Fr.fromNat_add_zero (x : Fr) : Fr.fromNat (x.val + 0) = x := by
  rw [Nat.add_zero]
  simp [Fr.fromNat]

@[simp]
theorem Fr.fromNat_zero_mul (x : Fr) : Fr.fromNat (0 * x.val) = Fr.fromNat 0 := by
  rw [Nat.zero_mul]

@[simp]
theorem Fr.fromNat_mul_zero (x : Fr) : Fr.fromNat (x.val * 0) = Fr.fromNat 0 := by
  rw [Nat.mul_zero]

@[simp]
theorem Fr.fromNat_one_mul (x : Fr) : Fr.fromNat (1 * x.val) = Fr.fromNat x.val := by
  rw [Nat.one_mul]

@[simp]
theorem Fr.fromNat_mul_one (x : Fr) : Fr.fromNat (x.val * 1) = Fr.fromNat x.val := by
  rw [Nat.mul_one]

@[simp]
theorem Fr.fromNat_mod_eq (n : Nat) : Fr.fromNat (n % BN254_P) = Fr.fromNat n := by
  simp [Fr.fromNat]

@[simp]
theorem Fr.add_fromNat_val (a b : Fr) : Fr.fromNat (a.val + b.val) = a + b := by
  simp [Fr.fromNat]

@[simp]
theorem Fr.mul_fromNat_val (a b : Fr) : Fr.fromNat (a.val * b.val) = a * b := by
  simp [Fr.fromNat]

-- ============================================================================
-- Field inverse and cancellation (theorems, not axioms)
-- ============================================================================

/-- Multiplicative inverse: a * a⁻¹ = 1 for a ≠ 0.
    Proven from ZMod field structure + primality of BN254_P. -/
theorem Fr.inv_mul (a : Fr) (ha : a.val % BN254_P ≠ 0) : a * Fr.inv a = 1 := by
  have h_ne : a ≠ 0 := by
    intro h
    rw [h] at ha
    simp at ha
  have h2 : a * a⁻¹ = 1 := by
    have h_unit : IsUnit a := by
      have h1 : a = (a.val : Fr) := by
        simp [Fr.fromNat]
      rw [h1]
      rw [ZMod.isUnit_iff_coprime a.val BN254_P]
      have h3 : a.val ≠ 0 := by
        intro h
        rw [h] at ha
        simp at ha
      have h4 : a.val < BN254_P := ZMod.val_lt a
      have h5 : Nat.Coprime a.val BN254_P := by
        rw [Nat.coprime_iff_gcd_eq_one]
        have h6 : a.val.gcd BN254_P = 1 := by
          have h7 : a.val.gcd BN254_P ∣ BN254_P := Nat.gcd_dvd_right a.val BN254_P
          have h8 : a.val.gcd BN254_P ∣ a.val := Nat.gcd_dvd_left a.val BN254_P
          have h9 : a.val.gcd BN254_P = 1 ∨ a.val.gcd BN254_P = BN254_P := by
            apply (Nat.dvd_prime BN254_P_prime).mp
            exact h7
          cases h9 with
          | inl h => exact h
          | inr h =>
            have h10 : BN254_P ∣ a.val := by
              rw [h] at h8
              exact h8
            have h11 : a.val < BN254_P := ZMod.val_lt a
            have h12 : a.val = 0 := by
              exact Nat.eq_zero_of_dvd_of_lt h10 h11
            contradiction
        exact h6
      exact h5
    exact ZMod.mul_inv_of_unit a h_unit
  exact h2

/-- Left cancellation in a field: if a ≠ 0 and a * b = a * c, then b = c.
    Proven from Field properties via mathlib. -/
theorem Fr.mul_cancel_left (a b c : Fr) (ha : a.val % BN254_P ≠ 0) (h : a * b = a * c) : b = c := by
  have h_ne : a ≠ 0 := by
    intro h0
    rw [h0] at ha
    simp at ha
  exact (mul_right_inj' h_ne).mp h

/-- Uniqueness of multiplicative inverse: if a * b = 1, then b = a⁻¹. -/
theorem Fr.inv_unique (a b : Fr) (ha : a.val % BN254_P ≠ 0) (h : a * b = 1) : b = Fr.inv a := by
  have h_inv : a * Fr.inv a = 1 := Fr.inv_mul a ha
  have h_eq : a * b = a * Fr.inv a := by rw [h, h_inv]
  exact Fr.mul_cancel_left a b (Fr.inv a) ha h_eq

/-- If a * b = 1 in Fr, then a.val % p ≠ 0 -/
theorem Fr.nonzero_of_mul_eq_one (a b : Fr) (h : a * b = 1) : a.val % BN254_P ≠ 0 := by
  intro ha
  have h_mod : a.val % BN254_P = a.val := by
    rw [Nat.mod_eq_of_lt]
    exact ZMod.val_lt a
  have h_val_zero : a.val = 0 := by
    rw [←h_mod]
    exact ha
  have h0 : a = 0 := by
    rw [←ZMod.val_eq_zero]
    exact h_val_zero
  rw [h0] at h
  simp at h

end CallchainShielded
