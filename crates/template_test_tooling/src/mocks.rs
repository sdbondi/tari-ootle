//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_engine::traits::{ClaimProofError, ClaimProofVerifier, VerifiedClaim};
use tari_ootle_common_types::Epoch;

pub struct AlwaysPassesProofVerifier;

impl ClaimProofVerifier for AlwaysPassesProofVerifier {
    fn verify_claim_proof(
        &self,
        _epoch: Epoch,
        claim_proof: &tari_engine_types::confidential::MinotariBurnClaimProof,
    ) -> Result<VerifiedClaim, ClaimProofError> {
        Ok(VerifiedClaim {
            claim_public_key: claim_proof.output.features.claim_public_key,
        })
    }
}
