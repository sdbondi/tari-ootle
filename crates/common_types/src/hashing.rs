//   Copyright 2022 The Tari Project
//   SPDX-License-Identifier: BSD-3-clause

use tari_hashing::layer2::TariConsensusHasher;
pub use tari_hashing::layer2::{
    block_hasher,
    block_metadata_hasher,
    command_hasher,
    proposal_vote_signature_hasher,
    tari_consensus_hasher,
};

pub fn quorum_certificate_id_hasher() -> TariConsensusHasher {
    tari_consensus_hasher("QuorumCertificateId")
}

pub fn timeout_certificate_id_hasher() -> TariConsensusHasher {
    tari_consensus_hasher("TimeoutCertificateId")
}

/// Hashes a transaction's outcome into a leaf of a block's transaction merkle root.
pub fn finalized_transaction_hasher() -> TariConsensusHasher {
    tari_consensus_hasher("FinalizedTransaction")
}
