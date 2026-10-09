//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_consensus_types::BlockId;
use tari_ootle_common_types::shard::Shard;
use tari_ootle_storage::consensus_models::StateVersionProof;
use tari_state_tree::Version;

use crate::{
    codecs::{BlockIdCodec, BytesCodec, KeyPrefix, NumberCodec, SerdeBridgeCodec, ShardCodec},
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

prefixed!(BlockCommitProofPrefix, KeyPrefix::BlockCommitProofs);

/// The CBOR-encoded commit proof of each committed block that produced state versions, which the
/// [`StateVersionProofCf`] entries naming the block are served with once the block is pruned.
pub struct BlockCommitProofCf;

impl Cf for BlockCommitProofCf {
    type Key = BlockId;
    type KeyCodec = BlockIdCodec;
    type Prefix = BlockCommitProofPrefix;
    type Value = Vec<u8>;
    type ValueCodec = BytesCodec;

    fn name() -> &'static str {
        cf_names::STATE_TREE
    }
}
