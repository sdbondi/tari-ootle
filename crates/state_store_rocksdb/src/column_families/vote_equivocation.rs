//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_ootle_common_types::{Epoch, NodeHeight};
use tari_ootle_storage::consensus_models::VoteEquivocation;
use tari_template_lib_types::crypto::RistrettoPublicKeyBytes;

use crate::{
    codecs::{DefaultCodec, EpochCodec, KeyPrefix, NodeHeightCodec, PublicKeyCodec},
    column_families::cf_names,
    prefixed,
    traits::{Cf, QueryCf},
};

/// Key = `(epoch, height, signer)`, each component big-endian encoded, so that an epoch is a
/// prefix range and entries within it read in view order.
///
/// Proposal and timeout equivocations are separate column families because one validator can
/// equivocate on both at a single view; sharing a key would silently drop the second proof.
macro_rules! vote_equivocation_cf {
    ($module:ident, $cf:ident, $prefix_name:ident, $prefix:expr) => {
        pub mod $module {
            use super::*;

            prefixed!($prefix_name, $prefix);

            pub struct $cf;

            impl Cf for $cf {
                type Key = (Epoch, NodeHeight, RistrettoPublicKeyBytes);
                type KeyCodec = (EpochCodec, NodeHeightCodec, PublicKeyCodec);
                type Prefix = $prefix_name;
                type Value = VoteEquivocation;
                type ValueCodec = DefaultCodec<Self::Value>;

                fn name() -> &'static str {
                    cf_names::CHAIN_METADATA
                }
            }

            pub struct ByEpochQuery;

            impl QueryCf for ByEpochQuery {
                type Cf = $cf;
                type Key = Epoch;
                type KeyCodec = EpochCodec;
            }
        }
    };
}

vote_equivocation_cf!(
    proposal,
    ProposalVoteEquivocationCf,
    ProposalVoteEquivocationPrefix,
    KeyPrefix::ProposalVoteEquivocations
);
vote_equivocation_cf!(
    timeout,
    TimeoutVoteEquivocationCf,
    TimeoutVoteEquivocationPrefix,
    KeyPrefix::TimeoutVoteEquivocations
);
