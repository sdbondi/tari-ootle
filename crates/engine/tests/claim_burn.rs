//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use ootle_byte_type::ToByteType;
use tari_crypto::{
    keys::PublicKey as _,
    ristretto::{RistrettoPublicKey, RistrettoSecretKey},
};
use tari_engine::traits::{ClaimProofError, ClaimProofRejection, ClaimProofVerifier, VerifiedClaim};
use tari_engine_types::{
    commit_result::ExecutionFailureCode,
    confidential::{
        BurnOutput,
        BurnOutputFeatures,
        BurnOutputInclusionProof,
        ClaimBurnOutputData,
        MinotariBurnClaimProof,
        MmrInclusionProof,
    },
};
use tari_ootle_transaction::{Epoch, Transaction};
use tari_template_lib::types::{
    EncryptedData,
    Hash32,
    constants::TARI_TOKEN,
    crypto::{PedersenCommitmentBytes, RistrettoPublicKeyBytes, SchnorrSignatureBytes},
};
use tari_template_test_tooling::{TemplateTest, support::stealth, wallet_crypto::MaskAndValue};

const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");

struct FixedOutcomeVerifier(ClaimProofError);

impl ClaimProofVerifier for FixedOutcomeVerifier {
    fn verify_claim_proof(
        &self,
        _epoch: Epoch,
        _claim: &MinotariBurnClaimProof,
    ) -> Result<VerifiedClaim, ClaimProofError> {
        Err(self.0.clone())
    }
}

/// Accepts every claim, as a claim whose ownership proof and output inclusion verify
struct AcceptingVerifier;

impl ClaimProofVerifier for AcceptingVerifier {
    fn verify_claim_proof(
        &self,
        _epoch: Epoch,
        claim: &MinotariBurnClaimProof,
    ) -> Result<VerifiedClaim, ClaimProofError> {
        Ok(VerifiedClaim {
            claim_public_key: claim.output.features.claim_public_key,
        })
    }
}

fn keypair() -> (RistrettoSecretKey, RistrettoPublicKeyBytes) {
    let (secret, public) = RistrettoPublicKey::random_keypair(&mut rand::rng());
    (secret, public.to_byte_type())
}

/// A claim of a burn output made out to `claim_public_key`
fn claim_to(claim_public_key: RistrettoPublicKeyBytes) -> (MinotariBurnClaimProof, ClaimBurnOutputData) {
    let (mut proof, output_data) = claim();
    proof.output.features.claim_public_key = claim_public_key;
    (proof, output_data)
}

fn one_byte<const N: usize>() -> bounded_vec::BoundedVec<u8, 1, N> {
    bounded_vec::BoundedVec::<u8, 1, N>::from_vec(vec![0]).unwrap()
}

fn claim() -> (MinotariBurnClaimProof, ClaimBurnOutputData) {
    let mmr_proof = || MmrInclusionProof {
        leaf_index: 0,
        mmr_size: 1,
        path: vec![],
        peaks: vec![],
    };
    let proof = MinotariBurnClaimProof {
        commitment: PedersenCommitmentBytes::zero(),
        ownership_proof: SchnorrSignatureBytes::zero(),
        value: 1_000,
        output: BurnOutput {
            version: 0,
            features: BurnOutputFeatures {
                version: 0,
                maturity: 0,
                claim_public_key: RistrettoPublicKeyBytes::zero(),
                sidechain_id: None,
                range_proof_type: 0,
            },
            rangeproof_hash: Hash32::zero(),
            script: one_byte(),
            sender_offset_public_key: RistrettoPublicKeyBytes::zero(),
            metadata_signature: one_byte(),
            covenant: one_byte(),
            encrypted_data: one_byte(),
            minimum_value_promise: 0,
        },
        inclusion_proof: BurnOutputInclusionProof {
            block_hash: Hash32::zero(),
            normal_output_proof: mmr_proof(),
            normal_output_mr: Hash32::zero(),
            block_output_proof: mmr_proof(),
        },
    };
    let output_data = ClaimBurnOutputData {
        encrypted_data: EncryptedData::empty(),
    };
    (proof, output_data)
}

fn verifier_fault() -> FixedOutcomeVerifier {
    FixedOutcomeVerifier(ClaimProofError::VerifierFault("database unavailable".to_string()))
}

#[test]
fn a_verifier_fault_in_the_fee_intent_aborts_execution() {
    let mut test = TemplateTest::new(CRATE_PATH, &[] as &[&str]);
    test.set_claim_proof_verifier(verifier_fault());
    let (proof, output_data) = claim();

    let err = test
        .try_execute(
            Transaction::builder_localnet(Epoch(1))
                .with_fee_instructions_builder(|builder| builder.claim_burn(proof, output_data))
                .build_and_seal(test.secret_key()),
            vec![],
        )
        .expect_err("a verifier fault must not produce a receipt");
    assert!(err.is_node_fault(), "expected a node fault, got {err}");
}

#[test]
fn a_verifier_fault_in_the_main_intent_aborts_execution() {
    let mut test = TemplateTest::new(CRATE_PATH, &[] as &[&str]);
    test.set_claim_proof_verifier(verifier_fault());
    let (proof, output_data) = claim();

    let err = test
        .try_execute(
            Transaction::builder_localnet(Epoch(1))
                .claim_burn(proof, output_data)
                .build_and_seal(test.secret_key()),
            vec![],
        )
        .expect_err("a verifier fault must not produce a receipt");
    assert!(err.is_node_fault(), "expected a node fault, got {err}");
}

#[test]
fn an_invalid_claim_is_rejected_with_a_receipt() {
    let mut test = TemplateTest::new(CRATE_PATH, &[] as &[&str]);
    test.set_claim_proof_verifier(FixedOutcomeVerifier(
        ClaimProofRejection::Invalid("bad proof".to_string()).into(),
    ));
    let (proof, output_data) = claim();

    let result = test
        .try_execute(
            Transaction::builder_localnet(Epoch(1))
                .claim_burn(proof, output_data)
                .build_and_seal(test.secret_key()),
            vec![],
        )
        .expect("an invalid claim is a verdict on the transaction");
    let reason = result.finalize.any_reject().expect("an invalid claim is rejected");
    assert_eq!(
        reason.execution_failure_code(),
        Some(ExecutionFailureCode::InvalidProof),
        "unexpected reject: {reason}"
    );
}

#[test]
fn a_claim_sealed_by_the_claim_key_is_accepted() {
    let mut test = TemplateTest::new(CRATE_PATH, &[] as &[&str]);
    test.set_claim_proof_verifier(AcceptingVerifier);
    let (claim_secret, claim_public_key) = keypair();
    let (proof, output_data) = claim_to(claim_public_key);

    let result = test
        .try_execute(
            Transaction::builder_localnet(Epoch(1))
                .claim_burn(proof, output_data)
                .build_and_seal(&claim_secret),
            vec![],
        )
        .unwrap();
    if let Some(reason) = result.finalize.any_reject() {
        panic!("a claim sealed by the claim key must be accepted: {reason}");
    }
}

#[test]
fn a_claim_co_signed_by_the_claim_key_is_accepted() {
    let mut test = TemplateTest::new(CRATE_PATH, &[] as &[&str]);
    test.set_claim_proof_verifier(AcceptingVerifier);
    let (claim_secret, claim_public_key) = keypair();
    let (proof, output_data) = claim_to(claim_public_key);

    // Another key, e.g. a relayer paying the fee, seals
    let result = test
        .try_execute(
            Transaction::builder_localnet(Epoch(1))
                .claim_burn(proof, output_data)
                .add_signer(&test.to_public_key_bytes(), &claim_secret)
                .seal(test.secret_key()),
            vec![],
        )
        .unwrap();
    if let Some(reason) = result.finalize.any_reject() {
        panic!("a claim co-signed by the claim key must be accepted: {reason}");
    }
}

#[test]
fn a_claim_sealed_by_another_key_is_spendable_by_the_claim_key_alone() {
    let mut test = TemplateTest::new(CRATE_PATH, &[] as &[&str]);
    test.set_claim_proof_verifier(AcceptingVerifier);
    let (claim_secret, claim_public_key) = keypair();
    let (relayer_secret, relayer_public_key) = keypair();
    let opening = MaskAndValue {
        mask: keypair().0,
        value: 1_000,
    };
    let (mut proof, output_data) = claim_to(claim_public_key);
    proof.commitment = opening.to_commitment().to_byte_type();
    proof.value = opening.value;

    let result = test.execute_expect_success(
        Transaction::builder_localnet(Epoch(1))
            .claim_burn(proof, output_data)
            .add_signer(&relayer_public_key, &claim_secret)
            .seal(&relayer_secret),
        vec![],
    );
    let diff = result.finalize.any_accept().unwrap();
    let spend_key = diff
        .up_iter()
        .find_map(|(_, substate)| substate.substate_value().as_utxo())
        .expect("claim_burn mints a UTXO")
        .spender_public_key()
        .copied();
    assert_eq!(spend_key, Some(claim_public_key));

    let transfer = stealth::generate_transfer_data([opening.clone()], 0u64, [opening.value], 0u64);
    test.execute_expect_success(
        Transaction::builder_localnet(Epoch(1))
            .stealth_transfer(TARI_TOKEN, transfer.statement)
            .build_and_seal(&claim_secret),
        vec![],
    );
}

#[test]
fn the_burner_cannot_claim_a_burn_made_out_to_someone_else() {
    let mut test = TemplateTest::new(CRATE_PATH, &[] as &[&str]);
    // The burner knows the commitment opening, so its ownership proof verifies
    test.set_claim_proof_verifier(AcceptingVerifier);
    let (_recipient_secret, recipient_public_key) = keypair();
    let (burner_secret, burner_public_key) = keypair();
    let (proof, output_data) = claim_to(recipient_public_key);

    for transaction in [
        Transaction::builder_localnet(Epoch(1))
            .claim_burn(proof.clone(), output_data.clone())
            .build_and_seal(&burner_secret),
        Transaction::builder_localnet(Epoch(1))
            .claim_burn(proof, output_data)
            .add_signer(&burner_public_key, test.secret_key())
            .seal(&burner_secret),
    ] {
        let result = test
            .try_execute(transaction, vec![])
            .expect("an unauthorized claim is a verdict on the transaction");
        let reason = result
            .finalize
            .any_reject()
            .expect("a claim without the claim key's signature must be rejected");
        assert_eq!(
            reason.execution_failure_code(),
            Some(ExecutionFailureCode::AccessDenied),
            "unexpected reject: {reason}"
        );
    }
}
