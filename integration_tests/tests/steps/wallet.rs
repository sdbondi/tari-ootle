//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::time::Duration;

use anyhow::anyhow;
use cucumber::{gherkin::Step, given, when};
use integration_tests::{claim_proof::CucumberClaimProof, cucumber_log};
use minotari_app_grpc::{
    tari_rpc,
    tari_rpc::{GetBalanceRequest, ValidateRequest},
};
use tari_common_types::{burn_proof::BurnOutputProof, types::CompressedPublicKey};
use tari_crypto::{
    ristretto::{CompressedRistrettoSchnorr, RistrettoSecretKey},
    tari_utilities::ByteArray,
};
use tari_ootle_app_utilities::burn_claim_proof::claim_proof_from_l1;
use tari_ootle_walletd_client::types::{ClaimBurnProof, ClaimBurnProofContents};
use tari_sidechain::{BurnClaimProof, CompleteClaimBurnProof};
use tari_template_lib_types::{EncryptedData, crypto::PedersenCommitmentBytes};
use tari_transaction_components::{
    tari_amount::T,
    transaction_components::{MemoField, memo_field::TxType},
};
use tokio::time::sleep;

use crate::{TariWorld, spawn_minotari_wallet};

#[given(expr = "a wallet {word} connected to base node {word}")]
async fn start_wallet(world: &mut TariWorld, step: &Step, wallet_name: String, bn_name: String) {
    cucumber_log!("==== Step: {}", step.value);
    spawn_minotari_wallet(world, wallet_name, bn_name).await;
}

#[when(expr = "I burn {int}T on wallet {word} to proof {word} for account {word} using wallet daemon {word}")]
async fn when_i_burn_on_wallet_for_account(
    world: &mut TariWorld,
    step: &Step,
    amount: u64,
    wallet_name: String,
    proof_name: String,
    account_name: String,
    walletd_name: String,
) {
    cucumber_log!("==== Step: {}", step.value);
    let walletd = world.get_wallet_daemon(&walletd_name);
    let mut walletd_client = walletd.get_authed_client().await;
    let account = walletd_client
        .accounts_get(account_name.clone().into())
        .await
        .unwrap_or_else(|e| panic!("Account {} not found: {}", account_name, e));
    let claim_public_key = account.account.owner_public_key().as_bytes().to_vec();
    burn_and_store_proof(world, amount, &wallet_name, proof_name, claim_public_key).await;
}

#[when(expr = "I burn {int}T on wallet {word} to proof {word} for wallet daemon {word}")]
async fn when_i_burn_on_wallet(
    world: &mut TariWorld,
    step: &Step,
    amount: u64,
    wallet_name: String,
    proof_name: String,
    walletd_name: String,
) {
    cucumber_log!("==== Step: {}", step.value);
    let walletd = world.get_wallet_daemon(&walletd_name);
    let mut walletd_client = walletd.get_authed_client().await;
    let account = walletd_client.accounts_get_default().await.unwrap();
    let claim_public_key = account.account.owner_public_key().as_bytes().to_vec();
    burn_and_store_proof(world, amount, &wallet_name, proof_name, claim_public_key).await;
}

async fn burn_and_store_proof(
    world: &mut TariWorld,
    amount: u64,
    wallet_name: &str,
    proof_name: String,
    claim_public_key: Vec<u8>,
) {
    let wallet = world
        .wallets
        .get(wallet_name)
        .unwrap_or_else(|| panic!("Wallet {} not found", wallet_name));

    let burn_amount = amount * T;
    let mut wallet_client = wallet.create_client().await;
    let resp = wallet_client
        .create_burn_transaction(minotari_app_grpc::tari_rpc::CreateBurnTransactionRequest {
            amount: burn_amount.as_u64(),
            fee_per_gram: 1,
            payment_id: MemoField::new_open("Burn".as_bytes().to_vec(), TxType::Burn)
                .unwrap()
                .to_bytes(),
            claim_public_key,
            sidechain_deployment_key: vec![],
        })
        .await
        .unwrap()
        .into_inner();

    assert!(resp.is_success);

    let kernel_excess_sig_nonce = resp.kernel_excess_nonce.clone();
    let kernel_excess_sig_signature = resp.kernel_excess_signature.clone();

    integration_tests::cucumber_log!(
        "Burn transaction created with kernel_excess_sig nonce: {}, signature: {}",
        hex::encode(&kernel_excess_sig_nonce),
        hex::encode(&kernel_excess_sig_signature)
    );

    world.claim_proofs.insert(proof_name, CucumberClaimProof::Pending {
        commitment: PedersenCommitmentBytes::from_bytes(&resp.commitment).unwrap(),
        kernel_excess_sig_nonce,
        kernel_excess_sig_signature,
    });
}

#[when(expr = "I wait for proof {word} to confirm on wallet {word}")]
#[allow(clippy::too_many_lines)]
async fn when_i_wait_for_proof_to_confirm_on_wallet(
    world: &mut TariWorld,
    step: &Step,
    proof_name: String,
    wallet_name: String,
) -> anyhow::Result<()> {
    cucumber_log!("==== Step: {}", step.value);
    let proof = world.claim_proofs.get(&proof_name).unwrap_or_else(|| {
        panic!("Claim proof {} not found", proof_name);
    });

    let CucumberClaimProof::Pending { commitment, .. } = proof else {
        // Already confirmed
        return Ok(());
    };

    let wallet = world
        .wallets
        .get(&wallet_name)
        .unwrap_or_else(|| panic!("Wallet {} not found", wallet_name));

    let mut client = wallet.create_client().await;

    let mut attempts = 0;
    let proof_resp = loop {
        let resp = client
            .get_burn_claim_proof(tari_rpc::GetBurnClaimProofRequest {
                commitment: commitment.as_bytes().to_vec(),
            })
            .await
            .unwrap()
            .into_inner();

        cucumber_log!("Received burn claim proof response: {:?}", resp);

        if resp.burn_output_proof.is_some() && resp.mined_in_epoch.is_some() {
            break resp;
        }
        if attempts >= 20 {
            return Err(anyhow!(
                "Burn output proof not available after waiting for {} attempts",
                attempts
            ));
        }
        attempts += 1;

        cucumber_log!("Burn output proof not available yet, waiting...");
        sleep(Duration::from_secs(3)).await;
    };
    let claim_proof = proof_resp
        .claim_proof
        .ok_or_else(|| anyhow!("No claim proof in response"))?;
    let ownership_proof = claim_proof
        .ownership_proof
        .ok_or_else(|| anyhow!("No ownership proof in response"))?;
    let ownership_proof = CompressedRistrettoSchnorr::new(
        CompressedPublicKey::from_canonical_bytes(&ownership_proof.public_nonce)
            .map_err(|e| anyhow!("sig public_nonce parse error {e}"))?,
        RistrettoSecretKey::from_canonical_bytes(&ownership_proof.signature)
            .map_err(|e| anyhow!("sig parse error {e}"))?,
    );
    let reciprocal_claim_public_key = CompressedPublicKey::from_canonical_bytes(&claim_proof.claim_public_key)
        .map_err(|e| anyhow!("reciprocal_claim_public_key parse error {e}"))?;
    let output_proof = proof_resp
        .burn_output_proof
        .ok_or_else(|| anyhow!("No burn output proof in response"))?;

    // The on-disk file format (CompleteClaimBurnProof) for the auto-claim integration tests
    let complete_proof = CompleteClaimBurnProof {
        claim_proof: BurnClaimProof {
            burn_public_key: reciprocal_claim_public_key,
            ownership_proof,
            output_proof: BurnOutputProof::try_from(output_proof)
                .map_err(|e| anyhow!("burn output proof parse error: {e}"))?,
            value: proof_resp.value,
        },
        encrypted_data: proof_resp.encrypted_data.clone(),
        mined_in_epoch: proof_resp
            .mined_in_epoch
            .ok_or_else(|| anyhow!("No mined_in_epoch in response"))?,
    };

    let proof = ClaimBurnProofContents {
        claim_proof: claim_proof_from_l1(&complete_proof.claim_proof)
            .map_err(|e| anyhow!("burn claim proof conversion error: {e}"))?,
        encrypted_data: EncryptedData::try_from(proof_resp.encrypted_data)
            .map_err(|e| anyhow!("Encrypted data length is out of bounds: {e}",))?,
    };

    world.claim_proofs.insert(proof_name, CucumberClaimProof::Confirmed {
        proof: ClaimBurnProof::Contents(Box::new(proof)),
        complete_proof: Box::new(complete_proof),
    });

    Ok(())
}

#[when(expr = "wallet {word} has at least {int} {word}")]
pub async fn check_balance(world: &mut TariWorld, step: &Step, wallet_name: String, balance: u64, units: String) {
    cucumber_log!("==== Step: {}", step.value);
    const MAX_WAIT_TIME_SECS: u64 = 100;
    let wallet = world
        .wallets
        .get(&wallet_name)
        .unwrap_or_else(|| panic!("Wallet {} not found", wallet_name));

    let mut client = wallet.create_client().await;
    let mut iterations = 0;
    let balance = match units.as_str() {
        "T" => balance * 1_000_000,
        "uT" => balance,
        _ => panic!("Unknown unit {}", units),
    };

    loop {
        let _result = client.validate_all_transactions(ValidateRequest {}).await.unwrap();
        let resp = client
            .get_balance(GetBalanceRequest { payment_id: None })
            .await
            .unwrap()
            .into_inner();
        if resp.available_balance >= balance {
            break;
        }
        cucumber_log!(
            "Waiting for wallet {} to have at least {} uT (balance: {} uT, pending: {} uT)",
            wallet_name,
            balance,
            resp.available_balance,
            resp.pending_incoming_balance
        );
        sleep(Duration::from_secs(2)).await;

        if iterations == MAX_WAIT_TIME_SECS.div_ceil(2) {
            panic!(
                "Wallet {} did not have at least {} uT after {} seconds  (balance: {} uT, pending: {} uT)",
                wallet_name, balance, MAX_WAIT_TIME_SECS, resp.available_balance, resp.pending_incoming_balance
            );
        }
        iterations += 1;
    }
}
