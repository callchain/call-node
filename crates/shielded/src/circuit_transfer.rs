//! ShieldedTransfer circuit — Halo2 PLONKish version over Pasta Pallas.
//!
//! Proves a user is transferring shielded funds to new shielded notes.
//! Fixed topology: 2 inputs, 2 outputs (covers ~90% of transfers).
//!
//! Public inputs: nullifier0 (Fp), nullifier1 (Fp),
//!                commitment0 (Fp), commitment1 (Fp),
//!                asset_id (Fp), merkle_root (Fp)
//! Private witnesses: per-note values, rcms, ivks, rhos, sks, merkle paths
//!
//! Constraints:
//!   T1. Nullifier per input: H(H("fvk_from_ivk" || ivk), rho) == public nullifier
//!   T2. Merkle path per input: note_commitment -> 32 levels -> root == public
//!   T3. Spending rights per input: ivk == H("call/shielded/ivk" || sk)
//!   T4. Value conservation: sum(input_values) == sum(output_values)
//!   T5. Range & non-zero per note (128-bit)

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

/// Private witness data for a single input (spent) note.
#[derive(Debug, Clone, Copy)]
pub struct InputNoteWitness {
    /// Note value
    pub value: u128,
    /// Random commitment material
    pub rcm: [u8; 32],
    /// Incoming viewing key of the recipient
    pub recipient_ivk: [u8; 32],
    /// Nullifier randomness (rho)
    pub rho: [u8; 32],
    /// Spending key that authorizes spending this note
    pub spending_key: [u8; 32],
}

/// Private witness data for a single output (created) note.
#[derive(Debug, Clone, Copy)]
pub struct OutputNoteWitness {
    /// Note value
    pub value: u128,
    /// Random commitment material
    pub rcm: [u8; 32],
    /// Incoming viewing key of the recipient
    pub recipient_ivk: [u8; 32],
    /// Nullifier randomness (rho) for future spends
    pub rho: [u8; 32],
}

/// ShieldedTransfer circuit (fixed 2-in, 2-out).
#[derive(Debug, Clone)]
pub struct TransferCircuit {
    /// Public inputs
    pub nullifiers: [[u8; 32]; 2],
    pub commitments: [[u8; 32]; 2],
    pub asset_id: u64,
    pub merkle_root: [u8; 32],
    /// Private witnesses
    pub input_notes: Option<[InputNoteWitness; 2]>,
    pub output_notes: Option<[OutputNoteWitness; 2]>,
    pub merkle_paths: Option<[[([u8; 32], bool); 32]; 2]>,
}

impl TransferCircuit {
    /// Create a new transfer circuit from public data and private witnesses.
    pub fn new(
        nullifiers: [[u8; 32]; 2],
        commitments: [[u8; 32]; 2],
        asset_id: u64,
        merkle_root: [u8; 32],
        input_notes: [InputNoteWitness; 2],
        output_notes: [OutputNoteWitness; 2],
        merkle_paths: [[([u8; 32], bool); 32]; 2],
    ) -> Self {
        Self {
            nullifiers,
            commitments,
            asset_id,
            merkle_root,
            input_notes: Some(input_notes),
            output_notes: Some(output_notes),
            merkle_paths: Some(merkle_paths),
        }
    }

    /// Create a circuit with only public data (for verification only).
    pub fn for_verify(
        nullifiers: [[u8; 32]; 2],
        commitments: [[u8; 32]; 2],
        asset_id: u64,
        merkle_root: [u8; 32],
    ) -> Self {
        Self {
            nullifiers,
            commitments,
            asset_id,
            merkle_root,
            input_notes: None,
            output_notes: None,
            merkle_paths: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct TransferConfig {
    // Witness columns
    value: Column<Advice>,
    rcm: Column<Advice>,
    ivk: Column<Advice>,
    rho: Column<Advice>,
    sk: Column<Advice>,
    asset_id: Column<Advice>,
    sibling: Column<Advice>,

    // Range-check / non-zero / swap auxiliary columns
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
    q_swap: Selector,
    q_conservation: Selector,
}

// ---------------------------------------------------------------------------
// Circuit impl
// ---------------------------------------------------------------------------

impl Circuit<Fp> for TransferCircuit {
    type Config = TransferConfig;
    type FloorPlanner = SimpleFloorPlanner;

    fn without_witnesses(&self) -> Self {
        Self {
            nullifiers: self.nullifiers,
            commitments: self.commitments,
            asset_id: self.asset_id,
            merkle_root: self.merkle_root,
            input_notes: None,
            output_notes: None,
            merkle_paths: None,
        }
    }

    fn configure(meta: &mut ConstraintSystem<Fp>) -> Self::Config {
        // ---- Advice columns -------------------------------------------------
        let value = meta.advice_column();
        let rcm = meta.advice_column();
        let ivk = meta.advice_column();
        let rho = meta.advice_column();
        let sk = meta.advice_column();
        let asset_id = meta.advice_column();
        let sibling = meta.advice_column();

        let value_inv = meta.advice_column();
        let bit = meta.advice_column();
        let running_sum = meta.advice_column();

        let poseidon_state = [meta.advice_column(), meta.advice_column(), meta.advice_column()];
        let poseidon_partial = meta.advice_column();

        // ---- Fixed columns for Poseidon round constants ---------------------
        let poseidon_rc_a: [Column<Fixed>; 3] = std::array::from_fn(|_| meta.fixed_column());
        let poseidon_rc_b: [Column<Fixed>; 3] = std::array::from_fn(|_| meta.fixed_column());

        // ---- Constant fixed column ------------------------------------------
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
        let q_swap = meta.selector();
        let q_conservation = meta.selector();

        // ---- Enable equality on columns used in copy constraints ------------
        meta.enable_equality(value);
        meta.enable_equality(rcm);
        meta.enable_equality(ivk);
        meta.enable_equality(rho);
        meta.enable_equality(sk);
        meta.enable_equality(asset_id);
        meta.enable_equality(sibling);
        meta.enable_equality(value_inv);
        meta.enable_equality(bit);
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

        // Swap gate for Merkle path ordering
        // Row 0: current (value_inv), sibling, is_right (bit)
        // Row 1: left (value_inv), right (bit)
        // left = current + is_right * (sibling - current)
        // right = sibling + is_right * (current - sibling)
        meta.create_gate("swap", |meta| {
            let q = meta.query_selector(q_swap);
            let left = meta.query_advice(value_inv, Rotation::cur());
            let right = meta.query_advice(bit, Rotation::cur());
            let current = meta.query_advice(value_inv, Rotation::prev());
            let sibling = meta.query_advice(sibling, Rotation::prev());
            let is_right = meta.query_advice(bit, Rotation::prev());
            let s = sibling.clone();
            let c = current.clone();
            let ir = is_right.clone();
            vec![
                q.clone() * (left - s.clone() - ir.clone() * (c.clone() - s.clone())),
                q * (right - c.clone() - ir * (s - c)),
            ]
        });

        // Value conservation: in0 + in1 - out0 - out1 = 0
        meta.create_gate("conservation", |meta| {
            let q = meta.query_selector(q_conservation);
            let in0 = meta.query_advice(value, Rotation::cur());
            let in1 = meta.query_advice(rcm, Rotation::cur());
            let out0 = meta.query_advice(ivk, Rotation::cur());
            let out1 = meta.query_advice(rho, Rotation::cur());
            vec![q * (in0 + in1 - out0 - out1)]
        });

        TransferConfig {
            value,
            rcm,
            ivk,
            rho,
            sk,
            asset_id,
            sibling,
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
            q_swap,
            q_conservation,
        }
    }

    fn synthesize(
        &self,
        config: Self::Config,
        mut layouter: impl Layouter<Fp>,
    ) -> Result<(), Error> {
        // ---- Shared asset_id ------------------------------------------------
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&self.asset_id.to_le_bytes());
        let asset_id_fp = Value::known(crate::poseidon::bytes_to_fp(&asset_bytes));

        let asset_id_cell = layouter.assign_region(
            || "asset_id",
            |mut region| {
                region.assign_advice(|| "asset_id", config.asset_id, 0, || asset_id_fp)
            },
        )?;

        // ---- Process input notes -------------------------------------------
        let mut input_value_cells: Vec<AssignedCell<Fp, Fp>> = Vec::with_capacity(2);
        let mut input_commitment_cells: Vec<AssignedCell<Fp, Fp>> = Vec::with_capacity(2);

        for i in 0..2 {
            let input = self.input_notes.as_ref().map(|n| n[i]);
            let _merkle_path = self.merkle_paths.as_ref().map(|p| p[i]);

            let (value_cell, rcm_cell, ivk_cell, rho_cell, sk_cell, _note_cm) =
                layouter.assign_region(
                    || format!("input note {}", i),
                    |mut region| {
                        let offset = 0;

                        let value_fp: Value<Fp> = input
                            .map(|w| {
                                Value::known(crate::poseidon::bytes_to_fp(
                                    &crate::poseidon::value_to_fp_bytes(w.value),
                                ))
                            })
                            .unwrap_or(Value::unknown());
                        let rcm_fp: Value<Fp> = input
                            .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.rcm)))
                            .unwrap_or(Value::unknown());
                        let ivk_fp: Value<Fp> = input
                            .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.recipient_ivk)))
                            .unwrap_or(Value::unknown());
                        let rho_fp: Value<Fp> = input
                            .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.rho)))
                            .unwrap_or(Value::unknown());
                        let sk_fp: Value<Fp> = input
                            .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.spending_key)))
                            .unwrap_or(Value::unknown());

                        let value_cell =
                            region.assign_advice(|| "value", config.value, offset, || value_fp)?;
                        let rcm_cell =
                            region.assign_advice(|| "rcm", config.rcm, offset, || rcm_fp)?;
                        let ivk_cell =
                            region.assign_advice(|| "ivk", config.ivk, offset, || ivk_fp)?;
                        let rho_cell =
                            region.assign_advice(|| "rho", config.rho, offset, || rho_fp)?;
                        let sk_cell =
                            region.assign_advice(|| "sk", config.sk, offset, || sk_fp)?;

                        // Non-zero inverse
                        let inv_fp: Value<Fp> = value_fp.and_then(|v| {
                            let inv = v.invert();
                            if bool::from(inv.is_some()) {
                                Value::known(inv.unwrap())
                            } else {
                                Value::known(Fp::zero())
                            }
                        });
                        region.assign_advice(
                            || "value_inv",
                            config.value_inv,
                            offset,
                            || inv_fp,
                        )?;
                        config.q_nonzero.enable(&mut region, offset)?;

                        // ---- 128-bit range check ------------------------
                        let value_u128 = input.map(|w| w.value).unwrap_or(0);
                        let two = Fp::from(2u64);

                        region.assign_advice(
                            || "running_0",
                            config.running_sum,
                            offset,
                            || Value::known(Fp::zero()),
                        )?;

                        let mut running = Fp::zero();
                        for j in 0..128 {
                            let bit_val = (value_u128 >> (127 - j)) & 1;
                            let bit_fp = Fp::from(bit_val as u64);
                            running = running * two + bit_fp;

                            let row = offset + 1 + j;
                            region.assign_advice(
                                || format!("bit_{}", j),
                                config.bit,
                                row,
                                || Value::known(bit_fp),
                            )?;
                            region.assign_advice(
                                || format!("running_{}", j + 1),
                                config.running_sum,
                                row,
                                || Value::known(running),
                            )?;
                            config.q_bit.enable(&mut region, row)?;
                            config.q_running_sum.enable(&mut region, row)?;
                        }

                        let running_final = region.assign_advice(
                            || "running_final",
                            config.running_sum,
                            offset + 128,
                            || Value::known(running),
                        )?;
                        region.constrain_equal(running_final.cell(), value_cell.cell())?;

                        // ---- Note commitment ----------------------------
                        // Compute off-circuit for return
                        let note_cm = if let Some(inp) = input {
                            let v = crate::poseidon::bytes_to_fp(
                                &crate::poseidon::value_to_fp_bytes(inp.value),
                            );
                            let r = crate::poseidon::bytes_to_fp(&inp.rcm);
                            let rh = crate::poseidon::bytes_to_fp(&inp.rho);
                            let a = crate::poseidon::bytes_to_fp(&asset_bytes);
                            let cm = crate::poseidon::poseidon_hash(&[v, a, r, rh]);
                            crate::poseidon::fp_to_bytes(&cm)
                        } else {
                            [0u8; 32]
                        };

                        Ok((value_cell, rcm_cell, ivk_cell, rho_cell, sk_cell, note_cm))
                    },
                )?;

            input_value_cells.push(value_cell.clone());

            // ---- T3: ivk == H("call/shielded/ivk" || sk) --------------------
            let ivk_tag_fp = crate::poseidon::bytes_to_fp(&crate::poseidon::tag_to_bytes(
                crate::poseidon::domain::IVK_FROM_SK,
            ));
            let ivk_tag_cell = layouter.assign_region(
                || format!("ivk tag {}", i),
                |mut region| {
                    region.assign_advice(
                        || "tag",
                        config.value,
                        0,
                        || Value::known(ivk_tag_fp),
                    )
                },
            )?;

            let chip_ivk = Pow5Chip::construct(config.poseidon_config.clone());
            let hasher_ivk =
                PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<2>, 3, 2>::init(
                    chip_ivk,
                    layouter.namespace(|| format!("poseidon ivk {}", i)),
                )?;
            let computed_ivk = hasher_ivk.hash(
                layouter.namespace(|| format!("hash ivk {}", i)),
                [ivk_tag_cell, sk_cell],
            )?;

            layouter.assign_region(
                || format!("ivk equality {}", i),
                |mut region| region.constrain_equal(computed_ivk.cell(), ivk_cell.cell()),
            )?;

            // ---- T1: Nullifier derivation -----------------------------------
            let fvk_tag_fp = crate::poseidon::bytes_to_fp(&crate::poseidon::tag_to_bytes(
                crate::poseidon::domain::FVK_FROM_IVK,
            ));
            let fvk_tag_cell = layouter.assign_region(
                || format!("fvk tag {}", i),
                |mut region| {
                    region.assign_advice(
                        || "tag",
                        config.value,
                        0,
                        || Value::known(fvk_tag_fp),
                    )
                },
            )?;

            let chip_fvk = Pow5Chip::construct(config.poseidon_config.clone());
            let hasher_fvk =
                PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<2>, 3, 2>::init(
                    chip_fvk,
                    layouter.namespace(|| format!("poseidon fvk {}", i)),
                )?;
            let fvk = hasher_fvk.hash(
                layouter.namespace(|| format!("hash fvk {}", i)),
                [fvk_tag_cell, ivk_cell.clone()],
            )?;

            let chip_nf = Pow5Chip::construct(config.poseidon_config.clone());
            let hasher_nf =
                PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<2>, 3, 2>::init(
                    chip_nf,
                    layouter.namespace(|| format!("poseidon nf {}", i)),
                )?;
            let computed_nf = hasher_nf.hash(
                layouter.namespace(|| format!("hash nf {}", i)),
                [fvk, rho_cell.clone()],
            )?;

            layouter.constrain_instance(computed_nf.cell(), config.instance, i)?;

            // ---- Compute note commitment in-circuit -------------------------
            let chip_cm = Pow5Chip::construct(config.poseidon_config.clone());
            let hasher_cm =
                PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<4>, 3, 2>::init(
                    chip_cm,
                    layouter.namespace(|| format!("poseidon cm {}", i)),
                )?;
            let note_cm_cell = hasher_cm.hash(
                layouter.namespace(|| format!("hash cm {}", i)),
                [value_cell, asset_id_cell.clone(), rcm_cell, rho_cell],
            )?;

            input_commitment_cells.push(note_cm_cell.clone());

            // ---- T2: Merkle path validity -----------------------------------
            let merkle_path = self.merkle_paths.as_ref().map(|p| p[i]);
            let mut current = note_cm_cell;

            for (j, (sibling_hash, sibling_is_right)) in
                merkle_path.unwrap_or([[([0u8; 32], false); 32]; 2][0]).iter().enumerate()
            {
                let sibling_fp = crate::poseidon::bytes_to_fp(sibling_hash);
                let is_right_fp = Fp::from(*sibling_is_right as u64);

                let (left_cell, right_cell) = layouter.assign_region(
                    || format!("input {} merkle level {}", i, j),
                    |mut region| {
                        let offset = 0;

                        let current_copy = region.assign_advice(
                            || "current",
                            config.value_inv,
                            offset,
                            || current.value().copied(),
                        )?;
                        region.constrain_equal(current_copy.cell(), current.cell())?;

                        region.assign_advice(
                            || "sibling",
                            config.sibling,
                            offset,
                            || Value::known(sibling_fp),
                        )?;

                        let _is_right_cell = region.assign_advice(
                            || "is_right",
                            config.bit,
                            offset,
                            || Value::known(is_right_fp),
                        )?;
                        config.q_bit.enable(&mut region, offset)?;

                        let current_val = current.value().copied();
                        let left_val = current_val.and_then(|c| {
                            if *sibling_is_right {
                                Value::known(c)
                            } else {
                                Value::known(sibling_fp)
                            }
                        });
                        let right_val = current_val.and_then(|c| {
                            if *sibling_is_right {
                                Value::known(sibling_fp)
                            } else {
                                Value::known(c)
                            }
                        });

                        let left_cell = region.assign_advice(
                            || "left",
                            config.value_inv,
                            offset + 1,
                            || left_val,
                        )?;
                        let right_cell = region.assign_advice(
                            || "right",
                            config.bit,
                            offset + 1,
                            || right_val,
                        )?;
                        config.q_swap.enable(&mut region, offset + 1)?;

                        Ok((left_cell, right_cell))
                    },
                )?;

                let chip_mt = Pow5Chip::construct(config.poseidon_config.clone());
                let hasher_mt = PoseidonHash::<
                    Fp,
                    Pow5Chip<Fp, 3, 2>,
                    P128Pow5T3,
                    ConstantLength<2>,
                    3,
                    2,
                >::init(
                    chip_mt,
                    layouter.namespace(|| format!("poseidon merkle {}-{}", i, j)),
                )?;
                current = hasher_mt.hash(
                    layouter.namespace(|| format!("hash merkle {}-{}", i, j)),
                    [left_cell, right_cell],
                )?;
            }

            // Constrain final hash == merkle_root
            layouter.constrain_instance(current.cell(), config.instance, 5)?;
        }

        // ---- Process output notes ------------------------------------------
        let mut output_value_cells: Vec<AssignedCell<Fp, Fp>> = Vec::with_capacity(2);
        let mut output_commitment_cells: Vec<AssignedCell<Fp, Fp>> = Vec::with_capacity(2);

        for j in 0..2 {
            let output = self.output_notes.as_ref().map(|n| n[j]);

            let (value_cell, rcm_cell, _ivk_cell, rho_cell, _out_cm) = layouter.assign_region(
                || format!("output note {}", j),
                |mut region| {
                    let offset = 0;

                    let value_fp: Value<Fp> = output
                        .map(|w| {
                            Value::known(crate::poseidon::bytes_to_fp(
                                &crate::poseidon::value_to_fp_bytes(w.value),
                            ))
                        })
                        .unwrap_or(Value::unknown());
                    let rcm_fp: Value<Fp> = output
                        .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.rcm)))
                        .unwrap_or(Value::unknown());
                    let ivk_fp: Value<Fp> = output
                        .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.recipient_ivk)))
                        .unwrap_or(Value::unknown());
                    let rho_fp: Value<Fp> = output
                        .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.rho)))
                        .unwrap_or(Value::unknown());

                    let value_cell =
                        region.assign_advice(|| "value", config.value, offset, || value_fp)?;
                    let rcm_cell =
                        region.assign_advice(|| "rcm", config.rcm, offset, || rcm_fp)?;
                    let ivk_cell =
                        region.assign_advice(|| "ivk", config.ivk, offset, || ivk_fp)?;
                    let rho_cell =
                        region.assign_advice(|| "rho", config.rho, offset, || rho_fp)?;

                    // Non-zero inverse
                    let inv_fp: Value<Fp> = value_fp.and_then(|v| {
                        let inv = v.invert();
                        if bool::from(inv.is_some()) {
                            Value::known(inv.unwrap())
                        } else {
                            Value::known(Fp::zero())
                        }
                    });
                    region.assign_advice(
                        || "value_inv",
                        config.value_inv,
                        offset,
                        || inv_fp,
                    )?;
                    config.q_nonzero.enable(&mut region, offset)?;

                    // ---- 128-bit range check ------------------------
                    let value_u128 = output.map(|w| w.value).unwrap_or(0);
                    let two = Fp::from(2u64);

                    region.assign_advice(
                        || "running_0",
                        config.running_sum,
                        offset,
                        || Value::known(Fp::zero()),
                    )?;

                    let mut running = Fp::zero();
                    for k in 0..128 {
                        let bit_val = (value_u128 >> (127 - k)) & 1;
                        let bit_fp = Fp::from(bit_val as u64);
                        running = running * two + bit_fp;

                        let row = offset + 1 + k;
                        region.assign_advice(
                            || format!("bit_{}", k),
                            config.bit,
                            row,
                            || Value::known(bit_fp),
                        )?;
                        region.assign_advice(
                            || format!("running_{}", k + 1),
                            config.running_sum,
                            row,
                            || Value::known(running),
                        )?;
                        config.q_bit.enable(&mut region, row)?;
                        config.q_running_sum.enable(&mut region, row)?;
                    }

                    let running_final = region.assign_advice(
                        || "running_final",
                        config.running_sum,
                        offset + 128,
                        || Value::known(running),
                    )?;
                    region.constrain_equal(running_final.cell(), value_cell.cell())?;

                    // Compute commitment off-circuit for return
                    let out_cm = if let Some(out) = output {
                        let v = crate::poseidon::bytes_to_fp(
                            &crate::poseidon::value_to_fp_bytes(out.value),
                        );
                        let r = crate::poseidon::bytes_to_fp(&out.rcm);
                        let rh = crate::poseidon::bytes_to_fp(&out.rho);
                        let a = crate::poseidon::bytes_to_fp(&asset_bytes);
                        let cm = crate::poseidon::poseidon_hash(&[v, a, r, rh]);
                        crate::poseidon::fp_to_bytes(&cm)
                    } else {
                        [0u8; 32]
                    };

                    Ok((value_cell, rcm_cell, ivk_cell, rho_cell, out_cm))
                },
            )?;

            output_value_cells.push(value_cell.clone());

            // Output commitment in-circuit
            let chip_out = Pow5Chip::construct(config.poseidon_config.clone());
            let hasher_out =
                PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<4>, 3, 2>::init(
                    chip_out,
                    layouter.namespace(|| format!("poseidon out cm {}", j)),
                )?;
            let out_cm_cell = hasher_out.hash(
                layouter.namespace(|| format!("hash out cm {}", j)),
                [value_cell, asset_id_cell.clone(), rcm_cell, rho_cell],
            )?;

            output_commitment_cells.push(out_cm_cell.clone());

            // Constrain output commitment == public input
            layouter.constrain_instance(out_cm_cell.cell(), config.instance, 2 + j)?;
        }

        // ---- T4: Value conservation ----------------------------------------
        layouter.assign_region(
            || "value conservation",
            |mut region| {
                let offset = 0;

                let in0_copy = region.assign_advice(
                    || "in0",
                    config.value,
                    offset,
                    || input_value_cells[0].value().copied(),
                )?;
                region.constrain_equal(in0_copy.cell(), input_value_cells[0].cell())?;

                let in1_copy = region.assign_advice(
                    || "in1",
                    config.rcm,
                    offset,
                    || input_value_cells[1].value().copied(),
                )?;
                region.constrain_equal(in1_copy.cell(), input_value_cells[1].cell())?;

                let out0_copy = region.assign_advice(
                    || "out0",
                    config.ivk,
                    offset,
                    || output_value_cells[0].value().copied(),
                )?;
                region.constrain_equal(out0_copy.cell(), output_value_cells[0].cell())?;

                let out1_copy = region.assign_advice(
                    || "out1",
                    config.rho,
                    offset,
                    || output_value_cells[1].value().copied(),
                )?;
                region.constrain_equal(out1_copy.cell(), output_value_cells[1].cell())?;

                config.q_conservation.enable(&mut region, offset)?;

                Ok(())
            },
        )?;

        // ---- Asset_id public input -----------------------------------------
        layouter.constrain_instance(asset_id_cell.cell(), config.instance, 4)?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Number of public inputs for the transfer circuit (2-in, 2-out).
pub const fn transfer_public_input_count() -> usize {
    6 // 2 nullifiers + 2 commitments + asset_id + merkle_root
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "halo2-prover"))]
mod tests {
    use super::*;
    use crate::merkle_poseidon::PoseidonMerkleTree;
    use crate::poseidon::{bytes_to_fp, fp_to_bytes, poseidon_hash, poseidon_hash_tagged};
    use crate::test_utils::{test_hash, test_spending_key};
    use crate::ViewingKey;
    use halo2_proofs::dev::MockProver;

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

    fn compute_commitment_plain(value: u128, asset_id: u64, rcm: &[u8; 32], rho: &[u8; 32]) -> Fp {
        let value_fp = bytes_to_fp(&crate::poseidon::value_to_fp_bytes(value));
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_fp = bytes_to_fp(&asset_bytes);
        let rcm_fp = bytes_to_fp(rcm);
        let rho_fp = bytes_to_fp(rho);
        poseidon_hash(&[value_fp, asset_fp, rcm_fp, rho_fp])
    }

    fn derive_nullifier_plain(ivk: &[u8; 32], rho: &[u8; 32]) -> Fp {
        let fvk_tag = bytes_to_fp(&crate::poseidon::tag_to_bytes(crate::poseidon::domain::FVK_FROM_IVK));
        let ivk_fp = bytes_to_fp(ivk);
        let rho_fp = bytes_to_fp(rho);
        let fvk = poseidon_hash(&[fvk_tag, ivk_fp]);
        poseidon_hash(&[fvk, rho_fp])
    }

    fn make_instance(
        nullifiers: &[Fp; 2],
        commitments: &[Fp; 2],
        asset_id: u64,
        merkle_root_fp: Fp,
    ) -> Vec<Fp> {
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_id_fp = bytes_to_fp(&asset_bytes);

        vec![
            nullifiers[0],
            nullifiers[1],
            commitments[0],
            commitments[1],
            asset_id_fp,
            merkle_root_fp,
        ]
    }

    fn build_transfer_data(
        in_values: [u128; 2],
        out_values: [u128; 2],
        asset_id: u64,
        seed_base: u8,
    ) -> (
        [Fp; 2],
        [Fp; 2],
        [u8; 32],
        [InputNoteWitness; 2],
        [OutputNoteWitness; 2],
        [[([u8; 32], bool); 32]; 2],
    ) {
        let mut tree = PoseidonMerkleTree::new(32);
        let mut input_notes = Vec::with_capacity(2);
        let mut nullifiers = Vec::with_capacity(2);
        let mut merkle_paths = Vec::with_capacity(2);

        for i in 0..2 {
            let sk = test_spending_key(seed_base + i as u8);
            let vk = ViewingKey::generate(&sk);
            let rho = test_hash(seed_base + i as u8).0;
            let rcm = compute_rcm_plain(&vk, in_values[i], asset_id, &rho);

            input_notes.push(InputNoteWitness {
                value: in_values[i],
                rcm,
                recipient_ivk: vk.incoming_view_key,
                rho,
                spending_key: sk,
            });

            let nf_fp = derive_nullifier_plain(&vk.incoming_view_key, &rho);
            nullifiers.push(nf_fp);

            let cm_fp = compute_commitment_plain(in_values[i], asset_id, &rcm, &rho);
            let cm = fp_to_bytes(&cm_fp);
            tree.insert(&cm);
        }

        let merkle_root = tree.root();

        // Collect merkle paths for each input (by index)
        for i in 0..2 {
            let proof = tree.proof_for_index(i).unwrap();
            // Convert Vec to fixed-size array
            let mut path_arr = [([0u8; 32], false); 32];
            for (j, p) in proof.iter().enumerate() {
                path_arr[j] = *p;
            }
            merkle_paths.push(path_arr);
        }

        let mut output_notes = Vec::with_capacity(2);
        let mut commitments = Vec::with_capacity(2);

        for j in 0..2 {
            let sk = test_spending_key(seed_base + 10 + j as u8);
            let vk = ViewingKey::generate(&sk);
            let rho = test_hash(seed_base + 10 + j as u8).0;
            let rcm = compute_rcm_plain(&vk, out_values[j], asset_id, &rho);

            output_notes.push(OutputNoteWitness {
                value: out_values[j],
                rcm,
                recipient_ivk: vk.incoming_view_key,
                rho,
            });

            let cm_fp = compute_commitment_plain(out_values[j], asset_id, &rcm, &rho);
            commitments.push(cm_fp);
        }

        let nf_arr = [nullifiers[0], nullifiers[1]];
        let cm_arr = [commitments[0], commitments[1]];
        let in_arr = [input_notes[0], input_notes[1]];
        let out_arr = [output_notes[0], output_notes[1]];
        let mp_arr = [merkle_paths[0], merkle_paths[1]];

        (nf_arr, cm_arr, merkle_root, in_arr, out_arr, mp_arr)
    }

    #[test]
    fn test_transfer_circuit_satisfiable() {
        let (nullifiers, commitments, merkle_root, input_notes, output_notes, merkle_paths) =
            build_transfer_data([500, 500], [600, 400], 1, 1);

        let circuit = TransferCircuit::new(
            [fp_to_bytes(&nullifiers[0]), fp_to_bytes(&nullifiers[1])],
            [fp_to_bytes(&commitments[0]), fp_to_bytes(&commitments[1])],
            1,
            merkle_root,
            input_notes,
            output_notes,
            merkle_paths,
        );

        let merkle_root_fp = bytes_to_fp(&merkle_root);
        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(&nullifiers, &commitments, 1, merkle_root_fp)],
        )
        .unwrap();
        prover.assert_satisfied();
    }

    #[test]
    fn test_transfer_circuit_zero_value_input_rejected() {
        let (nullifiers, commitments, merkle_root, mut input_notes, output_notes, merkle_paths) =
            build_transfer_data([500, 500], [600, 400], 1, 1);
        input_notes[0].value = 0;

        // Recompute nullifier and commitment for modified input
        let sk = input_notes[0].spending_key;
        let vk = ViewingKey::generate(&sk);
        let nf_fp = derive_nullifier_plain(&vk.incoming_view_key, &input_notes[0].rho);
        let nullifiers = [nf_fp, nullifiers[1]];

        let circuit = TransferCircuit::new(
            [fp_to_bytes(&nullifiers[0]), fp_to_bytes(&nullifiers[1])],
            [fp_to_bytes(&commitments[0]), fp_to_bytes(&commitments[1])],
            1,
            merkle_root,
            input_notes,
            output_notes,
            merkle_paths,
        );

        let merkle_root_fp = bytes_to_fp(&merkle_root);
        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(&nullifiers, &commitments, 1, merkle_root_fp)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "zero value input should be rejected");
    }

    #[test]
    fn test_transfer_circuit_wrong_nullifier_rejected() {
        let (nullifiers, commitments, merkle_root, input_notes, output_notes, merkle_paths) =
            build_transfer_data([500, 500], [600, 400], 1, 1);

        let mut bad_nullifiers = nullifiers;
        bad_nullifiers[0] = bytes_to_fp(&[0xFFu8; 32]);

        let circuit = TransferCircuit::new(
            [fp_to_bytes(&bad_nullifiers[0]), fp_to_bytes(&bad_nullifiers[1])],
            [fp_to_bytes(&commitments[0]), fp_to_bytes(&commitments[1])],
            1,
            merkle_root,
            input_notes,
            output_notes,
            merkle_paths,
        );

        let merkle_root_fp = bytes_to_fp(&merkle_root);
        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(&bad_nullifiers, &commitments, 1, merkle_root_fp)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "wrong nullifier should be rejected");
    }

    #[test]
    fn test_transfer_circuit_wrong_commitment_rejected() {
        let (nullifiers, commitments, merkle_root, input_notes, output_notes, merkle_paths) =
            build_transfer_data([500, 500], [600, 400], 1, 1);

        let mut bad_commitments = commitments;
        bad_commitments[0] = bytes_to_fp(&[0xFFu8; 32]);

        let circuit = TransferCircuit::new(
            [fp_to_bytes(&nullifiers[0]), fp_to_bytes(&nullifiers[1])],
            [fp_to_bytes(&bad_commitments[0]), fp_to_bytes(&bad_commitments[1])],
            1,
            merkle_root,
            input_notes,
            output_notes,
            merkle_paths,
        );

        let merkle_root_fp = bytes_to_fp(&merkle_root);
        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(&nullifiers, &bad_commitments, 1, merkle_root_fp)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "wrong commitment should be rejected");
    }

    #[test]
    fn test_transfer_circuit_wrong_merkle_root_rejected() {
        let (nullifiers, commitments, merkle_root, input_notes, output_notes, merkle_paths) =
            build_transfer_data([500, 500], [600, 400], 1, 1);

        let mut bad_root = merkle_root;
        bad_root[0] ^= 0xFF;

        let circuit = TransferCircuit::new(
            [fp_to_bytes(&nullifiers[0]), fp_to_bytes(&nullifiers[1])],
            [fp_to_bytes(&commitments[0]), fp_to_bytes(&commitments[1])],
            1,
            bad_root,
            input_notes,
            output_notes,
            merkle_paths,
        );

        let bad_root_fp = bytes_to_fp(&bad_root);
        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(&nullifiers, &commitments, 1, bad_root_fp)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "wrong merkle root should be rejected");
    }

    #[test]
    fn test_transfer_circuit_value_creation_rejected() {
        // Inputs sum to 500, outputs sum to 600 → value creation
        let (nullifiers, commitments, merkle_root, input_notes, output_notes, merkle_paths) =
            build_transfer_data([200, 300], [500, 100], 1, 1);

        let circuit = TransferCircuit::new(
            [fp_to_bytes(&nullifiers[0]), fp_to_bytes(&nullifiers[1])],
            [fp_to_bytes(&commitments[0]), fp_to_bytes(&commitments[1])],
            1,
            merkle_root,
            input_notes,
            output_notes,
            merkle_paths,
        );

        let merkle_root_fp = bytes_to_fp(&merkle_root);
        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(&nullifiers, &commitments, 1, merkle_root_fp)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "value creation should be rejected");
    }

    #[test]
    fn test_transfer_circuit_wrong_asset_id_rejected() {
        let (nullifiers, commitments, merkle_root, input_notes, output_notes, merkle_paths) =
            build_transfer_data([500, 500], [600, 400], 1, 1);

        let circuit = TransferCircuit::new(
            [fp_to_bytes(&nullifiers[0]), fp_to_bytes(&nullifiers[1])],
            [fp_to_bytes(&commitments[0]), fp_to_bytes(&commitments[1])],
            999, // wrong asset_id
            merkle_root,
            input_notes,
            output_notes,
            merkle_paths,
        );

        let merkle_root_fp = bytes_to_fp(&merkle_root);
        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(&nullifiers, &commitments, 999, merkle_root_fp)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "wrong asset_id should be rejected");
    }

    #[test]
    fn test_transfer_public_input_count() {
        assert_eq!(transfer_public_input_count(), 6);
    }

    #[test]
    fn test_transfer_circuit_for_verify() {
        let circuit = TransferCircuit::for_verify(
            [[1u8; 32], [2u8; 32]],
            [[3u8; 32], [4u8; 32]],
            1,
            [5u8; 32],
        );
        assert!(circuit.input_notes.is_none());
        assert!(circuit.output_notes.is_none());
    }
}
