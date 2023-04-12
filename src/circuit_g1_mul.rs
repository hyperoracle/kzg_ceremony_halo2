use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use ark_std::{end_timer, start_timer};
use num_bigint::BigUint;
use rand::rngs::OsRng;

use halo2_proofs::arithmetic::{CurveAffine, Field, FieldExt};
use halo2_proofs::circuit::{Layouter, SimpleFloorPlanner};
use halo2_proofs::pairing::bls12_381;
use halo2_proofs::pairing::bn256::{self, Bn256, Fr};
use halo2_proofs::pairing::group::Curve;
use halo2_proofs::plonk::{self, ConstraintSystem, Error, SingleVerifier};
use halo2_proofs::poly::commitment::{Params, ParamsVerifier};
use halo2_proofs::transcript::{Blake2bRead, Blake2bWrite, Challenge255};

use crate::circuit_utils::base_chip::{BaseChip, BaseChipConfig};
use crate::circuit_utils::ecc_chip::{EccChipBaseOps, EccChipScalarOps};
use crate::circuit_utils::integer_chip::IntegerChipOps;
use crate::circuit_utils::range_chip::{
    RangeChip, RangeChipConfig, RangeChipOps, COMMON_RANGE_BITS, MAX_CHUNKS,
};
use crate::context::{Context, GeneralScalarEccContext};
use crate::utils::{bn_to_field, field_to_bn};

pub const LENGTH: usize = 32;
#[allow(dead_code)]
const K: u32 = 23;
const INSTANCE_NUM: usize = 1 + 8 + 8 * 2 * LENGTH;

#[derive(Clone, Debug)]
pub struct Config {
    base_chip_config: BaseChipConfig,
    range_chip_config: RangeChipConfig,
}

#[derive(Clone, Debug)]
pub struct Circuit<C: CurveAffine, N: FieldExt> {
    pub from_index: Option<usize>,
    pub tau: Option<C::ScalarExt>,
    pub points: Vec<Option<C>>,
    pub new_points: Vec<Option<C>>,
    pub _mark: PhantomData<N>,
}

impl<C: CurveAffine, N: FieldExt> Default for Circuit<C, N> {
    fn default() -> Self {
        Self {
            from_index: None,
            tau: None,
            points: vec![None; LENGTH],
            new_points: vec![None; LENGTH],
            _mark: Default::default(),
        }
    }
}

impl<C: CurveAffine, N: FieldExt> plonk::Circuit<N> for Circuit<C, N> {
    type Config = Config;
    type FloorPlanner = SimpleFloorPlanner;

    fn without_witnesses(&self) -> Self {
        Self::default()
    }

    fn configure(meta: &mut ConstraintSystem<N>) -> Self::Config {
        Config {
            base_chip_config: BaseChip::configure(meta),
            range_chip_config: RangeChip::<N>::configure(meta),
        }
    }

    fn synthesize(
        &self,
        config: Self::Config,
        mut layouter: impl Layouter<N>,
    ) -> Result<(), Error> {
        let base_chip = BaseChip::new(config.base_chip_config.clone());
        let range_chip = RangeChip::<N>::new(config.range_chip_config);
        range_chip.init_table(&mut layouter)?;

        let ctx = Rc::new(RefCell::new(Context::new()));
        let mut ctx = GeneralScalarEccContext::<C, N>::new(ctx);
        let mut instances = vec![];

        // load from_index
        let from_index = ctx
            .scalar_integer_ctx
            .assign_small_number(self.from_index.unwrap_or_default(), 16);
        instances.push(from_index.clone());

        // load tau and check pubkey
        let tau = ctx
            .scalar_integer_ctx
            .assign_w(&field_to_bn(&self.tau.unwrap_or_default()));
        let generator = ctx.assign_constant_point(&C::generator());

        let pubkey = ctx.ecc_mul(&generator, tau.clone());
        instances.extend_from_slice(&pubkey.x.limbs_le);
        instances.extend_from_slice(&pubkey.y.limbs_le);

        // load points
        assert_eq!(self.points.len(), LENGTH);
        let points = self
            .points
            .iter()
            .map(|x| {
                let x = x.unwrap_or(C::identity());
                let p = ctx.assign_non_zero_point(&x);
                instances.extend_from_slice(&p.x.limbs_le);
                instances.extend_from_slice(&p.y.limbs_le);

                p
            })
            .collect::<Vec<_>>();
        let new_points = self
            .new_points
            .iter()
            .map(|x| {
                let x = x.unwrap_or(C::identity());
                let p = ctx.assign_non_zero_point(&x);
                instances.extend_from_slice(&p.x.limbs_le);
                instances.extend_from_slice(&p.y.limbs_le);

                p
            })
            .collect::<Vec<_>>();

        // process scalars
        let scalars = {
            // cal {tau^(from_index+i)| i=0,1,...,LENGTH-1
            let pow_bits_be = {
                let pow_bn = field_to_bn(&from_index.val);

                let mut bits_le = (0..16)
                    .map(|i| {
                        ctx.scalar_integer_ctx
                            .base_chip()
                            .assign_bit(pow_bn.bit(i).into())
                    })
                    .collect::<Vec<_>>();
                {
                    let schema = bits_le
                        .iter()
                        .enumerate()
                        .map(|(i, el)| (&el.0, N::from(1 << i)))
                        .collect::<Vec<_>>();

                    let sum = ctx
                        .scalar_integer_ctx
                        .base_chip()
                        .sum_with_constant(schema, None);
                    ctx.scalar_integer_ctx
                        .base_chip()
                        .assert_equal(&from_index, &sum);
                }

                bits_le.reverse();

                bits_le
            };

            let mut acc = ctx
                .scalar_integer_ctx
                .assign_int_constant(C::ScalarExt::one());

            for (i, bit) in pow_bits_be.iter().enumerate() {
                if i != 0 {
                    acc = ctx.scalar_integer_ctx.int_mul(&acc, &acc);
                }
                let acc_tmp = ctx.scalar_integer_ctx.int_mul(&acc, &tau);
                acc = ctx.scalar_integer_ctx.bisec_int(bit, &acc_tmp, &acc);
            }
            let mut scalars = vec![acc.clone()];
            for _ in 1..LENGTH {
                acc = ctx.scalar_integer_ctx.int_mul(&acc, &tau);
                scalars.push(acc.clone());
            }

            scalars
        };

        for (point, (new_point, scalar)) in points
            .iter()
            .zip(new_points.iter().zip(scalars.into_iter()))
        {
            let p = ctx.ecc_mul(point, scalar);
            ctx.ecc_assert_equal(&p, &new_point);
        }
        assert_eq!(instances.len(), INSTANCE_NUM);

        let ctx = Context::<N>::from(ctx);
        let mut records = Arc::try_unwrap(ctx.records).unwrap().into_inner().unwrap();

        for instance in instances.iter() {
            records.enable_permute(&instance.cell);
        }

        let mut assigned_instance_cells = vec![];
        layouter.assign_region(
            || "base",
            |mut region| {
                let cells = records.assign_all(&mut region, &base_chip, &range_chip)?;
                assigned_instance_cells = instances
                    .iter()
                    .map(|ist| {
                        let cell = cells[ist.cell.region as usize][ist.cell.col][ist.cell.row]
                            .clone()
                            .unwrap();

                        cell.cell()
                    })
                    .collect::<Vec<_>>();
                Ok(())
            },
        )?;

        // Constrain public input
        for (offset, instance) in assigned_instance_cells.into_iter().enumerate() {
            layouter.constrain_instance(instance, config.base_chip_config.primary, offset)?;
        }

        Ok(())
    }
}

/// The verifying key for the Orchard Action circuit.
#[derive(Debug)]
pub struct VerifyingKey {
    pub vk: plonk::VerifyingKey<bn256::G1Affine>,
}

impl VerifyingKey {
    /// Builds the verifying key.
    pub fn build(params: &Params<bn256::G1Affine>) -> Self {
        let circuit: Circuit<bls12_381::G1Affine, Fr> = Default::default();

        let vk = plonk::keygen_vk(&params, &circuit).unwrap();

        VerifyingKey { vk }
    }
}

#[derive(Debug)]
pub struct ProvingKey {
    pk: plonk::ProvingKey<bn256::G1Affine>,
}

impl ProvingKey {
    /// Builds the proving key.
    pub fn build(params: &Params<bn256::G1Affine>) -> Self {
        let circuit: Circuit<bls12_381::G1Affine, Fr> = Default::default();

        let vk = plonk::keygen_vk(&params, &circuit).unwrap();
        let pk = plonk::keygen_pk(&params, vk, &circuit).unwrap();

        ProvingKey { pk }
    }
}

#[derive(Clone, Debug)]
pub struct Instance {
    pub(crate) from_index: usize,
    pub(crate) pubkey: bls12_381::G1Affine,
    pub(crate) old_points: Vec<bls12_381::G1Affine>,
    pub(crate) new_points: Vec<bls12_381::G1Affine>,
}

pub(crate) fn generate_instance(instance: &Instance) -> Vec<Fr> {
    let mut halo2_instances = vec![bn_to_field::<Fr>(&BigUint::from(instance.from_index))];

    let bits = COMMON_RANGE_BITS * MAX_CHUNKS;
    let bit_mask = (BigUint::from(1u64) << bits) - 1u64;

    let split_point = |p: &bls12_381::G1Affine| {
        let mut limbs = vec![];
        for el in vec![p.x, p.y].iter() {
            let bu = field_to_bn(el);
            let part = (0..4)
                .map(|i| bn_to_field::<Fr>(&((&bu >> (i * bits)) & &bit_mask)))
                .collect::<Vec<_>>();
            limbs.extend_from_slice(&part);
        }

        limbs
    };

    halo2_instances.extend_from_slice(&split_point(&instance.pubkey));
    let _ = instance
        .old_points
        .iter()
        .chain(instance.new_points.iter())
        .map(|p| halo2_instances.extend_from_slice(&split_point(&p)))
        .collect::<Vec<_>>();

    assert_eq!(halo2_instances.len(), INSTANCE_NUM);

    halo2_instances
}

pub fn create_proofs(
    params: &Params<bn256::G1Affine>,
    circuit: Circuit<bls12_381::G1Affine, Fr>,
    pk: &ProvingKey,
    instance: &Vec<Fr>,
) -> Vec<u8> {
    let mut transcript = Blake2bWrite::<_, _, Challenge255<_>>::init(vec![]);

    let timer = start_timer!(|| "create proof");
    plonk::create_proof(
        &params,
        &pk.pk,
        &[circuit],
        &[&[&instance]],
        OsRng,
        &mut transcript,
    )
    .expect("proof generation should not fail");
    end_timer!(timer);

    transcript.finalize()
}

pub fn verify_proof(
    params: &Params<bn256::G1Affine>,
    vk: &VerifyingKey,
    proof: &Vec<u8>,
    instance: &Vec<Fr>,
) -> Result<(), Error> {
    let params_verifier: ParamsVerifier<Bn256> = params.verifier(INSTANCE_NUM).unwrap();

    let strategy = SingleVerifier::new(&params_verifier);
    let mut transcript = Blake2bRead::<_, _, Challenge255<_>>::init(&proof[..]);

    let timer = start_timer!(|| "verify proof");
    let result = plonk::verify_proof(
        &params_verifier,
        &vk.vk,
        strategy,
        &[&[&instance]],
        &mut transcript,
    );
    end_timer!(timer);

    result
}

#[test]
fn test_circuit() {
    use halo2_proofs::pairing::group::ff::PrimeField;
    use rand::SeedableRng;
    use rand_xorshift::XorShiftRng;

    // let params = Params::<bn256::G1Affine>::unsafe_setup::<Bn256>(K);
    // let pk = ProvingKey::build(&params);

    let mut rng = XorShiftRng::seed_from_u64(0x0102030405060708);
    let tau = bls12_381::Fr::random(&mut rng);
    let from_index = 0;
    let old_points = (0..LENGTH)
        .map(|_| {
            let s = bls12_381::Fr::random(&mut rng);
            let p = bls12_381::G1Affine::generator() * s;

            p.to_affine()
        })
        .collect::<Vec<_>>();
    let mut scalar = tau.pow_vartime(&[from_index as u64, 0, 0, 0]);
    let mut new_points = vec![];
    for p in old_points.iter() {
        let new_p = p * scalar;
        new_points.push(new_p.to_affine());
        scalar = scalar * tau;
    }

    let circuit = Circuit::<bls12_381::G1Affine, Fr> {
        from_index: Some(from_index),
        tau: Some(tau),
        points: old_points.iter().map(|p| Some(*p)).collect::<Vec<_>>(),
        new_points: new_points.iter().map(|p| Some(*p)).collect::<Vec<_>>(),
        _mark: Default::default(),
    };

    let instance = generate_instance(&Instance {
        from_index,
        pubkey: (bls12_381::G1Affine::generator() * tau).to_affine(),
        old_points,
        new_points,
    });

    use halo2_proofs::dev::MockProver;
    let prover = match MockProver::run(K, &circuit, vec![instance]) {
        Ok(prover) => prover,
        Err(e) => panic!("{:#?}", e),
    };
    assert_eq!(prover.verify(), Ok(()));

    // let proof = create_proofs(&params, circuit, &pk, &instance);
    // let vk = VerifyingKey::build(&params);
    // assert!(verify_proof(&params, &vk, &proof, &instance).is_ok());
    //
    // let mut instance = instance;
    // instance[0] = Fr::from_str_vartime("1").unwrap();
    // assert!(verify_proof(&params, &vk, &proof, &instance).is_err());
}

#[test]
fn test_write_params() {
    let params = Params::<bn256::G1Affine>::unsafe_setup::<Bn256>(K);

    let mut params_buffer = vec![];
    params.write(&mut params_buffer).unwrap();
    std::fs::write("g1_params.bin", &params_buffer).expect("Write params failed");
}
