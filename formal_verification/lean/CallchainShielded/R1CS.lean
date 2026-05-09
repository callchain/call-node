import CallchainShielded.Fr

namespace CallchainShielded

/-- A single R1CS constraint: (A · w) * (B · w) = (C · w)
    where w = [1, public_inputs..., private_witness...] -/
structure R1CSConstraint where
  a : List Fr  -- coefficients for A vector
  b : List Fr  -- coefficients for B vector
  c : List Fr  -- coefficients for C vector
  deriving Repr

/-- A witness assignment: public inputs followed by private witnesses -/
structure Assignment (nPublic nPrivate : Nat) where
  values : List Fr
  h_size : values.length = 1 + nPublic + nPrivate

/-- Get the constant 1 (always at index 0) -/
def Assignment.one {nPublic nPrivate : Nat} (a : Assignment nPublic nPrivate) : Fr :=
  a.values[0]!

/-- Get a public input by index (0-based, after the constant 1) -/
def Assignment.public {nPublic nPrivate : Nat} (a : Assignment nPublic nPrivate) (i : Nat)
    (hi : i < nPublic) : Fr :=
  a.values[1 + i]!

/-- Get a private witness by index (0-based, after public inputs) -/
def Assignment.private {nPublic nPrivate : Nat} (a : Assignment nPublic nPrivate) (i : Nat)
    (hi : i < nPrivate) : Fr :=
  a.values[1 + nPublic + i]!

/-- An R1CS instance with low-level constraints and high-level semantic assertions.
    The `constraints` are native R1CS gates (A·w)*(B·w)=(C·w).
    The `semantic` captures high-level properties (hash equalities, range bounds)
    that would be expanded into hundreds of R1CS gates in a full implementation. -/
structure R1CS (nPublic nPrivate : Nat) where
  constraints : List R1CSConstraint
  semantic : Assignment nPublic nPrivate → Prop

/-- Evaluate a linear combination: Σ coeff[i] * witness[i] -/
def evalLC (coeffs : List Fr) (witness : List Fr) : Fr :=
  match coeffs, witness with
  | [], _ => 0
  | _, [] => 0
  | c :: cs, w :: ws => c * w + evalLC cs ws

/-- Check if a single constraint is satisfied by a witness vector -/
def constraintSatisfied (c : R1CSConstraint) (witness : List Fr) : Prop :=
  evalLC c.a witness * evalLC c.b witness = evalLC c.c witness

/-- An assignment satisfies an R1CS if all low-level constraints AND high-level
    semantic assertions are satisfied. -/
def R1CS.satisfied {nPublic nPrivate : Nat} (r1cs : R1CS nPublic nPrivate)
    (assignment : Assignment nPublic nPrivate) : Prop :=
  let witness := assignment.values
  (∀ c ∈ r1cs.constraints, constraintSatisfied c witness) ∧ r1cs.semantic assignment

/-- Encode a deposit witness into an R1CS assignment
    Layout: [1, commitment, asset_id, value, rcm, ivk, rho, inv_val]
    public inputs: commitment (index 0), asset_id (index 1)
    private witnesses: value (index 0), rcm (index 1), ivk (index 2), rho (index 3), inv_val (index 4) -/
def encodeDepositWitness (commitment asset_id value rcm ivk rho inv_val : Fr) :
    Assignment 2 5 :=
  ⟨[1, commitment, asset_id, value, rcm, ivk, rho, inv_val], by simp⟩

end CallchainShielded
