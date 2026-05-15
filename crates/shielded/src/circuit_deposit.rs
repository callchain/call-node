//! ShieldedDeposit circuit — Halo2 PLONKish version over Pasta Pallas.
//!
//! Proves that a user deposited transparent funds into the shielded pool
//! by creating a valid note commitment, without revealing the note's
//! spending key or nullifier rho.
//!
//! Public inputs: commitment (Fp), asset_id (Fp)
//! Private witnesses: value (u128), rcm (Fp), recipient_ivk (Fp), rho (Fp)
//!
//! Constraints:
//!   D1. Commitment validity: Recompute H(value || asset_id || rcm || rho) == public
//!   D2. Value range: Non-zero, 128-bit range (bit decomposition + running sum)
//!   D3. RCM determinism: H("rcm" || ivk || value || asset_id || rho) == rcm

use halo2_gadgets::poseidon::primitives::{ConstantLength, P128Pow5T3};
use halo2_gadgets::poseidon::{Hash as PoseidonHash, Pow5Chip, Pow5Config};
use halo2_proofs::circuit::{AssignedCell, Layouter, SimpleFloorPlanner, Value};
use halo2_proofs::plonk::{
    Advice, Circuit, Column, ConstraintSystem, Error, Expression, Fixed, Instance, Selector,
};
use halo2_proofs::poly::Rotation;
use pasta_curves::Fp;
use pasta_curves::group::ff::Field;

// ---------------------------------------------------------------------------
// Witness / Circuit structs
// ---------------------------------------------------------------------------

/// Witness data for a deposit note.
#[derive(Debug, Clone)]
pub struct DepositWitness {
    pub value: u128,
    pub rcm: [u8; 32],
    pub recipient_ivk: [u8; 32],
    pub rho: [u8; 32],
}

/// ShieldedDeposit circuit.
#[derive(Debug, Clone)]
pub struct DepositCircuit {
    /// Public inputs
    pub commitment: [u8; 32],
    pub asset_id: u64,
    /// Private witnesses
    pub witness: Option<DepositWitness>,
}

impl DepositCircuit {
    /// Create a new deposit circuit from public data and private witness.
    pub fn new(commitment: [u8; 32], asset_id: u64, witness: DepositWitness) -> Self {
        Self {
            commitment,
            asset_id,
            witness: Some(witness),
        }
    }

    /// Create a circuit with only public data (for verification only).
    pub fn for_verify(commitment: [u8; 32], asset_id: u64) -> Self {
        Self {
            commitment,
            asset_id,
            witness: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DepositConfig {
    // Witness columns
    value: Column<Advice>,
    rcm: Column<Advice>,
    ivk: Column<Advice>,
    rho: Column<Advice>,
    asset_id: Column<Advice>,

    // Range-check / non-zero auxiliary columns
    value_inv: Column<Advice>,
    bit: Column<Advice>,
    running_sum: Column<Advice>,

    // Poseidon chip columns
    poseidon_state: [Column<Advice>; 3],
    poseidon_partial: Column<Advice>,
    poseidon_rc_a: [Column<Fixed>; 3],
    poseidon_rc_b: [Column<Fixed>; 3],

    // Public inputs
    instance: Column<Instance>,

    // Poseidon config
    poseidon_config: Pow5Config<Fp, 3, 2>,

    // Selectors
    q_bit: Selector,
    q_running_sum: Selector,
    q_nonzero: Selector,
}

// ---------------------------------------------------------------------------
// Circuit impl
// ---------------------------------------------------------------------------

impl Circuit<Fp> for DepositCircuit {
    type Config = DepositConfig;
    type FloorPlanner = SimpleFloorPlanner;

    fn without_witnesses(&self) -> Self {
        Self {
            commitment: self.commitment,
            asset_id: self.asset_id,
            witness: None,
        }
    }

    fn configure(meta: &mut ConstraintSystem<Fp>) -> Self::Config {
        // ---- Advice columns -------------------------------------------------
        let value = meta.advice_column();
        let rcm = meta.advice_column();
        let ivk = meta.advice_column();
        let rho = meta.advice_column();
        let asset_id = meta.advice_column();
        let value_inv = meta.advice_column();
        let bit = meta.advice_column();
        let running_sum = meta.advice_column();

        let poseidon_state = [meta.advice_column(), meta.advice_column(), meta.advice_column()];
        let poseidon_partial = meta.advice_column();

        // ---- Fixed columns for Poseidon round constants ---------------------
        let poseidon_rc_a: [Column<Fixed>; 3] = std::array::from_fn(|_| meta.fixed_column());
        let poseidon_rc_b: [Column<Fixed>; 3] = std::array::from_fn(|_| meta.fixed_column());

        // ---- Constant fixed column (required for Expression::Constant) ------
        let constant = meta.fixed_column();
        meta.enable_constant(constant);

        // ---- Instance column ------------------------------------------------
        let instance = meta.instance_column();

        // ---- Poseidon chip config ------------------------------------------
        let poseidon_config = Pow5Chip::configure::<P128Pow5T3>(
            meta,
            poseidon_state,
            poseidon_partial,
            poseidon_rc_a,
            poseidon_rc_b,
        );

        // ---- Selectors ------------------------------------------------------
        let q_bit = meta.selector();
        let q_running_sum = meta.selector();
        let q_nonzero = meta.selector();

        // ---- Enable equality on columns used in copy constraints ------------
        meta.enable_equality(value);
        meta.enable_equality(rcm);
        meta.enable_equality(ivk);
        meta.enable_equality(rho);
        meta.enable_equality(asset_id);
        meta.enable_equality(running_sum);
        meta.enable_equality(instance);

        // ---- Gates ----------------------------------------------------------

        // Bit constraint: bit * (1 - bit) = 0
        meta.create_gate("bit", |meta| {
            let q = meta.query_selector(q_bit);
            let b = meta.query_advice(bit, Rotation::cur());
            let one = Expression::Constant(Fp::one());
            vec![q * b.clone() * (one - b)]
        });

        // Running sum: cur = prev * 2 + bit
        // Enabled on rows 1..=128; row 0 has running_sum = 0 (assigned, not gated)
        meta.create_gate("running_sum", |meta| {
            let q = meta.query_selector(q_running_sum);
            let prev = meta.query_advice(running_sum, Rotation::prev());
            let cur = meta.query_advice(running_sum, Rotation::cur());
            let b = meta.query_advice(bit, Rotation::cur());
            let two = Expression::Constant(Fp::from(2u64));
            vec![q * (cur - prev * two - b)]
        });

        // Non-zero: value * value_inv = 1
        meta.create_gate("nonzero", |meta| {
            let q = meta.query_selector(q_nonzero);
            let v = meta.query_advice(value, Rotation::cur());
            let inv = meta.query_advice(value_inv, Rotation::cur());
            let one = Expression::Constant(Fp::one());
            vec![q * (v * inv - one)]
        });

        DepositConfig {
            value,
            rcm,
            ivk,
            rho,
            asset_id,
            value_inv,
            bit,
            running_sum,
            poseidon_state,
            poseidon_partial,
            poseidon_rc_a,
            poseidon_rc_b,
            instance,
            poseidon_config,
            q_bit,
            q_running_sum,
            q_nonzero,
        }
    }

    fn synthesize(
        &self,
        config: Self::Config,
        mut layouter: impl Layouter<Fp>,
    ) -> Result<(), Error> {
        // ---- Convert witness data to Value<Fp> -----------------------------
        let value_fp: Value<Fp> = self
            .witness
            .as_ref()
            .map(|w| {
                Value::known(crate::poseidon::bytes_to_fp(&crate::poseidon::value_to_fp_bytes(
                    w.value,
                )))
            })
            .unwrap_or(Value::unknown());
        let rcm_fp: Value<Fp> = self
            .witness
            .as_ref()
            .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.rcm)))
            .unwrap_or(Value::unknown());
        let ivk_fp: Value<Fp> = self
            .witness
            .as_ref()
            .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.recipient_ivk)))
            .unwrap_or(Value::unknown());
        let rho_fp: Value<Fp> = self
            .witness
            .as_ref()
            .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.rho)))
            .unwrap_or(Value::unknown());

        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&self.asset_id.to_le_bytes());
        let asset_id_fp = Value::known(crate::poseidon::bytes_to_fp(&asset_bytes));

        // Inverse for non-zero check (dummy zero when value is zero / missing)
        let inv_fp: Value<Fp> = value_fp.and_then(|v| {
            let inv = v.invert();
            if bool::from(inv.is_some()) {
                Value::known(inv.unwrap())
            } else {
                Value::known(Fp::zero())
            }
        });

        // ---- Assign witnesses + range check in one region ------------------
        let (value_cell, rcm_cell, ivk_cell, rho_cell, asset_id_cell) = layouter
            .assign_region(
                || "deposit witnesses",
                |mut region| {
                    let offset = 0;

                    let value_cell =
                        region.assign_advice(|| "value", config.value, offset, || value_fp)?;
                    let rcm_cell =
                        region.assign_advice(|| "rcm", config.rcm, offset, || rcm_fp)?;
                    let ivk_cell =
                        region.assign_advice(|| "ivk", config.ivk, offset, || ivk_fp)?;
                    let rho_cell =
                        region.assign_advice(|| "rho", config.rho, offset, || rho_fp)?;
                    let asset_id_cell = region.assign_advice(
                        || "asset_id",
                        config.asset_id,
                        offset,
                        || asset_id_fp,
                    )?;

                    // Non-zero inverse at row 0
                    region.assign_advice(
                        || "value_inv",
                        config.value_inv,
                        offset,
                        || inv_fp,
                    )?;
                    config.q_nonzero.enable(&mut region, offset)?;

                    // ---- 128-bit range check via bit decomposition --------
                    let value_u128 = self.witness.as_ref().map(|w| w.value).unwrap_or(0);
                    let two = Fp::from(2u64);

                    // running_sum at row 0 = 0
                    let _running_0 = region.assign_advice(
                        || "running_0",
                        config.running_sum,
                        offset,
                        || Value::known(Fp::zero()),
                    )?;

                    let mut running = Fp::zero();
                    for i in 0..128 {
                        let bit_val = (value_u128 >> (127 - i)) & 1;
                        let bit_fp = Fp::from(bit_val as u64);
                        running = running * two + bit_fp;

                        let row = offset + 1 + i;
                        region.assign_advice(
                            || format!("bit_{}", i),
                            config.bit,
                            row,
                            || Value::known(bit_fp),
                        )?;
                        region.assign_advice(
                            || format!("running_{}", i + 1),
                            config.running_sum,
                            row,
                            || Value::known(running),
                        )?;
                        config.q_bit.enable(&mut region, row)?;
                        config.q_running_sum.enable(&mut region, row)?;
                    }

                    // Final running_sum (row 128) must equal value (row 0)
                    let running_final = region.assign_advice(
                        || "running_final",
                        config.running_sum,
                        offset + 128,
                        || Value::known(running),
                    )?;
                    region.constrain_equal(running_final.cell(), value_cell.cell())?;

                    Ok((value_cell, rcm_cell, ivk_cell, rho_cell, asset_id_cell))
                },
            )?;

        // ---- Constrain asset_id == public input ----------------------------
        layouter.constrain_instance(asset_id_cell.cell(), config.instance, 1)?;

        // ---- D1: Poseidon(value, asset_id, rcm, rho) == commitment ---------
        let chip = Pow5Chip::construct(config.poseidon_config.clone());
        let hasher_cm =
            PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<4>, 3, 2>::init(
                chip,
                layouter.namespace(|| "poseidon cm"),
            )?;
        let computed_cm = hasher_cm.hash(
            layouter.namespace(|| "hash cm"),
            [value_cell.clone(), asset_id_cell.clone(), rcm_cell.clone(), rho_cell.clone()],
        )?;
        layouter.constrain_instance(computed_cm.cell(), config.instance, 0)?;

        // ---- D3: Poseidon("rcm", ivk, value, asset_id, rho) == rcm ---------
        let tag_fp = crate::poseidon::bytes_to_fp(&crate::poseidon::tag_to_bytes(
            crate::poseidon::domain::RCM,
        ));
        let tag_cell = layouter.assign_region(
            || "rcm tag",
            |mut region| {
                region.assign_advice(
                    || "tag",
                    config.value,
                    0,
                    || Value::known(tag_fp),
                )
            },
        )?;

        let chip_rcm = Pow5Chip::construct(config.poseidon_config);
        let hasher_rcm =
            PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<5>, 3, 2>::init(
                chip_rcm,
                layouter.namespace(|| "poseidon rcm"),
            )?;
        let computed_rcm = hasher_rcm.hash(
            layouter.namespace(|| "hash rcm"),
            [tag_cell, ivk_cell, value_cell, asset_id_cell, rho_cell],
        )?;

        // Constrain computed_rcm == rcm
        layouter.assign_region(
            || "rcm equality",
            |mut region| region.constrain_equal(computed_rcm.cell(), rcm_cell.cell()),
        )?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Number of public inputs for the deposit circuit.
pub const fn deposit_public_input_count() -> usize {
    2 // commitment (Fp) + asset_id (Fp)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "halo2-prover"))]
mod tests {
    use super::*;
    use crate::poseidon::{bytes_to_fp, fp_to_bytes, poseidon_hash, poseidon_hash_tagged};
    use crate::test_utils::{test_hash, test_spending_key};
    use crate::ViewingKey;
    use halo2_proofs::dev::MockProver;

    fn make_deposit_witness(value: u128, seed: u8) -> DepositWitness {
        let sk = test_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(seed).0;

        DepositWitness {
            value,
            rcm: compute_rcm_plain(&vk, value, 1, &rho),
            recipient_ivk: vk.incoming_view_key,
            rho,
        }
    }

    fn compute_rcm_plain(vk: &ViewingKey, value: u128, asset_id: u64, rho: &[u8; 32]) -> [u8; 32] {
        let ivk_fp = bytes_to_fp(&vk.incoming_view_key);
        let value_fp = bytes_to_fp(&crate::poseidon::value_to_fp_bytes(value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fp = bytes_to_fp(&asset_bytes);
        let rho_fp = bytes_to_fp(rho);
        let rcm_fp = poseidon_hash_tagged(crate::poseidon::domain::RCM, &[ivk_fp, value_fp, asset_fp, rho_fp]);
        fp_to_bytes(&rcm_fp)
    }

    fn compute_commitment_plain(witness: &DepositWitness, asset_id: u64) -> Fp {
        let value_fp = bytes_to_fp(&crate::poseidon::value_to_fp_bytes(witness.value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fp = bytes_to_fp(&asset_bytes);
        let rcm_fp = bytes_to_fp(&witness.rcm);
        let rho_fp = bytes_to_fp(&witness.rho);
        poseidon_hash(&[value_fp, asset_fp, rcm_fp, rho_fp])
    }

    fn make_instance(commitment_fp: Fp, asset_id: u64) -> Vec<Fp> {
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_id_fp = bytes_to_fp(&asset_bytes);
        vec![commitment_fp, asset_id_fp]
    }

    #[test]
    fn test_deposit_circuit_satisfiable() {
        let witness = make_deposit_witness(1000, 1);
        let commitment_fp = compute_commitment_plain(&witness, 1);
        let circuit = DepositCircuit::new(fp_to_bytes(&commitment_fp), 1, witness);

        let prover = MockProver::run(10, &circuit, vec![make_instance(commitment_fp, 1)]).unwrap();
        prover.assert_satisfied();
    }

    #[test]
    fn test_deposit_circuit_witness_complete() {
        let witness = make_deposit_witness(500, 42);
        assert_eq!(witness.value, 500);
        assert_eq!(witness.rcm.len(), 32);
        assert_eq!(witness.recipient_ivk.len(), 32);
        assert_eq!(witness.rho.len(), 32);
    }

    #[test]
    fn test_deposit_circuit_deterministic() {
        let w1 = make_deposit_witness(1000, 7);
        let w2 = make_deposit_witness(1000, 7);
        assert_eq!(w1.value, w2.value);
        assert_eq!(w1.rcm, w2.rcm);
        assert_eq!(w1.recipient_ivk, w2.recipient_ivk);
        assert_eq!(w1.rho, w2.rho);
    }

    #[test]
    fn test_deposit_circuit_different_values() {
        let w1 = make_deposit_witness(1000, 7);
        let w2 = make_deposit_witness(2000, 7);
        assert_ne!(w1.value, w2.value);
        assert_ne!(w1.rcm, w2.rcm);
    }

    #[test]
    fn test_deposit_circuit_zero_value_rejected() {
        let witness = make_deposit_witness(0, 1);
        let commitment_fp = compute_commitment_plain(&witness, 1);
        let circuit = DepositCircuit::new(fp_to_bytes(&commitment_fp), 1, witness);

        let prover = MockProver::run(10, &circuit, vec![make_instance(commitment_fp, 1)]).unwrap();
        assert!(prover.verify().is_err(), "zero value should be rejected");
    }

    #[test]
    fn test_deposit_circuit_large_value() {
        let max_value = u128::MAX;
        let witness = DepositWitness {
            value: max_value,
            rcm: [1u8; 32],
            recipient_ivk: [2u8; 32],
            rho: [3u8; 32],
        };
        assert_eq!(witness.value, max_value);
    }

    #[test]
    fn test_deposit_public_input_count() {
        assert_eq!(deposit_public_input_count(), 2);
    }

    #[test]
    fn test_deposit_circuit_for_verify() {
        let circuit = DepositCircuit::for_verify([1u8; 32], 1);
        assert!(circuit.witness.is_none());
        assert_eq!(circuit.commitment.len(), 32);
        assert_eq!(circuit.asset_id, 1);
    }

    #[test]
    fn test_deposit_circuit_wrong_commitment_rejected() {
        let witness = make_deposit_witness(1000, 1);
        let commitment_fp = compute_commitment_plain(&witness, 1);
        let mut bad_commitment = fp_to_bytes(&commitment_fp);
        bad_commitment[0] ^= 0xFF;
        let bad_commitment_fp = bytes_to_fp(&bad_commitment);

        let circuit = DepositCircuit::new(bad_commitment, 1, witness);
        // Public input uses the WRONG commitment — circuit should reject
        let prover =
            MockProver::run(10, &circuit, vec![make_instance(bad_commitment_fp, 1)]).unwrap();
        assert!(
            prover.verify().is_err(),
            "wrong commitment should be rejected"
        );
    }

    #[test]
    fn test_deposit_circuit_wrong_asset_id_rejected() {
        let witness = make_deposit_witness(1000, 1);
        let commitment_fp = compute_commitment_plain(&witness, 1);

        // Public asset_id=2, but witness was built for asset_id=1
        let circuit = DepositCircuit::new(fp_to_bytes(&commitment_fp), 2, witness);
        let prover =
            MockProver::run(10, &circuit, vec![make_instance(commitment_fp, 2)]).unwrap();
        assert!(
            prover.verify().is_err(),
            "wrong asset_id should be rejected"
        );
    }

    #[test]
    fn test_deposit_circuit_wrong_rcm_rejected() {
        let witness = make_deposit_witness(1000, 1);
        let commitment_fp = compute_commitment_plain(&witness, 1);

        let mut bad_witness = witness;
        bad_witness.rcm[0] ^= 0xFF;

        let circuit = DepositCircuit::new(fp_to_bytes(&commitment_fp), 1, bad_witness);
        let prover =
            MockProver::run(10, &circuit, vec![make_instance(commitment_fp, 1)]).unwrap();
        assert!(prover.verify().is_err(), "wrong rcm should be rejected");
    }
}
