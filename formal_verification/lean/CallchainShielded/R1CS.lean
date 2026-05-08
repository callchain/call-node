/-!
# R1CS Constraint System

Generic Rank-1 Constraint System definitions used to model arkworks circuits.
-/

import Mathlib
import CallchainShielded.Fr

namespace CallchainShielded

/-- A single R1CS constraint: (A · w) * (B · w) = (C · w)
    where w = [1, public_inputs..., private_witness...] -/
structure R1CSConstraint where
  a : List Fr  -- coefficients for A vector
  b : List Fr  -- coefficients for B vector
  c : List Fr  -- coefficients for C vector
  deriving Repr

/-- An R1CS instance with public inputs and constraints -/
structure R1CS (nPublic nPrivate : ℕ) where
  constraints : List R1CSConstraint
  deriving Repr

/-- A witness assignment: public inputs followed by private witnesses -/
def Assignment (nPublic nPrivate : ℕ) :=
  Vector Fr (1 + nPublic + nPrivate)

/-- The constant 1 is always at index 0 -/
def Assignment.one {nPublic nPrivate : ℕ} (a : Assignment nPublic nPrivate) : Fr :=
  a.get ⟨0, by simp⟩

/-- Get a public input by index (0-based, after the constant 1) -/
def Assignment.public {nPublic nPrivate : ℕ} (a : Assignment nPublic nPrivate) (i : ℕ)
    (hi : i < nPublic) : Fr :=
  a.get ⟨1 + i, by omega⟩

/-- Get a private witness by index (0-based, after public inputs) -/
def Assignment.private {nPublic nPrivate : ℕ} (a : Assignment nPublic nPrivate) (i : ℕ)
    (hi : i < nPrivate) : Fr :=
  a.get ⟨1 + nPublic + i, by omega⟩

/-- Evaluate a linear combination: Σ coeff[i] * witness[i] -/
def evalLC (coeffs : List Fr) (witness : List Fr) : Fr :=
  (coeffs.zip witness).foldl (fun acc (c, w) => acc + c * w) 0

/-- Check if a single constraint is satisfied by a witness vector -/
def constraintSatisfied (c : R1CSConstraint) (witness : List Fr) : Prop :=
  evalLC c.a witness * evalLC c.b witness = evalLC c.c witness

/-- An assignment satisfies an R1CS if all constraints are satisfied -/
def R1CS.satisfied {nPublic nPrivate : ℕ} (r1cs : R1CS nPublic nPrivate)
    (assignment : Assignment nPublic nPrivate) : Prop :=
  let witness := 1 :: (List.ofFn (fun i => assignment.get ⟨1 + i.1, by omega⟩)) ++
                  (List.ofFn (fun i => assignment.get ⟨1 + nPublic + i.1, by omega⟩))
  ∀ c ∈ r1cs.constraints, constraintSatisfied c witness

/-- Encode a deposit witness into an R1CS assignment
    witness = [1, commitment, asset_id, value, rcm, ivk, rho]
    public inputs: commitment (index 0), asset_id (index 1)
    private witnesses: value (index 0), rcm (index 1), ivk (index 2), rho (index 3) -/
def encodeDepositWitness (commitment asset_id value rcm ivk rho : Fr) :
    Assignment 2 4 :=
  ⟨[1, commitment, asset_id, value, rcm, ivk, rho], by simp⟩

end CallchainShielded
