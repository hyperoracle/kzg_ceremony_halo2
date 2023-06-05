use halo2_proofs::arithmetic::Field;
use std::fs;
use std::fs::File;
use std::io::Write;

use halo2_proofs::poly::commitment::Params;

use crate::circuit_g1_mul::{
    verify_proof as g1_verify_proof, Circuit as G1_Circuit, Instance as G1_Instance,
    ProvingKey as G1_PK, VerifyingKey as G1_VK, LENGTH as G1_LENGTH,
};
use crate::circuit_g2_mul::{
    verify_proof as g2_verify_proof, Circuit as G2_Circuit, Instance as G2_Instance,
    ProvingKey as G2_PK, VerifyingKey as G2_VK, LENGTH as G2_LENGTH,
};
use crate::serialization::{BatchContribution, BatchContributionJson, Decode};
use crate::{
    bls12_381, bn256, circuit_g1_mul, circuit_g2_mul, serialization, BatchTranscriptJson, Curve,
    Fr, Proof,
};
use bls12_381::Fr as Scalar;

pub fn prove(
    old_contributions: &BatchContribution,
    new_contributions: &BatchContribution,
    taus: &Vec<Scalar>,
) {
    println!("Proving");

    println!("Reading G1 params...");
    let g1_params = fs::read("g1_params.bin").expect("Read G1 params file failed");
    let g1_params = Params::<bn256::G1Affine>::read(&g1_params[..]).expect("Read G1 params failed");
    println!("Building G1 Proving Key..");
    let g1_pk = G1_PK::build(&g1_params);

    println!("Reading G2 params...");
    let g2_params = fs::read("g2_params.bin").expect("Read G2 params file failed");
    let g2_params = Params::<bn256::G1Affine>::read(&g2_params[..]).expect("Read G2 params failed");
    println!("Building G2 Proving Key..");
    let g2_pk = G2_PK::build(&g2_params);

    println!("Generating proofs...");
    let mut proofs = vec![];
    for (i, (tau, (old_contribution, new_contribution))) in taus
        .iter()
        .zip(
            old_contributions
                .contributions
                .iter()
                .zip(new_contributions.contributions.iter()),
        )
        .enumerate()
    {
        println!("Processing contributions {}...", i);

        let pubkey = (bls12_381::G1Affine::generator() * tau).to_affine();

        println!("Processing G1 proofs...");
        let number_g1_powers = old_contribution.num_g1_powers as usize;
        assert_eq!(number_g1_powers % G1_LENGTH, 0);
        let number_g1_proofs = number_g1_powers / G1_LENGTH;

        let mut proofs_g1 = Vec::with_capacity(number_g1_proofs);
        for (j, (old_points, new_points)) in old_contribution
            .powers_of_tau
            .g1_powers
            .chunks(G1_LENGTH)
            .zip(new_contribution.powers_of_tau.g1_powers.chunks(G1_LENGTH))
            .enumerate()
        {
            println!("Generating G1 proof {}.{}...", i, j);
            let from_index = i * G1_LENGTH;

            let g1_circuit = G1_Circuit::<bls12_381::G1Affine, Fr> {
                from_index: Some(from_index),
                tau: Some(tau),
                points: old_points.iter().map(|p| Some(*p)).collect::<Vec<_>>(),
                new_points: new_points.iter().map(|p| Some(*p)).collect::<Vec<_>>(),
                _mark: Default::default(),
            };

            let instances = circuit_g1_mul::generate_instance(&G1_Instance {
                from_index,
                pubkey,
                old_points: old_points.to_vec(),
                new_points: new_points.to_vec(),
            });
            let proof_g1 =
                circuit_g1_mul::create_proofs(&g1_params, g1_circuit, &g1_pk, &instances);
            proofs_g1.push(proof_g1);
        }

        println!("Processing G2 proofs...");
        let number_g2_powers = old_contribution.num_g2_powers as usize;
        assert_eq!(number_g2_powers % G2_LENGTH, 1);
        let number_g2_proofs = number_g2_powers / G2_LENGTH;

        let mut proofs_g2 = Vec::with_capacity(number_g2_proofs);
        for (j, (old_points, new_points)) in old_contribution.powers_of_tau.g2_powers[1..]
            .chunks(G2_LENGTH)
            .zip(new_contribution.powers_of_tau.g2_powers[1..].chunks(G2_LENGTH))
            .enumerate()
        {
            println!("Generating G2 proof {},{}", i, j);
            let from_index = i * G2_LENGTH + 1;

            let g2_circuit = G2_Circuit::<Fr> {
                from_index: Some(from_index),
                tau: Some(*tau),
                points: old_points.iter().map(|p| Some(*p)).collect::<Vec<_>>(),
                new_points: new_points.iter().map(|p| Some(*p)).collect::<Vec<_>>(),
                _mark: Default::default(),
            };

            let instances = circuit_g2_mul::generate_instance(&G2_Instance {
                from_index,
                pubkey,
                old_points: old_points.to_vec(),
                new_points: new_points.to_vec(),
            });
            let proof_g2 =
                circuit_g2_mul::create_proofs(&g2_params, g2_circuit, &g2_pk, &instances);
            proofs_g2.push(proof_g2);
        }

        proofs.push((proofs_g1, proofs_g2));
    }
    let serialized = serde_json::to_string(&Proof(proofs)).expect("Serialize proof failed");
    let mut file = File::create("Proof.json").expect("Create file failed");
    file.write_all(serialized.as_bytes())
        .expect("Write proof failed");
}

pub fn verify() {
    println!("Verifying");

    let old_contributions_json =
        fs::read_to_string("old_contributions.json").expect("should exist");
    let old_contributions_json: BatchContributionJson =
        serde_json::from_str(&old_contributions_json).expect("Deserialize failed");
    let old_contributions = old_contributions_json.decode();

    let new_contributions_json =
        fs::read_to_string("new_contributions.json").expect("should exist");
    let new_contributions_json: BatchContributionJson =
        serde_json::from_str(&new_contributions_json).expect("Deserialize failed");
    let new_contributions = new_contributions_json.decode();

    let proof_file = fs::read_to_string("Proof.json").expect("should exist");
    let proofs: Proof = serde_json::from_str(&proof_file).expect("Deserialize proof failed");

    println!("Reading G1 params...");
    let g1_params = fs::read("g1_params.bin").expect("Read G1 params file failed");
    let g1_params = Params::<bn256::G1Affine>::read(&g1_params[..]).expect("Read G1 params failed");
    println!("Building G1 Verification Key..");
    let g1_vk = G1_VK::build(&g1_params);

    println!("Reading G2 params...");
    let g2_params = fs::read("g2_params.bin").expect("Read G2 params file failed");
    let g2_params = Params::<bn256::G1Affine>::read(&g2_params[..]).expect("Read G2 params failed");
    println!("Building G2 Verification Key..");
    let g2_vk = G2_VK::build(&g2_params);

    assert_eq!(proofs.0.len(), new_contributions.contributions.len());
    for (proof, (old_contribution, new_contribution)) in proofs.0.iter().zip(
        old_contributions
            .contributions
            .iter()
            .zip(new_contributions.contributions.iter()),
    ) {
        let pubkey = {
            let str = new_transcript
                .witness
                .pot_pubkeys
                .last()
                .expect("Should exist");

            let bytes = hex::decode(&str[2..]).expect("Failed to decode point in hex string");

            bls12_381::G1Affine::from_compressed(&bytes.try_into().expect("Error length"))
                .expect("Deserialize pubkey failed")
        };

        let num_chunks = new_transcript.num_g1_powers as usize / G1_LENGTH;
        assert_eq!(proof.0.len(), num_chunks);
        assert_eq!(new_transcript.num_g1_powers as usize % G1_LENGTH, 0);

        for (i, (proof_g1, (old_g1_transcript, new_g1_transcript))) in proof
            .0
            .iter()
            .zip(
                old_transcript
                    .powers_of_tau
                    .g1_powers
                    .chunks(G1_LENGTH)
                    .zip(new_transcript.powers_of_tau.g1_powers.chunks(G1_LENGTH)),
            )
            .enumerate()
        {
            let old_points = serialization::decode_g1_points(old_g1_transcript);
            let new_points = serialization::decode_g1_points(new_g1_transcript);

            let instances = circuit_g1_mul::generate_instance(&G1_Instance {
                from_index: i * G1_LENGTH,
                pubkey,
                old_points,
                new_points,
            });

            g1_verify_proof(&g1_params, &g1_vk, &proof_g1, &instances).unwrap();
        }

        let num_chunks = new_transcript.num_g2_powers as usize / G2_LENGTH;
        assert_eq!(proof.1.len(), num_chunks);
        assert_eq!(new_transcript.num_g2_powers as usize % G2_LENGTH, 1);

        assert_eq!(
            old_transcript.powers_of_tau.g2_powers[0],
            new_transcript.powers_of_tau.g2_powers[0]
        );
        for (i, (proof_g2, (old_g2_transcript, new_g2_transcript))) in proof
            .1
            .iter()
            .zip(
                old_transcript.powers_of_tau.g2_powers[1..]
                    .chunks(G2_LENGTH)
                    .zip(new_transcript.powers_of_tau.g2_powers[1..].chunks(G2_LENGTH)),
            )
            .enumerate()
        {
            let old_points = serialization::decode_g2_points(old_g2_transcript);
            let new_points = serialization::decode_g2_points(new_g2_transcript);

            let instances = circuit_g2_mul::generate_instance(&G2_Instance {
                from_index: i * G2_LENGTH + 1,
                pubkey,
                old_points,
                new_points,
            });

            g2_verify_proof(&g2_params, &g2_vk, &proof_g2, &instances).unwrap();
        }
    }
}

#[tokio::main]
async fn pull_transcripts() -> BatchTranscriptJson {
    println!("Pulling transcripts...");

    let transcripts =
        match reqwest::get("https://seq.ceremony.ethereum.org/info/current_state").await {
            Ok(resp) => resp.json().await.unwrap(),
            Err(err) => panic!("Error: {}", err),
        };

    transcripts
}