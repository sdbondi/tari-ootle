//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Tests for the vote-equivocation evidence column families.

pub mod helpers;

use helpers::{create_rocksdb, create_rocksdb_with_opts};
use tari_consensus_types::{BlockId, ProposalVote, TimeoutVote, ValidatorSignatureBytes};
use tari_ootle_common_types::{Epoch, NodeHeight};
use tari_ootle_storage::{
    StateStore,
    StateStoreReadTransaction,
    StateStoreWriteTransaction,
    consensus_models::{EquivocatingVotes, VoteEquivocation, VoteEquivocationKind},
};
use tari_sidechain::QuorumDecision;
use tari_state_store_rocksdb::DatabaseOptions;
use tari_template_lib_types::crypto::{RistrettoPublicKeyBytes, Scalar32Bytes, SchnorrSignatureBytes};

const EPOCH: Epoch = Epoch(7);
const HEIGHT: NodeHeight = NodeHeight(42);

fn signer(byte: u8) -> RistrettoPublicKeyBytes {
    RistrettoPublicKeyBytes::from_bytes(&[byte; 32]).unwrap()
}

fn signature(signer_byte: u8, nonce_byte: u8) -> ValidatorSignatureBytes {
    ValidatorSignatureBytes::new(
        signer(signer_byte),
        SchnorrSignatureBytes::new([nonce_byte; 32].into(), Scalar32Bytes::zero()),
    )
}

fn proposal_vote(signer_byte: u8, nonce_byte: u8, block_byte: u8) -> ProposalVote {
    ProposalVote {
        epoch: EPOCH,
        block_id: BlockId::new(tari_common_types::types::FixedHash::new([block_byte; 32])),
        block_height: HEIGHT,
        decision: QuorumDecision::Accept,
        signature: signature(signer_byte, nonce_byte),
    }
}

fn timeout_vote(signer_byte: u8, nonce_byte: u8) -> TimeoutVote {
    TimeoutVote {
        epoch: EPOCH,
        height: HEIGHT,
        signature: signature(signer_byte, nonce_byte),
    }
}

fn proposal_evidence(signer_byte: u8) -> VoteEquivocation {
    VoteEquivocation::new(EPOCH, HEIGHT, signer(signer_byte), EquivocatingVotes::Proposal {
        first: proposal_vote(signer_byte, 1, 0xA),
        second: proposal_vote(signer_byte, 2, 0xB),
    })
}

fn timeout_evidence(signer_byte: u8) -> VoteEquivocation {
    VoteEquivocation::new(EPOCH, HEIGHT, signer(signer_byte), EquivocatingVotes::Timeout {
        first: timeout_vote(signer_byte, 1),
        second: timeout_vote(signer_byte, 2),
    })
}

#[test]
fn record_round_trips_both_votes() {
    let (db, _tmp) = create_rocksdb();
    let evidence = proposal_evidence(1);
    assert!(db.with_write_tx(|tx| tx.vote_equivocation_record(&evidence)).unwrap());

    let stored = db
        .with_read_tx(|tx| tx.vote_equivocations_get_all_for_epoch(EPOCH))
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].epoch, EPOCH);
    assert_eq!(stored[0].height, HEIGHT);
    assert_eq!(stored[0].public_key, signer(1));
    assert_eq!(stored[0].kind(), VoteEquivocationKind::Proposal);
    let EquivocatingVotes::Proposal { first, second } = &stored[0].votes else {
        panic!("expected a proposal vote pair");
    };
    assert_eq!(first.signature, signature(1, 1));
    assert_eq!(second.signature, signature(1, 2));
}

/// An equivocator can sign arbitrarily many conflicting votes for one view; only the first pair is
/// kept so that what it makes this node store is bounded.
#[test]
fn the_first_evidence_for_a_view_and_signer_is_kept() {
    let (db, _tmp) = create_rocksdb();
    let first = proposal_evidence(1);
    let mut later = proposal_evidence(1);
    later.votes = EquivocatingVotes::Proposal {
        first: proposal_vote(1, 3, 0xC),
        second: proposal_vote(1, 4, 0xD),
    };

    assert!(db.with_write_tx(|tx| tx.vote_equivocation_record(&first)).unwrap());
    assert!(!db.with_write_tx(|tx| tx.vote_equivocation_record(&later)).unwrap());

    let stored = db
        .with_read_tx(|tx| tx.vote_equivocations_get_all_for_epoch(EPOCH))
        .unwrap();
    assert_eq!(stored.len(), 1);
    let EquivocatingVotes::Proposal {
        first: stored_first, ..
    } = &stored[0].votes
    else {
        panic!("expected a proposal vote pair");
    };
    assert_eq!(stored_first.signature, signature(1, 1));
}

/// A validator that equivocates on both vote streams at one view produces two proofs, and neither
/// may displace the other.
#[test]
fn proposal_and_timeout_evidence_coexist_at_one_view() {
    let (db, _tmp) = create_rocksdb();
    let proposal = proposal_evidence(1);
    let timeout = timeout_evidence(1);

    assert!(db.with_write_tx(|tx| tx.vote_equivocation_record(&proposal)).unwrap());
    assert!(db.with_write_tx(|tx| tx.vote_equivocation_record(&timeout)).unwrap());

    db.with_read_tx(|tx| {
        assert!(tx.vote_equivocation_exists(VoteEquivocationKind::Proposal, EPOCH, HEIGHT, &signer(1))?);
        assert!(tx.vote_equivocation_exists(VoteEquivocationKind::Timeout, EPOCH, HEIGHT, &signer(1))?);
        assert!(!tx.vote_equivocation_exists(VoteEquivocationKind::Proposal, EPOCH, HEIGHT, &signer(2))?);
        Ok::<_, tari_ootle_storage::StorageError>(())
    })
    .unwrap();

    let stored = db
        .with_read_tx(|tx| tx.vote_equivocations_get_all_for_epoch(EPOCH))
        .unwrap();
    assert_eq!(stored.len(), 2);
}

#[test]
fn evidence_is_scoped_to_its_epoch() {
    let (db, _tmp) = create_rocksdb();
    let mut other_epoch = proposal_evidence(1);
    other_epoch.epoch = EPOCH + Epoch(1);

    db.with_write_tx(|tx| tx.vote_equivocation_record(&proposal_evidence(1)))
        .unwrap();
    db.with_write_tx(|tx| tx.vote_equivocation_record(&other_epoch))
        .unwrap();

    assert_eq!(
        db.with_read_tx(|tx| tx.vote_equivocations_get_all_for_epoch(EPOCH))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        db.with_read_tx(|tx| tx.vote_equivocations_get_all_for_epoch(EPOCH + Epoch(1)))
            .unwrap()
            .len(),
        1
    );
}

/// Evidence ages out with the blocks of the view it indicts, so an equivocator cannot grow this
/// column family without bound across epochs.
#[test]
fn epoch_cleanup_prunes_evidence_past_the_retention_window() {
    let (db, _tmp) = create_rocksdb_with_opts(DatabaseOptions::default().with_epoch_history_length(2));
    let mut newer = timeout_evidence(1);
    newer.epoch = EPOCH + Epoch(2);

    db.with_write_tx(|tx| tx.vote_equivocation_record(&proposal_evidence(1)))
        .unwrap();
    db.with_write_tx(|tx| tx.vote_equivocation_record(&timeout_evidence(1)))
        .unwrap();
    db.with_write_tx(|tx| tx.vote_equivocation_record(&newer)).unwrap();

    // Retention is 2 epochs, so cleaning up at EPOCH + 3 prunes everything at or below EPOCH + 1.
    db.with_write_tx(|tx| tx.epoch_cleanup(EPOCH + Epoch(3))).unwrap();

    assert!(
        db.with_read_tx(|tx| tx.vote_equivocations_get_all_for_epoch(EPOCH))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.with_read_tx(|tx| tx.vote_equivocations_get_all_for_epoch(EPOCH + Epoch(2)))
            .unwrap()
            .len(),
        1
    );
}
