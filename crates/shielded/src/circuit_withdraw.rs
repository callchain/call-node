//! ShieldedWithdraw circuit — Halo2 PLONKish version over Pasta Pallas.
//!
//! Proves that a user is withdrawing from the shielded pool to a transparent
//! address, consuming a shielded note.
//!
//! Public inputs: nullifier (Fp), asset_id (Fp), value (Fp),
//!                merkle_root (Fp), target_address (Fp)
//! Private witnesses: note_value (u128), rcm (Fp), recipient_ivk (Fp),
//!                    rho (Fp), spending_key (Fp), merkle_path (32 siblings)
//!
//! Constraints:
//!   W1. Nullifier: H(H("fvk_from_ivk" || ivk), rho) == public nullifier
//!   W2. Merkle: note_commitment -> walk 32 Poseidon levels -> root == public
//!   W3. Value: note_value == public value
//!   W4. Spending: ivk == H("call/shielded/ivk" || sk)
//!   W5. Range: non-zero + 128-bit range

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

/// Witness data for a withdraw note.
#[derive(Debug, Clone)]
pub struct WithdrawWitness {
    pub note_value: u128,
    pub rcm: [u8; 32],
    pub recipient_ivk: [u8; 32],
    pub rho: [u8; 32],
    pub spending_key: [u8; 32],
    pub merkle_path: Vec<([u8; 32], bool)>,
}

/// ShieldedWithdraw circuit.
#[derive(Debug, Clone)]
pub struct WithdrawCircuit {
    /// Public inputs
    pub nullifier: [u8; 32],
    pub asset_id: u64,
    pub value: u128,
    pub target_address: [u8; 20],
    pub merkle_root: [u8; 32],
    /// Private witnesses
    pub witness: Option<WithdrawWitness>,
}

impl WithdrawCircuit {
    /// Create a new withdraw circuit from public data and private witness.
    pub fn new(
        nullifier: [u8; 32],
        asset_id: u64,
        value: u128,
        target_address: [u8; 20],
        merkle_root: [u8; 32],
        witness: WithdrawWitness,
    ) -> Self {
        Self {
            nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness: Some(witness),
        }
    }

    /// Create a circuit with only public data (for verification only).
    pub fn for_verify(
        nullifier: [u8; 32],
        asset_id: u64,
        value: u128,
        target_address: [u8; 20],
        merkle_root: [u8; 32],
    ) -> Self {
        Self {
            nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct WithdrawConfig {
    // Witness columns
    value: Column<Advice>,
    rcm: Column<Advice>,
    ivk: Column<Advice>,
    rho: Column<Advice>,
    sk: Column<Advice>,
    asset_id: Column<Advice>,
    sibling: Column<Advice>,

    // Range-check / non-zero auxiliary columns (also reused for Merkle swap)
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
}

// ---------------------------------------------------------------------------
// Circuit impl
// ---------------------------------------------------------------------------

impl Circuit<Fp> for WithdrawCircuit {
    type Config = WithdrawConfig;
    type FloorPlanner = SimpleFloorPlanner;

    fn without_witnesses(&self) -> Self {
        Self {
            nullifier: self.nullifier,
            asset_id: self.asset_id,
            value: self.value,
            target_address: self.target_address,
            merkle_root: self.merkle_root,
            witness: None,
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

        // ---- Enable equality ------------------------------------------------
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

        // Merkle swap gate:
        // Row 0: current (value_inv), sibling (sibling), is_right (bit)
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

        WithdrawConfig {
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
                Value::known(crate::poseidon::bytes_to_fp(
                    &crate::poseidon::value_to_fp_bytes(w.note_value),
                ))
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
        let sk_fp: Value<Fp> = self
            .witness
            .as_ref()
            .map(|w| Value::known(crate::poseidon::bytes_to_fp(&w.spending_key)))
            .unwrap_or(Value::unknown());

        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&self.asset_id.to_le_bytes());
        let asset_id_fp = Value::known(crate::poseidon::bytes_to_fp(&asset_bytes));

        // Inverse for non-zero check
        let inv_fp: Value<Fp> = value_fp.and_then(|v| {
            let inv = v.invert();
            if bool::from(inv.is_some()) {
                Value::known(inv.unwrap())
            } else {
                Value::known(Fp::zero())
            }
        });

        // ---- Assign witnesses + range check --------------------------------
        let (value_cell, rcm_cell, ivk_cell, rho_cell, sk_cell, asset_id_cell) = layouter
            .assign_region(
                || "withdraw witnesses",
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
                    let sk_cell =
                        region.assign_advice(|| "sk", config.sk, offset, || sk_fp)?;
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
                    let value_u128 = self.witness.as_ref().map(|w| w.note_value).unwrap_or(0);
                    let two = Fp::from(2u64);

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

                    // Final running_sum must equal value
                    let running_final = region.assign_advice(
                        || "running_final",
                        config.running_sum,
                        offset + 128,
                        || Value::known(running),
                    )?;
                    region.constrain_equal(running_final.cell(), value_cell.cell())?;

                    Ok((value_cell, rcm_cell, ivk_cell, rho_cell, sk_cell, asset_id_cell))
                },
            )?;

        // ---- W3: Constrain value == public input ----------------------------
        layouter.constrain_instance(value_cell.cell(), config.instance, 2)?;

        // ---- W4: ivk == H("call/shielded/ivk" || sk) ------------------------
        let ivk_tag_fp = crate::poseidon::bytes_to_fp(&crate::poseidon::tag_to_bytes(
            crate::poseidon::domain::IVK_FROM_SK,
        ));
        let ivk_tag_cell = layouter.assign_region(
            || "ivk tag",
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
                layouter.namespace(|| "poseidon ivk"),
            )?;
        let computed_ivk = hasher_ivk.hash(
            layouter.namespace(|| "hash ivk"),
            [ivk_tag_cell, sk_cell],
        )?;

        layouter.assign_region(
            || "ivk equality",
            |mut region| region.constrain_equal(computed_ivk.cell(), ivk_cell.cell()),
        )?;

        // ---- Compute note commitment ----------------------------------------
        let chip_cm = Pow5Chip::construct(config.poseidon_config.clone());
        let hasher_cm =
            PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<4>, 3, 2>::init(
                chip_cm,
                layouter.namespace(|| "poseidon cm"),
            )?;
        let note_cm = hasher_cm.hash(
            layouter.namespace(|| "hash cm"),
            [value_cell.clone(), asset_id_cell.clone(), rcm_cell.clone(), rho_cell.clone()],
        )?;

        // ---- W2: Merkle path validity ---------------------------------------
        let merkle_path = self
            .witness
            .as_ref()
            .map(|w| w.merkle_path.clone())
            .unwrap_or_default();

        let mut current = note_cm;
        for (i, (sibling_hash, sibling_is_right)) in merkle_path.iter().enumerate() {
            let sibling_fp = crate::poseidon::bytes_to_fp(sibling_hash);
            let is_right_fp = Fp::from(*sibling_is_right as u64);

            let (left_cell, right_cell) = layouter.assign_region(
                || format!("merkle level {}", i),
                |mut region| {
                    let offset = 0;

                    // Row 0: current copy, sibling, is_right
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

                    // Compute left/right in plain Rust
                    // sibling_is_right=true  → sibling is on right → left=current, right=sibling
                    // sibling_is_right=false → sibling is on left  → left=sibling, right=current
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

                    // Row 1: left, right
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

            // Poseidon hash [left, right]
            let chip_mt = Pow5Chip::construct(config.poseidon_config.clone());
            let hasher_mt =
                PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<2>, 3, 2>::init(
                    chip_mt,
                    layouter.namespace(|| format!("poseidon merkle {}", i)),
                )?;
            current = hasher_mt.hash(
                layouter.namespace(|| format!("hash merkle {}", i)),
                [left_cell, right_cell],
            )?;
        }

        // Constrain final hash == merkle_root
        layouter.constrain_instance(current.cell(), config.instance, 3)?;

        // ---- W1: Nullifier derivation ---------------------------------------
        let fvk_tag_fp = crate::poseidon::bytes_to_fp(&crate::poseidon::tag_to_bytes(
            crate::poseidon::domain::FVK_FROM_IVK,
        ));
        let fvk_tag_cell = layouter.assign_region(
            || "fvk tag",
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
                layouter.namespace(|| "poseidon fvk"),
            )?;
        let fvk = hasher_fvk.hash(
            layouter.namespace(|| "hash fvk"),
            [fvk_tag_cell, ivk_cell],
        )?;

        let chip_nf = Pow5Chip::construct(config.poseidon_config.clone());
        let hasher_nf =
            PoseidonHash::<Fp, Pow5Chip<Fp, 3, 2>, P128Pow5T3, ConstantLength<2>, 3, 2>::init(
                chip_nf,
                layouter.namespace(|| "poseidon nf"),
            )?;
        let computed_nf = hasher_nf.hash(
            layouter.namespace(|| "hash nf"),
            [fvk, rho_cell],
        )?;

        layouter.constrain_instance(computed_nf.cell(), config.instance, 0)?;

        // ---- asset_id == public input ---------------------------------------
        layouter.constrain_instance(asset_id_cell.cell(), config.instance, 1)?;

        // ---- target_address == public input ---------------------------------
        let mut target_bytes = [0u8; 32];
        target_bytes[..20].copy_from_slice(&self.target_address);
        let target_fp = Value::known(crate::poseidon::bytes_to_fp(&target_bytes));
        let target_cell = layouter.assign_region(
            || "target_address",
            |mut region| {
                region.assign_advice(|| "target", config.value, 0, || target_fp)
            },
        )?;
        layouter.constrain_instance(target_cell.cell(), config.instance, 4)?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Number of public inputs for the withdraw circuit.
pub const fn withdraw_public_input_count() -> usize {
    5 // nullifier + asset_id + value + merkle_root + target_address
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

    fn make_withdraw_witness(value: u128, asset_id: u64, seed: u8) -> WithdrawWitness {
        let sk = test_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(seed).0;

        WithdrawWitness {
            note_value: value,
            rcm: compute_rcm_plain(&vk, value, asset_id, &rho),
            recipient_ivk: vk.incoming_view_key,
            rho,
            spending_key: sk,
            merkle_path: vec![],
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

    fn derive_ivk_plain(sk: &[u8; 32]) -> Fp {
        let tag = bytes_to_fp(&crate::poseidon::tag_to_bytes(crate::poseidon::domain::IVK_FROM_SK));
        let sk_fp = bytes_to_fp(sk);
        poseidon_hash(&[tag, sk_fp])
    }

    fn make_instance(nullifier_fp: Fp, asset_id: u64, value: u128, merkle_root_fp: Fp, target_address: [u8; 20]) -> Vec<Fp> {
        let mut asset_bytes = [0u8; 32];
        asset_bytes[..8].copy_from_slice(&asset_id.to_le_bytes());
        let asset_id_fp = bytes_to_fp(&asset_bytes);

        let value_fp = bytes_to_fp(&crate::poseidon::value_to_fp_bytes(value));

        let mut target_bytes = [0u8; 32];
        target_bytes[..20].copy_from_slice(&target_address);
        let target_fp = bytes_to_fp(&target_bytes);

        vec![nullifier_fp, asset_id_fp, value_fp, merkle_root_fp, target_fp]
    }

    fn build_withdraw_data(value: u128, asset_id: u64, seed: u8) -> ([u8; 32], u64, u128, [u8; 20], [u8; 32], WithdrawWitness) {
        let sk = test_spending_key(seed);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(seed).0;

        let rcm = compute_rcm_plain(&vk, value, asset_id, &rho);
        let nullifier_fp = derive_nullifier_plain(&vk.incoming_view_key, &rho);
        let nullifier = fp_to_bytes(&nullifier_fp);

        let commitment_fp = compute_commitment_plain(value, asset_id, &rcm, &rho);
        let commitment = fp_to_bytes(&commitment_fp);

        // Build Merkle tree with the commitment
        let mut tree = PoseidonMerkleTree::new(32);
        tree.insert(&commitment);
        let merkle_root = tree.root();
        let merkle_path = tree.proof_for_last();

        let target_address = [seed; 20];

        let witness = WithdrawWitness {
            note_value: value,
            rcm,
            recipient_ivk: vk.incoming_view_key,
            rho,
            spending_key: sk,
            merkle_path,
        };

        (nullifier, asset_id, value, target_address, merkle_root, witness)
    }

    #[test]
    fn test_withdraw_circuit_satisfiable() {
        let (nullifier, asset_id, value, target_address, merkle_root, witness) =
            build_withdraw_data(1000, 1, 1);

        let nullifier_fp = bytes_to_fp(&nullifier);
        let merkle_root_fp = bytes_to_fp(&merkle_root);

        let circuit = WithdrawCircuit::new(
            nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness,
        );

        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(nullifier_fp, asset_id, value, merkle_root_fp, target_address)],
        )
        .unwrap();
        prover.assert_satisfied();
    }

    #[test]
    fn test_withdraw_circuit_zero_value_rejected() {
        let (nullifier, asset_id, _value, target_address, merkle_root, mut witness) =
            build_withdraw_data(1000, 1, 1);
        witness.note_value = 0;

        let nullifier_fp = bytes_to_fp(&nullifier);
        let merkle_root_fp = bytes_to_fp(&merkle_root);

        let circuit = WithdrawCircuit::new(
            nullifier,
            asset_id,
            0,
            target_address,
            merkle_root,
            witness,
        );

        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(nullifier_fp, asset_id, 0, merkle_root_fp, target_address)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "zero value should be rejected");
    }

    #[test]
    fn test_withdraw_circuit_wrong_nullifier_rejected() {
        let (nullifier, asset_id, value, target_address, merkle_root, witness) =
            build_withdraw_data(1000, 1, 1);

        let mut bad_nullifier = nullifier;
        bad_nullifier[0] ^= 0xFF;
        let bad_nullifier_fp = bytes_to_fp(&bad_nullifier);
        let merkle_root_fp = bytes_to_fp(&merkle_root);

        let circuit = WithdrawCircuit::new(
            bad_nullifier,
            asset_id,
            value,
            target_address,
            merkle_root,
            witness,
        );

        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(bad_nullifier_fp, asset_id, value, merkle_root_fp, target_address)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "wrong nullifier should be rejected");
    }

    #[test]
    fn test_withdraw_circuit_wrong_merkle_root_rejected() {
        let (nullifier, asset_id, value, target_address, merkle_root, witness) =
            build_withdraw_data(1000, 1, 1);

        let nullifier_fp = bytes_to_fp(&nullifier);
        let mut bad_root = merkle_root;
        bad_root[0] ^= 0xFF;
        let bad_root_fp = bytes_to_fp(&bad_root);

        let circuit = WithdrawCircuit::new(
            nullifier,
            asset_id,
            value,
            target_address,
            bad_root,
            witness,
        );

        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(nullifier_fp, asset_id, value, bad_root_fp, target_address)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "wrong merkle root should be rejected");
    }

    #[test]
    fn test_withdraw_circuit_wrong_value_rejected() {
        let (nullifier, asset_id, value, target_address, merkle_root, witness) =
            build_withdraw_data(1000, 1, 1);

        let nullifier_fp = bytes_to_fp(&nullifier);
        let merkle_root_fp = bytes_to_fp(&merkle_root);
        let bad_value = value + 1;

        let circuit = WithdrawCircuit::new(
            nullifier,
            asset_id,
            bad_value,
            target_address,
            merkle_root,
            witness,
        );

        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(nullifier_fp, asset_id, bad_value, merkle_root_fp, target_address)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "wrong value should be rejected");
    }

    #[test]
    fn test_withdraw_circuit_wrong_asset_id_rejected() {
        let (nullifier, _asset_id, value, target_address, merkle_root, witness) =
            build_withdraw_data(1000, 1, 1);

        let nullifier_fp = bytes_to_fp(&nullifier);
        let merkle_root_fp = bytes_to_fp(&merkle_root);
        let bad_asset_id = 999u64;

        let circuit = WithdrawCircuit::new(
            nullifier,
            bad_asset_id,
            value,
            target_address,
            merkle_root,
            witness,
        );

        let prover = MockProver::run(
            12,
            &circuit,
            vec![make_instance(nullifier_fp, bad_asset_id, value, merkle_root_fp, target_address)],
        )
        .unwrap();
        assert!(prover.verify().is_err(), "wrong asset_id should be rejected");
    }

    #[test]
    fn test_withdraw_circuit_nullifier_derivation() {
        let sk = test_spending_key(7);
        let vk = ViewingKey::generate(&sk);
        let rho = test_hash(7).0;

        let nf1 = derive_nullifier_plain(&vk.incoming_view_key, &rho);
        let nf2 = derive_nullifier_plain(&vk.incoming_view_key, &rho);
        assert_eq!(nf1, nf2);
        assert_ne!(nf1, Fp::zero());
    }

    #[test]
    fn test_withdraw_circuit_spending_rights() {
        let sk = test_spending_key(42);
        let vk = ViewingKey::generate(&sk);

        let derived_ivk = derive_ivk_plain(&sk);
        let expected_ivk = bytes_to_fp(&vk.incoming_view_key);
        assert_eq!(derived_ivk, expected_ivk);

        let sk2 = test_spending_key(43);
        let derived_ivk2 = derive_ivk_plain(&sk2);
        assert_ne!(derived_ivk, derived_ivk2);
    }

    #[test]
    fn test_withdraw_public_input_count() {
        assert_eq!(withdraw_public_input_count(), 5);
    }

    #[test]
    fn test_withdraw_circuit_for_verify() {
        let circuit = WithdrawCircuit::for_verify([1u8; 32], 1, 500, [2u8; 20], [3u8; 32]);
        assert!(circuit.witness.is_none());
    }
}
