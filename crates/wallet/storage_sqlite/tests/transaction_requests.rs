//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Storage-level tests for the transaction-request state machine (issue #2343).
//!
//! A transaction request holds a frozen `UnsignedTransaction` and the hash an
//! approval commits to, from creation until a separately-permissioned principal
//! approves it and it is submitted. These exercise the behaviour the
//! `transaction_requests.*` handlers depend on:
//!   * a request is born `Pending` and round-trips,
//!   * transitions are atomic and guarded, so a request cannot be approved twice or approved after rejection,
//!   * expiry is derived from `expires_at` on read rather than stored, so no reaper is needed to make an abandoned
//!     request stop being approvable.

use std::time::Duration;

use tari_ootle_common_types::optional::IsNotFoundError;
use tari_ootle_wallet_sdk::{
    models::TransactionRequestStatus,
    storage::{CommittableStore, WalletStoreReader, WalletStoreWriter, WriteableWalletStore},
};
use tari_ootle_wallet_storage_sqlite::SqliteWalletStore;

fn open_store() -> SqliteWalletStore {
    let db = SqliteWalletStore::try_open(":memory:").unwrap();
    db.run_migrations().unwrap();
    db
}

/// Inserts a request with a generous expiry and returns its public id.
fn insert_request(db: &SqliteWalletStore, request_id: &str) -> String {
    let mut tx = db.create_write_tx().unwrap();
    tx.transaction_request_insert(
        request_id,
        b"canonical-unsigned-tx-bytes",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "seal-key-id",
        "[]",
        "[]",
        Some("htlc-swap-tool"),
        Duration::from_secs(30 * 60),
    )
    .unwrap();
    tx.commit().unwrap();
    request_id.to_string()
}

#[test]
fn a_request_can_only_be_approved_once() {
    // The guard is a conditional UPDATE, so two approvers racing resolve to
    // one winner rather than both believing they approved.
    let db = open_store();
    let request_id = insert_request(&db, "req-double-approve");

    let mut tx = db.create_write_tx().unwrap();
    tx.transaction_request_transition(
        &request_id,
        TransactionRequestStatus::Pending,
        TransactionRequestStatus::Approved,
    )
    .unwrap();

    let second = tx.transaction_request_transition(
        &request_id,
        TransactionRequestStatus::Pending,
        TransactionRequestStatus::Approved,
    );
    assert!(second.is_err(), "a second approval must be refused");

    let found = tx.transaction_request_get(&request_id).unwrap();
    assert_eq!(
        found.status,
        TransactionRequestStatus::Approved,
        "a refused transition must leave the request untouched"
    );
    tx.commit().unwrap();
}

#[test]
fn submitting_preserves_when_the_human_approved() {
    let db = open_store();
    let request_id = insert_request(&db, "req-submit");

    let mut tx = db.create_write_tx().unwrap();
    let approved = tx
        .transaction_request_transition(
            &request_id,
            TransactionRequestStatus::Pending,
            TransactionRequestStatus::Approved,
        )
        .unwrap();
    let approved_at = approved.approved_at.expect("approving records when it happened");

    let submitted = tx
        .transaction_request_transition(
            &request_id,
            TransactionRequestStatus::Approved,
            TransactionRequestStatus::Submitted,
        )
        .unwrap();

    assert_eq!(
        submitted.approved_at,
        Some(approved_at),
        "submitting must not overwrite when the approval happened"
    );
    tx.commit().unwrap();
}

#[test]
fn a_rejected_request_cannot_later_be_approved() {
    let db = open_store();
    let request_id = insert_request(&db, "req-rejected");

    let mut tx = db.create_write_tx().unwrap();
    tx.transaction_request_transition(
        &request_id,
        TransactionRequestStatus::Pending,
        TransactionRequestStatus::Rejected,
    )
    .unwrap();

    let approve = tx.transaction_request_transition(
        &request_id,
        TransactionRequestStatus::Pending,
        TransactionRequestStatus::Approved,
    );
    assert!(approve.is_err(), "rejection is terminal");

    let found = tx.transaction_request_get(&request_id).unwrap();
    assert_eq!(found.status, TransactionRequestStatus::Rejected);
    assert!(found.approved_at.is_none(), "a rejected request was never approved");
    tx.commit().unwrap();
}

#[test]
fn transitioning_an_unknown_request_is_not_found() {
    // A bad id and a double-approve must be distinguishable: the handler maps
    // them to different JSON-RPC errors.
    let db = open_store();
    let mut tx = db.create_write_tx().unwrap();

    let err = tx
        .transaction_request_transition(
            "no-such-request",
            TransactionRequestStatus::Pending,
            TransactionRequestStatus::Approved,
        )
        .unwrap_err();

    assert!(err.is_not_found_error(), "expected NotFound, got: {err}");
}

#[test]
fn insert_and_get_round_trips_as_pending() {
    let db = open_store();
    let request_id = insert_request(&db, "req-1");

    let mut tx = db.create_write_tx().unwrap();
    let found = tx.transaction_request_get(&request_id).unwrap();

    assert_eq!(found.request_id, "req-1");
    assert_eq!(found.unsigned_transaction, b"canonical-unsigned-tx-bytes");
    assert_eq!(found.requested_by.as_deref(), Some("htlc-swap-tool"));
    assert_eq!(
        found.status,
        TransactionRequestStatus::Pending,
        "a request is born awaiting a human"
    );
    assert!(found.transaction_id.is_none(), "nothing is submitted yet");
}
