//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Create / approve / submit for transaction requests (issue #2343).
//!
//! A limited-permission tool creates a request; a principal holding
//! `transaction_requests:approve` authorises it; submit seals it. The three are
//! separately permissioned, so a tool granted only `:create` cannot approve the
//! requests it creates.
//!
//! The transaction is **frozen at creation**: inputs are detected, the seal flag
//! is normalised, and the resulting bytes are hashed. An approval commits to
//! that hash and submit re-checks it, so the bytes a person approved are the
//! bytes that get sealed. Nothing between approve and submit may rewrite them.

use std::time::Duration;

use axum_extra::headers::authorization::Bearer;
use blake2::{Blake2b, Digest, digest::consts::U32};
use log::*;
use tari_bor::encode;
use tari_ootle_common_types::optional::Optional;
use tari_ootle_transaction::{TransactionId, UnsignedTransaction};
use tari_ootle_transaction_validation::check_stealth_limits;
use tari_ootle_wallet_sdk::{
    models::{
        EffectiveStatus,
        OutputStatus,
        TransactionRequestCreatedEvent,
        TransactionRequestModel,
        TransactionRequestStatus,
        WalletEvent,
        WalletLockId,
    },
    storage::{CommittableStore, ReadableWalletStore, WalletStoreReader, WalletStoreWriter, WriteableWalletStore},
};
use tari_ootle_walletd_client::{
    permissions::{Permission, TxRequestAction},
    types::{
        TransactionRequestCreateRequest,
        TransactionRequestCreateResponse,
        TransactionRequestDecisionRequest,
        TransactionRequestDecisionResponse,
        TransactionRequestGetRequest,
        TransactionRequestGetResponse,
        TransactionRequestInfo,
        TransactionRequestListRequest,
        TransactionRequestListResponse,
        TransactionRequestSubmitRequest,
        TransactionRequestSubmitResponse,
        TransactionRequestValueSummary,
        TransactionSubmitRequest,
    },
};

use super::context::HandlerContext;
use crate::handlers::{
    helpers::{invalid_params, invalid_request},
    transaction::{derive_stealth_signers, submit_inner_for_request},
};

const LOG_TARGET: &str = "tari::ootle::wallet_daemon::handlers::transaction_requests";

/// Extra margin on the locks beyond the approval window.
///
/// The lock must outlive the request, never the reverse: a lock released under
/// an Approved request would let submit sign a spend of UTXOs that are
/// spendable again, whereas a lock outliving a dead request merely idles funds
/// until the stale sweep takes it.
const LOCK_GRACE: Duration = Duration::from_secs(5 * 60);

pub async fn handle_create(
    context: &HandlerContext,
    token: Option<&Bearer>,
    req: TransactionRequestCreateRequest,
) -> Result<TransactionRequestCreateResponse, anyhow::Error> {
    let auth = context.authorize_with_identity(token, &[Permission::TransactionRequests(TxRequestAction::Create)])?;
    let sdk = context.wallet_sdk();

    req.transaction
        .validate_blob_references()
        .map_err(|e| invalid_params("transaction.blobs", Some(e.to_string())))?;

    // Detect inputs NOW, not at submit. Submit must not rewrite the approved
    // bytes, so anything that changes the transaction happens before the hash
    // is taken. Safe to freeze early because detected inputs are unversioned by
    // default and consensus resolves them.
    let detected_inputs = if req.detect_inputs {
        let substates = req.transaction.to_referenced_substates()?;
        let substates = substates
            .into_iter()
            .chain(req.transaction.inputs().iter().map(|r| r.substate_id().clone()))
            .collect::<Vec<_>>();
        sdk.substate_api()
            .locate_dependent_substates(&substates, req.detect_inputs_use_unversioned)
            .await?
            .into_iter()
            .map(|input| {
                if req.detect_inputs_use_unversioned {
                    input.into_unversioned()
                } else {
                    input
                }
            })
            .collect()
    } else {
        vec![]
    };

    let mut unsigned = context
        .transaction_builder()
        .with_unsigned_transaction(req.transaction)
        .with_inputs(detected_inputs)
        .build_unsigned();

    // Derive the signer set the same way submit will, so normalisation below
    // settles on the value `seal()` will settle on.
    let stealth_signers = derive_stealth_signers(sdk, &req.lock_ids)?;
    let has_signers = !req.other_signers.is_empty() || !stealth_signers.is_empty();
    normalize_seal_authorization(&mut unsigned, has_signers);

    // Reject work the network would reject anyway, before a human is asked to
    // look at it. These caps depend only on the instructions, so they can be
    // checked unsigned; otherwise an over-cap request is only detectable after
    // sealing -- i.e. after approval.
    check_stealth_limits(unsigned.fee_instructions(), unsigned.instructions()).map_err(|v| {
        invalid_params(
            "transaction",
            Some(format!(
                "stealth {} {} exceeds the maximum of {}",
                v.limit, v.actual, v.max
            )),
        )
    })?;

    let bytes = encode(&unsigned).map_err(|e| invalid_params("transaction", Some(e.to_string())))?;
    let hash = hash_unsigned(&bytes);

    let ttl = req
        .ttl_secs
        .map(Duration::from_secs)
        .unwrap_or(context.config().transaction_request_ttl);
    let request_id = new_request_id();

    let model = {
        let mut tx = sdk.store().create_write_tx()?;

        // Hold the inputs across the approval window. The transfer-selection
        // handlers lock for five minutes, which is a selection deadline, not a
        // deadline a person can meet.
        for lock_id in &req.lock_ids {
            tx.locks_set_timeout(*lock_id, Some(ttl + LOCK_GRACE))?;
        }

        let model = tx.transaction_request_insert(
            &request_id,
            &bytes,
            &hash,
            &serde_json::to_string(&req.seal_signer)?,
            &serde_json::to_string(&req.other_signers)?,
            &serde_json::to_string(&req.lock_ids)?,
            auth.api_key_name.as_deref(),
            ttl,
        )?;
        tx.commit()?;
        model
    };

    info!(
        target: LOG_TARGET,
        "Transaction request {} created by {} ({} lock(s), expires {})",
        request_id,
        auth.api_key_name.as_deref().unwrap_or("a wallet session"),
        req.lock_ids.len(),
        model.expires_at,
    );

    context
        .notifier()
        .notify(WalletEvent::TransactionRequestCreated(TransactionRequestCreatedEvent {
            request_id: request_id.clone(),
            requested_by: auth.api_key_name.clone(),
        }));

    Ok(TransactionRequestCreateResponse {
        request_id,
        transaction_hash: hash,
        expires_at: model.expires_at.assume_utc().unix_timestamp(),
    })
}

pub async fn handle_get(
    context: &HandlerContext,
    token: Option<&Bearer>,
    req: TransactionRequestGetRequest,
) -> Result<TransactionRequestGetResponse, anyhow::Error> {
    context.authorize(token, &[Permission::TransactionRequests(TxRequestAction::Read)])?;

    let model = context
        .wallet_sdk()
        .store()
        .with_read_tx(|tx| tx.transaction_request_get(&req.request_id))?;

    Ok(TransactionRequestGetResponse {
        request: to_info(context, model)?,
    })
}

pub async fn handle_list(
    context: &HandlerContext,
    token: Option<&Bearer>,
    req: TransactionRequestListRequest,
) -> Result<TransactionRequestListResponse, anyhow::Error> {
    context.authorize(token, &[Permission::TransactionRequests(TxRequestAction::Read)])?;

    let models = context
        .wallet_sdk()
        .store()
        .with_read_tx(|tx| tx.transaction_requests_list())?;

    let requests = models
        .into_iter()
        .map(|m| to_info(context, m))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        // Expiry is derived, so it cannot be filtered in SQL.
        .filter(|info| req.status.is_none_or(|s| s == info.status))
        .collect();

    Ok(TransactionRequestListResponse { requests })
}

pub async fn handle_approve(
    context: &HandlerContext,
    token: Option<&Bearer>,
    req: TransactionRequestDecisionRequest,
) -> Result<TransactionRequestDecisionResponse, anyhow::Error> {
    // Deliberately NOT `authorize_user_only`: approve is grantable to a tool.
    // The control is who holds the scope, not what kind of credential they are.
    context.authorize(token, &[Permission::TransactionRequests(TxRequestAction::Approve)])?;

    let model = transition(
        context,
        &req.request_id,
        req.transaction_hash.as_deref(),
        TransactionRequestStatus::Pending,
        TransactionRequestStatus::Approved,
    )?;

    info!(target: LOG_TARGET, "Transaction request {} approved", req.request_id);

    Ok(TransactionRequestDecisionResponse {
        request_id: req.request_id,
        status: model.effective_status_now(),
    })
}

pub async fn handle_reject(
    context: &HandlerContext,
    token: Option<&Bearer>,
    req: TransactionRequestDecisionRequest,
) -> Result<TransactionRequestDecisionResponse, anyhow::Error> {
    context.authorize(token, &[Permission::TransactionRequests(TxRequestAction::Approve)])?;

    let model = transition(
        context,
        &req.request_id,
        req.transaction_hash.as_deref(),
        TransactionRequestStatus::Pending,
        TransactionRequestStatus::Rejected,
    )?;

    // Release the inputs immediately rather than idling them until the sweep:
    // a refused request is never going to spend them.
    for lock_id in parse_lock_ids(&model)? {
        if let Err(e) = context.wallet_sdk().locks_api().release_lock(lock_id) {
            warn!(
                target: LOG_TARGET,
                "Failed to release lock {lock_id} for rejected request {}: {e}", req.request_id,
            );
        }
    }

    Ok(TransactionRequestDecisionResponse {
        request_id: req.request_id,
        status: model.effective_status_now(),
    })
}

pub async fn handle_submit(
    context: &HandlerContext,
    token: Option<&Bearer>,
    req: TransactionRequestSubmitRequest,
) -> Result<TransactionRequestSubmitResponse, anyhow::Error> {
    // Submitting an approved request is not the same authority as
    // `transactions:create` (unrestricted submit): the bytes are frozen and
    // hash-committed, so this confers no ability to alter what gets signed.
    context.authorize(token, &[Permission::TransactionRequests(TxRequestAction::Create)])?;
    let sdk = context.wallet_sdk();

    let model = sdk
        .store()
        .with_read_tx(|tx| tx.transaction_request_get(&req.request_id))?;

    match model.effective_status_now() {
        EffectiveStatus::Approved => {},
        EffectiveStatus::Expired => {
            return Err(invalid_request(format!(
                "Transaction request {} expired at {}",
                req.request_id, model.expires_at
            )));
        },
        other => {
            return Err(invalid_request(format!(
                "Transaction request {} is {other:?}, expected Approved",
                req.request_id
            )));
        },
    }

    let unsigned: UnsignedTransaction = tari_bor::decode(&model.unsigned_transaction).map_err(|e| {
        invalid_request(format!(
            "Transaction request {} holds undecodable bytes: {e}",
            req.request_id
        ))
    })?;

    // The bytes are the approved artifact; re-hash rather than trust the
    // column, so a tampered row cannot be submitted as if approved.
    let hash = hash_unsigned(&model.unsigned_transaction);
    if hash != model.transaction_hash {
        return Err(invalid_request(format!(
            "Transaction request {} does not match the hash that was approved",
            req.request_id
        )));
    }

    let lock_ids = parse_lock_ids(&model)?;
    let seal_signer = serde_json::from_str(&model.seal_signer)?;
    let other_signers: Vec<_> = serde_json::from_str(&model.other_signers)?;

    // Backstop for the seal-flag rewrite: `seal()` forces
    // `is_seal_signer_authorized = true` when no other signature is attached,
    // and that flag is inside the signing domain. Creation normalised it for
    // the signer set as it stood then; if the set has since emptied (a lock
    // reaped, say) sealing would silently rewrite the approved bytes and
    // promote the seal signer to owner authority. Refuse instead.
    let stealth_signers = derive_stealth_signers(sdk, &lock_ids)?;
    let has_signers = !other_signers.is_empty() || !stealth_signers.is_empty();
    let mut check = unsigned.clone();
    normalize_seal_authorization(&mut check, has_signers);
    if encode(&check).map_err(|e| invalid_request(e.to_string()))? != model.unsigned_transaction {
        return Err(invalid_request(format!(
            "Transaction request {} would be rewritten at seal time and no longer matches what was approved",
            req.request_id
        )));
    }

    let submit = TransactionSubmitRequest {
        transaction: unsigned,
        seal_signer,
        other_signers,
        // Everything was resolved at creation. Detecting again here would
        // rewrite the approved input set.
        detect_inputs: false,
        detect_inputs_use_unversioned: true,
        lock_ids,
    };

    let response = submit_inner_for_request(context, submit).await?;

    sdk.store().with_write_tx(|tx| {
        tx.transaction_request_transition(
            &req.request_id,
            TransactionRequestStatus::Approved,
            TransactionRequestStatus::Submitted,
        )
    })?;

    info!(
        target: LOG_TARGET,
        "Transaction request {} submitted as {}", req.request_id, response.transaction_id,
    );

    Ok(TransactionRequestSubmitResponse {
        transaction_id: response.transaction_id,
    })
}

/// Apply `seal()`'s own rule up front.
///
/// `UnsealedTransactionV1::seal` forces `is_seal_signer_authorized = true` when
/// there are no other signatures. That field is part of the signed bytes, so
/// leaving it false in a request that will attract no co-signers would mean the
/// bytes a human approved are not the bytes that get sealed -- and the rewrite
/// promotes the seal signer to the transaction's owner authority. Settle it
/// here so the approver sees the value that will actually be sealed.
fn normalize_seal_authorization(unsigned: &mut UnsignedTransaction, has_signers: bool) {
    if !has_signers {
        unsigned.set_seal_signer_authorized(true);
    }
}

fn hash_unsigned(bytes: &[u8]) -> String {
    let mut hasher = Blake2b::<U32>::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn new_request_id() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn parse_lock_ids(model: &TransactionRequestModel) -> Result<Vec<WalletLockId>, anyhow::Error> {
    Ok(serde_json::from_str(&model.lock_ids)?)
}

/// Guarded transition, optionally pinned to the hash the caller saw.
fn transition(
    context: &HandlerContext,
    request_id: &str,
    expect_hash: Option<&str>,
    from: TransactionRequestStatus,
    to: TransactionRequestStatus,
) -> Result<TransactionRequestModel, anyhow::Error> {
    let sdk = context.wallet_sdk();

    let current = sdk.store().with_read_tx(|tx| tx.transaction_request_get(request_id))?;

    // A decision on a request that has already timed out is not a decision.
    if matches!(current.effective_status_now(), EffectiveStatus::Expired) {
        return Err(invalid_request(format!(
            "Transaction request {request_id} expired at {}",
            current.expires_at
        )));
    }

    // The approver acts on what they were shown. If the UI rendered a stale
    // request, refuse rather than authorise something else.
    if let Some(expected) = expect_hash &&
        expected != current.transaction_hash
    {
        return Err(invalid_request(format!(
            "Transaction request {request_id} no longer matches the hash you were shown",
        )));
    }

    let model = sdk
        .store()
        .with_write_tx(|tx| tx.transaction_request_transition(request_id, from, to))?;

    Ok(model)
}

/// The wallet-computed view of a request, including what it moves.
fn to_info(context: &HandlerContext, model: TransactionRequestModel) -> Result<TransactionRequestInfo, anyhow::Error> {
    let transaction: UnsignedTransaction = tari_bor::decode(&model.unsigned_transaction)?;
    let status = model.effective_status_now();
    let lock_ids = parse_lock_ids(&model)?;

    Ok(TransactionRequestInfo {
        request_id: model.request_id.clone(),
        transaction,
        transaction_hash: model.transaction_hash.clone(),
        seal_signer: serde_json::from_str(&model.seal_signer)?,
        other_signers: serde_json::from_str(&model.other_signers)?,
        requested_by: model.requested_by.clone(),
        status,
        transaction_id: model
            .transaction_id
            .as_deref()
            .and_then(|s| TransactionId::from_hex(s).ok()),
        value_summary: value_summary(context, &lock_ids)?,
        expires_at: model.expires_at.assume_utc().unix_timestamp(),
        approved_at: model.approved_at.map(|t| t.assume_utc().unix_timestamp()),
        created_at: model.created_at.assume_utc().unix_timestamp(),
    })
}

/// What actually leaves the wallet, derived from the request's locks.
///
/// A stealth transfer's instructions are commitments and range proofs; the
/// wallet owns the masks and is the only party that can say "10,000 µT leaves
/// this account". Without this the approver is authorising an opaque blob.
///
/// `None` for an ordinary transaction, whose instructions are readable as-is.
fn value_summary(
    context: &HandlerContext,
    lock_ids: &[WalletLockId],
) -> Result<Option<TransactionRequestValueSummary>, anyhow::Error> {
    let sdk = context.wallet_sdk();
    let mut inputs_total = 0u64;
    let mut change_total = 0u64;
    let mut resource_address = None;

    for lock_id in lock_ids {
        let Some(outputs) = sdk.stealth_outputs_api().get_locked_by_lock_id(*lock_id).optional()? else {
            continue;
        };
        for output in outputs {
            resource_address.get_or_insert(output.resource_address);
            match output.status {
                // What this request spends.
                OutputStatus::LockedForSpend => inputs_total = inputs_total.saturating_add(output.value),
                // Outputs the statement created. Only the ones we hold a spend
                // key for are change coming back; the rest belong to the
                // recipient and are genuinely leaving.
                OutputStatus::LockedUnconfirmed if output.owner_key_id.is_some() => {
                    change_total = change_total.saturating_add(output.value)
                },
                _ => {},
            }
        }
    }

    let Some(resource_address) = resource_address else {
        return Ok(None);
    };

    Ok(Some(TransactionRequestValueSummary {
        resource_address,
        inputs_total,
        change_total,
        amount_leaving: inputs_total.saturating_sub(change_total),
    }))
}
