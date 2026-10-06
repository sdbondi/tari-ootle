//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_ootle_common_types::shard::Shard;
use tari_ootle_storage::consensus_models::StateVersionProof;
use tari_state_tree::Version;

use crate::{
    codecs::{KeyPrefix, NumberCodec, SerdeBridgeCodec, ShardCodec},
    column_families::cf_names,
    prefixed,
    traits::Cf,
};

prefixed!(StateVersionProofPrefix, KeyPrefix::StateVersionProofs);

/// Per shard and state version, what this node holds to prove that state to a peer syncing it.
pub struct StateVersionProofCf;

impl Cf for StateVersionProofCf {
    type Key = (Shard, Version);
    type KeyCodec = (ShardCodec, NumberCodec<Version>);
    type Prefix = StateVersionProofPrefix;
    type Value = StateVersionProof;
    type ValueCodec = SerdeBridgeCodec<Self::Value>;

    fn name() -> &'static str {
        cf_names::STATE_TREE
    }
}
