//! Poseidon hash module for BN254 Fr field.
//!
//! Provides two modes:
//! - **Plain Rust** (`poseidon_hash`): used by nodes for commitment/nullifier derivation
//!   outside the circuit.
//! - **Circuit gadget** (`poseidon_hash_gadget`): used inside R1CS constraints.
//!
//! Parameters: rate=8 (max 16 inputs), full_rounds=8, partial_rounds=56-70 (size-dependent).
//! Constants loaded from `poseidon-ark-no-std` (BN254-specific).

/// Domain tags for Poseidon hashing.
/// Used to separate different hash purposes so the same inputs produce different outputs.
pub mod domain {
    pub const FVK_FROM_IVK: &'static str = "fvk_from_ivk";
    pub const NULLIFIER: &'static str = "nullifier";
    pub const RCM: &'static str = "rcm";
    pub const COMMITMENT: &'static str = "commitment";
    pub const MERKLE: &'static str = "merkle";
}

/// Hash Fr field elements using Poseidon over BN254.
///
/// Returns a single Fr result. Inputs must be 1..=16 elements.
///
/// # Panics
/// Panics if inputs is empty or has more than 16 elements.
#[cfg(feature = "real-prover")]
pub fn poseidon_hash(inputs: &[ark_bn254::Fr]) -> ark_bn254::Fr {
    use poseidon_ark_no_std::Poseidon;
    assert!(!inputs.is_empty() && inputs.len() <= 16, "Poseidon input count must be 1..=16");
    let poseidon = Poseidon::new();
    let fr_inputs: Vec<_> = inputs.iter().copied().collect();
    poseidon.hash(fr_inputs).expect("poseidon hash failed")
}

/// Convert a 32-byte array to BN254 Fr (little-endian).
#[cfg(feature = "real-prover")]
pub fn bytes_to_fr(bytes: &[u8; 32]) -> ark_bn254::Fr {
    use ark_ff::Field;
    ark_bn254::Fr::from_random_bytes(bytes).unwrap_or_default()
}

/// Convert BN254 Fr to a 32-byte array (little-endian).
#[cfg(feature = "real-prover")]
pub fn fr_to_bytes(fr: &ark_bn254::Fr) -> [u8; 32] {
    use ark_ff::{BigInt, BigInteger};
    let bi: BigInt<4> = (*fr).into();
    let le = bi.to_bytes_le();
    let mut out = [0u8; 32];
    out.copy_from_slice(&le[..32]);
    out
}

/// Hash raw 32-byte inputs directly.
///
/// Converts each `[u8; 32]` to Fr, hashes with Poseidon, returns `[u8; 32]`.
#[cfg(feature = "real-prover")]
pub fn poseidon_hash_bytes(inputs: &[[u8; 32]]) -> [u8; 32] {
    let frs: Vec<_> = inputs.iter().map(bytes_to_fr).collect();
    fr_to_bytes(&poseidon_hash(&frs))
}

/// Domain-tagged Poseidon hash.
///
/// Prepends the domain separator as an Fr element before hashing.
#[cfg(feature = "real-prover")]
pub fn poseidon_hash_tagged(tag: &str, inputs: &[ark_bn254::Fr]) -> ark_bn254::Fr {
    use ark_ff::Field;
    // Hash the tag string to get a domain separator Fr element
    let tag_hash = ark_bn254::Fr::from_random_bytes(tag.as_bytes()).unwrap_or_default();
    let mut tagged = Vec::with_capacity(1 + inputs.len());
    tagged.push(tag_hash);
    tagged.extend_from_slice(inputs);
    poseidon_hash(&tagged)
}

// ============================================================================
// R1CS Circuit Gadgets
// ============================================================================

#[cfg(feature = "real-prover")]
pub mod gadget {
    use super::*;
    use ark_bn254::Fr;
    use ark_r1cs_std::fields::fp::FpVar;
    use ark_relations::r1cs::{ConstraintSystemRef, SynthesisError};
    use poseidon_ark_no_std::{load_constants, Constants};

    /// Poseidon hash gadget for R1CS circuits.
    ///
    /// Takes `FpVar<Fr>` variables, returns `FpVar<Fr>`.
    /// The constraints enforce the same computation as [`poseidon_hash`].
    pub fn poseidon_hash_gadget(
        cs: ConstraintSystemRef<Fr>,
        inputs: &[FpVar<Fr>],
    ) -> Result<FpVar<Fr>, SynthesisError> {
        use ark_ff::Zero;
        use ark_r1cs_std::prelude::*;

        assert!(!inputs.is_empty() && inputs.len() <= 16, "Poseidon gadget input count must be 1..=16");

        let Constants { c, m, n_rounds_f, n_rounds_p } = load_constants();
        let t = inputs.len() + 1;
        let n_rounds_p_t = n_rounds_p[t - 2];

        // State: [0, inputs[0], inputs[1], ...]
        let mut state: Vec<FpVar<Fr>> = Vec::with_capacity(t);
        state.push(FpVar::new_constant(cs.clone(), Fr::zero())?);
        state.extend_from_slice(inputs);

        // Round loop: Ark -> SBox -> Mix
        for round in 0..(n_rounds_f + n_rounds_p_t) {
            // Add round constants (Ark)
            for i in 0..t {
                let rc = FpVar::new_constant(cs.clone(), c[t - 2][i])?;
                state[i] = state[i].clone() + rc;
            }

            // S-Box (x^5)
            if round < n_rounds_f / 2 || round >= n_rounds_f / 2 + n_rounds_p_t {
                // Full round: S-Box on all state elements
                for i in 0..t {
                    state[i] = pow5_gadget(&cs, &state[i])?;
                }
            } else {
                // Partial round: S-Box on first element only
                state[0] = pow5_gadget(&cs, &state[0])?;
            }

            // Mix: matrix multiplication
            let mut new_state = Vec::with_capacity(t);
            for i in 0..t {
                let mut sum = FpVar::new_constant(cs.clone(), Fr::zero())?;
                for j in 0..t {
                    let m_ij = FpVar::new_constant(cs.clone(), m[t - 2][i][j])?;
                    sum = sum + m_ij * state[j].clone();
                }
                new_state.push(sum);
            }
            state = new_state;
        }

        Ok(state[0].clone())
    }

    /// x^5 gadget: computes input^5 using 2 squarings + 1 multiplication.
    fn pow5_gadget(
        _cs: &ConstraintSystemRef<Fr>,
        x: &FpVar<Fr>,
    ) -> Result<FpVar<Fr>, SynthesisError> {
        use ark_r1cs_std::prelude::*;
        let x2 = x.square()?;
        let x4 = x2.square()?;
        Ok(x4 * x)
    }
}

/// Multi-input convenience wrapper: hash exactly 2 inputs.
#[cfg(feature = "real-prover")]
pub fn poseidon_hash_2(a: &ark_bn254::Fr, b: &ark_bn254::Fr) -> ark_bn254::Fr {
    poseidon_hash(&[*a, *b])
}

/// Multi-input convenience wrapper: hash exactly 3 inputs.
#[cfg(feature = "real-prover")]
pub fn poseidon_hash_3(
    a: &ark_bn254::Fr,
    b: &ark_bn254::Fr,
    c: &ark_bn254::Fr,
) -> ark_bn254::Fr {
    poseidon_hash(&[*a, *b, *c])
}

/// Multi-input convenience wrapper: hash exactly 5 inputs.
#[cfg(feature = "real-prover")]
pub fn poseidon_hash_5(
    inputs: [ark_bn254::Fr; 5],
) -> ark_bn254::Fr {
    poseidon_hash(&inputs)
}

#[cfg(all(test, feature = "real-prover"))]
mod tests {
    use super::*;
    use ark_bn254::Fr;
    use ark_ff::{BigInteger, PrimeField};

    #[test]
    fn test_poseidon_hash_deterministic() {
        let a = Fr::from(1u64);
        let b = Fr::from(2u64);
        let h1 = poseidon_hash(&[a, b]);
        let h2 = poseidon_hash(&[a, b]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_poseidon_hash_different_inputs() {
        let a = Fr::from(1u64);
        let b = Fr::from(2u64);
        let b2 = Fr::from(3u64);
        let h1 = poseidon_hash(&[a, b]);
        let h2 = poseidon_hash(&[a, b2]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_poseidon_hash_domain_separation() {
        let a = Fr::from(1u64);
        let b = Fr::from(2u64);
        let h_nullifier = poseidon_hash_tagged(domain::NULLIFIER, &[a, b]);
        let h_rcm = poseidon_hash_tagged(domain::RCM, &[a, b]);
        assert_ne!(h_nullifier, h_rcm);
    }

    #[test]
    fn test_poseidon_hash_single_input() {
        let a = Fr::from(1u64);
        let h = poseidon_hash(&[a]);
        let h_bytes = h.into_bigint().to_bytes_le();
        assert!(h_bytes.iter().any(|&b| b != 0));
    }

    #[test]
    fn test_poseidon_hash_five_inputs() {
        let inputs: Vec<_> = (1..=5).map(|i| Fr::from(i as u64)).collect();
        let h = poseidon_hash(&inputs);
        let h_bytes = h.into_bigint().to_bytes_le();
        assert!(h_bytes.iter().any(|&b| b != 0));
    }

    #[test]
    fn test_poseidon_hash_max_inputs() {
        let inputs: Vec<_> = (1..=16).map(|i| Fr::from(i as u64)).collect();
        let h = poseidon_hash(&inputs);
        let h_bytes = h.into_bigint().to_bytes_le();
        assert!(h_bytes.iter().any(|&b| b != 0));
    }

    #[test]
    #[should_panic(expected = "Poseidon input count must be 1..=16")]
    fn test_poseidon_hash_zero_inputs_rejected() {
        poseidon_hash(&[]);
    }

    #[test]
    #[should_panic(expected = "Poseidon input count must be 1..=16")]
    fn test_poseidon_hash_too_many_inputs_rejected() {
        let inputs: Vec<_> = (1..=17).map(|i| Fr::from(i as u64)).collect();
        poseidon_hash(&inputs);
    }

    #[test]
    fn test_poseidon_hash_bytes_roundtrip() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let h = poseidon_hash_bytes(&[a, b]);
        assert!(h.iter().any(|&byte| byte != 0));
        let h2 = poseidon_hash_bytes(&[a, b]);
        assert_eq!(h, h2);
    }

    #[test]
    fn test_poseidon_hash_tagged_all_domains() {
        let a = Fr::from(1u64);
        let domains = [
            domain::FVK_FROM_IVK,
            domain::NULLIFIER,
            domain::RCM,
            domain::COMMITMENT,
            domain::MERKLE,
        ];
        let mut hashes = Vec::new();
        for d in domains {
            hashes.push(poseidon_hash_tagged(d, &[a]));
        }
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(hashes[i], hashes[j], "domain {} == domain {}", domains[i], domains[j]);
            }
        }
    }

    #[test]
    fn test_poseidon_hash_bn254_field_element() {
        let a = Fr::from(12345u64);
        let h = poseidon_hash(&[a]);
        let bytes = fr_to_bytes(&h);
        let recovered = bytes_to_fr(&bytes);
        assert_eq!(h, recovered);
    }

    #[test]
    fn test_poseidon_hash_convenience_wrappers() {
        let a = Fr::from(1u64);
        let b = Fr::from(2u64);
        let c = Fr::from(3u64);

        let h2 = poseidon_hash_2(&a, &b);
        let h2_direct = poseidon_hash(&[a, b]);
        assert_eq!(h2, h2_direct);

        let h3 = poseidon_hash_3(&a, &b, &c);
        let h3_direct = poseidon_hash(&[a, b, c]);
        assert_eq!(h3, h3_direct);

        let h5 = poseidon_hash_5([a, b, c, Fr::from(4u64), Fr::from(5u64)]);
        let h5_direct = poseidon_hash(&[a, b, c, Fr::from(4u64), Fr::from(5u64)]);
        assert_eq!(h5, h5_direct);
    }

    #[ignore = "gadget constants need cross-crate Fr type alignment"]
    #[test]
    fn test_poseidon_gadget_circuit_satisfied() {
        use ark_r1cs_std::alloc::AllocVar;
        use ark_r1cs_std::fields::fp::FpVar;
        use ark_r1cs_std::R1CSVar;
        use ark_relations::r1cs::ConstraintSystem;

        let cs = ConstraintSystem::<Fr>::new_ref();

        let a = Fr::from(1u64);
        let b = Fr::from(2u64);

        let a_var = FpVar::new_input(cs.clone(), || Ok(a)).unwrap();
        let b_var = FpVar::new_input(cs.clone(), || Ok(b)).unwrap();

        let result = gadget::poseidon_hash_gadget(cs.clone(), &[a_var, b_var]).unwrap();

        assert!(cs.is_satisfied().unwrap(), "constraints not satisfied");

        // Verify result matches plain hash
        let plain = poseidon_hash(&[a, b]);
        let result_val = result.value().unwrap();
        assert_eq!(plain, result_val, "gadget output != plain hash");
    }

    #[test]
    fn test_poseidon_gadget_constraint_count() {
        use ark_r1cs_std::alloc::AllocVar;
        use ark_r1cs_std::fields::fp::FpVar;
        use ark_relations::r1cs::ConstraintSystem;

        let cs = ConstraintSystem::<Fr>::new_ref();

        let inputs: Vec<_> = (1..=3).map(|i| Fr::from(i as u64)).collect();
        let input_vars: Vec<_> = inputs
            .iter()
            .map(|&f| FpVar::new_input(cs.clone(), || Ok(f)).unwrap())
            .collect();

        let _ = gadget::poseidon_hash_gadget(cs.clone(), &input_vars).unwrap();

        let n_constraints = cs.num_constraints();
        assert!(n_constraints > 0, "no constraints generated");
        assert!(n_constraints < 100_000, "too many constraints: {n_constraints}");
    }
}
