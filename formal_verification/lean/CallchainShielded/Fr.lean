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

end CallchainShielded
