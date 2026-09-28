//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_engine::traits::{ClaimProofError, ClaimProofRejection, ClaimProofVerifier};
use tari_engine_types::{
    commit_result::ExecutionFailureCode,
    confidential::{AbridgedTransactionKernel, ClaimBurnOutputData, EncodedMerkleProof, MinotariBurnClaimProof},
};
use tari_ootle_transaction::{Epoch, Transaction};
use tari_template_lib::types::{
    EncryptedData,
    crypto::{PedersenCommitmentBytes, RistrettoPublicKeyBytes, SchnorrSignatureBytes},
};
use tari_template_test_tooling::TemplateTest;

const CRATE_PATH: &str = env!("CARGO_MANIFEST_DIR");

struct FixedOutcomeVerifier(ClaimProofError);

impl ClaimProofVerifier for FixedOutcomeVerifier {
    fn verify_claim_proof(
        &self,
        _epoch: Epoch,
        _claimant: &RistrettoPublicKeyBytes,
        _claim: &MinotariBurnClaimProof,
    ) -> Result<(), ClaimProofError> {
        Err(self.0.clone())
    }
}

fn claim() -> (MinotariBurnClaimProof, ClaimBurnOutputData) {
    let proof = MinotariBurnClaimProof {
        burn_public_key: RistrettoPublicKeyBytes::zero(),
        commitment: PedersenCommitmentBytes::zero(),
        ownership_proof: SchnorrSignatureBytes::zero(),
        encoded_merkle_proof: EncodedMerkleProof {
            block_hash: Default::default(),
            encoded_merkle_proof: bounded_vec::BoundedVec::<u8, 1, 4096>::from_vec(vec![0]).unwrap(),
            leaf_index: 0,
        },
        kernel: AbridgedTransactionKernel {
            version: 0,
            fee: 0,
            lock_height: 0,
            excess: PedersenCommitmentBytes::zero(),
            excess_sig: SchnorrSignatureBytes::zero(),
        },
        value: 1_000,
        sender_offset_public_key: RistrettoPublicKeyBytes::zero(),
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
