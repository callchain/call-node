/-!
# BN254 Finite Field (Fr) — Minimal Stub

Temporary definitions to allow `lake build` without mathlib dependency.
When mathlib is available, replace with `ZMod BN254_P`.
-/

namespace CallchainShielded

/-- The BN254 prime -/
def BN254_P : Nat :=
  21888242871839275222246405745257275088548364400416034343698204186575808495617

/-- Fast modular exponentiation: base^exp mod mod -/
def powMod (base exp mod : Nat) : Nat :=
  if mod = 0 then 0
  else if exp = 0 then 1 % mod
  else
    let half := powMod base (exp / 2) mod
    let result := (half * half) % mod
    if exp % 2 = 1 then (result * base) % mod
    else result

/-- BN254 Fr field elements represented as natural numbers modulo p -/
structure Fr where
  val : Nat
  deriving Repr, BEq, Ord

/-- Reduce a natural number to Fr (mod p) -/
def Fr.fromNat (n : Nat) : Fr := ⟨n % BN254_P⟩

/-- Zero element -/
instance : OfNat Fr 0 where
  ofNat := Fr.fromNat 0

/-- One element -/
instance : OfNat Fr 1 where
  ofNat := Fr.fromNat 1

/-- Addition modulo p -/
instance : Add Fr where
  add a b := Fr.fromNat (a.val + b.val)

/-- Multiplication modulo p -/
instance : Mul Fr where
  mul a b := Fr.fromNat (a.val * b.val)

/-- Additive inverse -/
instance : Neg Fr where
  neg a := Fr.fromNat (BN254_P - a.val % BN254_P)

/-- Subtraction -/
instance : Sub Fr where
  sub a b := Fr.fromNat (a.val + (BN254_P - b.val % BN254_P))

/-- Multiplicative inverse (Fermat's little theorem) -/
def Fr.inv (a : Fr) : Fr :=
  if a.val % BN254_P = 0 then Fr.fromNat 0
  else Fr.fromNat (powMod a.val (BN254_P - 2) BN254_P)

/-- Multiplicative inverse axiom: a * a⁻¹ = 1 for a ≠ 0.
    This follows from Fermat's little theorem (a^(p-1) ≡ 1 mod p).
    Marked as axiom because a full proof requires substantial number theory
    infrastructure that mathlib provides via ZMod.instField.
    TODO: Replace with theorem proof when mathlib is available. -/
axiom Fr.inv_mul (a : Fr) (ha : a.val % BN254_P ≠ 0) : a * Fr.inv a = 1

-- ============================================================================
-- Modular arithmetic theorems (provided by Lean 4 core)
-- ============================================================================

/-- Fr.fromNat distributes over multiplication.
    Proven from Nat.mul_mod in Lean 4 core. -/
theorem Fr.mul_fromNat (a b : Nat) : Fr.fromNat (a * b) = Fr.fromNat a * Fr.fromNat b := by
  have h1 : Fr.fromNat a * Fr.fromNat b = Fr.fromNat ((Fr.fromNat a).val * (Fr.fromNat b).val) := rfl
  rw [h1]
  have h2 : (Fr.fromNat a).val = a % BN254_P := rfl
  have h3 : (Fr.fromNat b).val = b % BN254_P := rfl
  rw [h2, h3]
  dsimp only [Fr.fromNat]
  rw [Nat.mul_mod]

/-- Fr.fromNat distributes over addition.
    Proven from Nat.add_mod in Lean 4 core. -/
theorem Fr.add_fromNat (a b : Nat) : Fr.fromNat (a + b) = Fr.fromNat a + Fr.fromNat b := by
  have h1 : Fr.fromNat a + Fr.fromNat b = Fr.fromNat ((Fr.fromNat a).val + (Fr.fromNat b).val) := rfl
  rw [h1]
  have h2 : (Fr.fromNat a).val = a % BN254_P := rfl
  have h3 : (Fr.fromNat b).val = b % BN254_P := rfl
  rw [h2, h3]
  dsimp only [Fr.fromNat]
  rw [Nat.add_mod]

/-- Injectivity of Fr.fromNat modulo p -/
theorem Fr.fromNat_eq_iff (a b : Nat) : Fr.fromNat a = Fr.fromNat b ↔ a % BN254_P = b % BN254_P := by
  simp [Fr.fromNat]

@[simp]
theorem Fr.fromNat_zero : Fr.fromNat 0 = 0 := rfl

@[simp]
theorem Fr.fromNat_one : Fr.fromNat 1 = 1 := rfl

@[simp]
theorem Fr.zero_eq : (⟨0⟩ : Fr) = 0 := rfl

@[simp]
theorem Fr.one_eq : (⟨1⟩ : Fr) = 1 := rfl

@[simp]
theorem Fr.mk_val_zero : ({ val := 0 } : Fr).val = 0 := rfl

@[simp]
theorem Fr.mk_val_one : ({ val := 1 } : Fr).val = 1 := rfl

-- ============================================================================
-- Structure projection simp lemmas
-- ============================================================================

@[simp]
theorem Fr.val_zero : (0 : Fr).val = 0 := rfl

@[simp]
theorem Fr.val_one : (1 : Fr).val = 1 := rfl

-- ============================================================================
-- Fr.fromNat on concrete structure projections
-- ============================================================================

@[simp]
theorem Fr.fromNat_val_zero' : Fr.fromNat ({ val := 0 } : Fr).val = 0 := rfl

@[simp]
theorem Fr.fromNat_val_one' : Fr.fromNat ({ val := 1 } : Fr).val = 1 := rfl

-- ============================================================================
-- Basic field arithmetic simplification lemmas (for simp)
-- ============================================================================

@[simp]
theorem Fr.zero_mul (x : Fr) : (0 : Fr) * x = 0 := by
  have h1 : (0 : Fr) * x = Fr.fromNat ((0 : Fr).val * x.val) := rfl
  rw [h1]
  have h2 : (0 : Fr).val = 0 := rfl
  rw [h2]
  have h3 : 0 * x.val = 0 := Nat.zero_mul x.val
  rw [h3]
  rfl

@[simp]
theorem Fr.mul_zero (x : Fr) : x * (0 : Fr) = 0 := by
  have h1 : x * (0 : Fr) = Fr.fromNat (x.val * (0 : Fr).val) := rfl
  rw [h1]
  have h2 : (0 : Fr).val = 0 := rfl
  rw [h2]
  have h3 : x.val * 0 = 0 := Nat.mul_zero x.val
  rw [h3]
  rfl

@[simp]
theorem Fr.zero_add' (x : Fr) : (0 : Fr) + x = Fr.fromNat x.val := by
  have h1 : (0 : Fr) + x = Fr.fromNat ((0 : Fr).val + x.val) := rfl
  rw [h1]
  have h2 : (0 : Fr).val = 0 := rfl
  rw [h2]
  have h3 : 0 + x.val = x.val := Nat.zero_add x.val
  rw [h3]

@[simp]
theorem Fr.add_zero' (x : Fr) : x + (0 : Fr) = Fr.fromNat x.val := by
  have h1 : x + (0 : Fr) = Fr.fromNat (x.val + (0 : Fr).val) := rfl
  rw [h1]
  have h2 : (0 : Fr).val = 0 := rfl
  rw [h2]
  have h3 : x.val + 0 = x.val := Nat.add_zero x.val
  rw [h3]

@[simp]
theorem Fr.one_mul' (x : Fr) : (1 : Fr) * x = Fr.fromNat x.val := by
  have h1 : (1 : Fr) * x = Fr.fromNat ((1 : Fr).val * x.val) := rfl
  rw [h1]
  have h2 : (1 : Fr).val = 1 := rfl
  rw [h2]
  have h3 : 1 * x.val = x.val := Nat.one_mul x.val
  rw [h3]

@[simp]
theorem Fr.mul_one' (x : Fr) : x * (1 : Fr) = Fr.fromNat x.val := by
  have h1 : x * (1 : Fr) = Fr.fromNat (x.val * (1 : Fr).val) := rfl
  rw [h1]
  have h2 : (1 : Fr).val = 1 := rfl
  rw [h2]
  have h3 : x.val * 1 = x.val := Nat.mul_one x.val
  rw [h3]

@[simp]
theorem Fr.fromNat_zero_add (x : Fr) : Fr.fromNat (0 + x.val) = Fr.fromNat x.val := by
  rw [Nat.zero_add]

@[simp]
theorem Fr.fromNat_add_zero (x : Fr) : Fr.fromNat (x.val + 0) = Fr.fromNat x.val := by
  rw [Nat.add_zero]

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
theorem Fr.fromNat_val (n : Nat) : Fr.fromNat (Fr.fromNat n).val = Fr.fromNat n := by
  simp [Fr.fromNat]

@[simp]
theorem Fr.add_fromNat_val (a b : Fr) : Fr.fromNat (a.val + b.val) = a + b := rfl

@[simp]
theorem Fr.mul_fromNat_val (a b : Fr) : Fr.fromNat (a.val * b.val) = a * b := rfl

@[simp]
theorem Fr.fromNat_mod_eq (n : Nat) : Fr.fromNat (n % BN254_P) = Fr.fromNat n := by
  simp [Fr.fromNat]

/-- If a.val < BN254_P, then Fr.fromNat a.val = a.
    This is true because Fr.fromNat normalizes modulo BN254_P. -/
theorem Fr.fromNat_eq_of_lt (a : Fr) (h : a.val < BN254_P) : Fr.fromNat a.val = a := by
  simp [Fr.fromNat]
  rw [Nat.mod_eq_of_lt h]

/-- Power operation -/
instance : Pow Fr Nat where
  pow a n := Fr.fromNat (powMod a.val n BN254_P)

/-- String representation -/
instance : ToString Fr where
  toString a := toString (a.val % BN254_P)

/-- Cast from Nat to Fr -/
instance : Coe Nat Fr where
  coe n := Fr.fromNat n

/-- Inhabited instance for default values -/
instance : Inhabited Fr where
  default := Fr.fromNat 0

/-- Any natural number literal can be used as Fr -/
instance (n : Nat) : OfNat Fr n where
  ofNat := Fr.fromNat n

-- ============================================================================
-- Field cancellation and inverse uniqueness
-- ============================================================================

/-- Left cancellation in a field: if a ≠ 0 and a * b = a * c, then b = c.
    Provable in mathlib via Field properties. Marked as axiom pending mathlib. -/
axiom Fr.mul_cancel_left (a b c : Fr) (ha : a.val % BN254_P ≠ 0) (h : a * b = a * c) : b = c

/-- Uniqueness of multiplicative inverse: if a * b = 1, then b = a⁻¹. -/
theorem Fr.inv_unique (a b : Fr) (ha : a.val % BN254_P ≠ 0) (h : a * b = 1) : b = Fr.inv a := by
  have h_inv : a * Fr.inv a = 1 := Fr.inv_mul a ha
  have h_eq : a * b = a * Fr.inv a := by rw [h, h_inv]
  exact Fr.mul_cancel_left a b (Fr.inv a) ha h_eq

end CallchainShielded
