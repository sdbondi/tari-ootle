//  Copyright 2025. The Tari Project
//
//  Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
//  following conditions are met:
//
//  1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
//  disclaimer.
//
//  2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
//  following disclaimer in the documentation and/or other materials provided with the distribution.
//
//  3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
//  products derived from this software without specific prior written permission.
//
//  THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
//  INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//  DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//  SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//  SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
//  WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
//  USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use minicbor::{CborLen, Decode, Encode};
use serde::Serialize;
use tari_consensus_types::BlockId;
use tari_ootle_common_types::shard::Shard;
use tari_ootle_storage::consensus_models::PendingShardStateTreeDiff;

use crate::{
    codecs::{BlockIdCodec, DefaultCodec, KeyPrefix},
    column_families::cf_names,
    prefixed,
    traits::Cf,
};

/// One shard's pending state tree diff.
#[derive(Debug, Clone, Serialize, Encode, Decode, CborLen)]
pub struct ShardStateTreeDiff {
    #[n(0)]
    pub shard: Shard,
    #[n(1)]
    pub diff: PendingShardStateTreeDiff,
}

/// [`ShardStateTreeDiff`] by reference, for writing a record without copying its diffs.
#[derive(Debug, Clone, Copy, Encode, CborLen)]
pub struct ShardStateTreeDiffRef<'a> {
    #[n(0)]
    pub shard: Shard,
    #[n(1)]
    pub diff: &'a PendingShardStateTreeDiff,
}

prefixed!(PendingStateTreeDiffRecordPrefix, KeyPrefix::PendingStateTreeDiffRecords);

/// A block's pending state tree diffs, one per shard it changes.
pub struct PendingStateTreeDiffRecordCf;

impl Cf for PendingStateTreeDiffRecordCf {
    type Key = BlockId;
    type KeyCodec = BlockIdCodec;
    type Prefix = PendingStateTreeDiffRecordPrefix;
    type Value = Vec<ShardStateTreeDiff>;
    type ValueCodec = DefaultCodec<Self::Value>;

    fn name() -> &'static str {
        cf_names::STATE_TREE
    }
}

/// Pending state tree diffs as stored up to schema version 2, with each diff in the serde-bridged encoding: version 1
/// kept one record per block and shard, version 2 one record per block. Only migrations read them.
pub mod legacy {
    use minicbor::{CborLen, Decode, Encode};
    use tari_consensus_types::BlockId;
    use tari_ootle_common_types::shard::Shard;
    use tari_ootle_storage::consensus_models::PendingShardStateTreeDiff;
    use tari_state_tree::{StateHashTreeDiff, StateTreePayload, Version};

    use super::PendingStateTreeDiffRecordPrefix;
    use crate::{
        codecs::{BlockIdCodec, DefaultCodec, KeyPrefix, ShardCodec},
        column_families::cf_names,
        prefixed,
        traits::Cf,
    };

    #[derive(Debug, Clone, Encode, Decode, CborLen)]
    pub struct LegacyPendingShardStateTreeDiff {
        #[n(0)]
        pub version: Version,
        #[cbor(n(1), with = "tari_bor::adapters::serde_bridge")]
        pub diff: StateHashTreeDiff<StateTreePayload>,
    }

    impl From<LegacyPendingShardStateTreeDiff> for PendingShardStateTreeDiff {
        fn from(legacy: LegacyPendingShardStateTreeDiff) -> Self {
            PendingShardStateTreeDiff::new(legacy.version, legacy.diff)
        }
    }

    impl From<&PendingShardStateTreeDiff> for LegacyPendingShardStateTreeDiff {
        fn from(diff: &PendingShardStateTreeDiff) -> Self {
            Self {
                version: diff.version,
                diff: (*diff.diff).clone(),
            }
        }
    }

    #[derive(Debug, Clone, Encode, Decode, CborLen)]
    pub struct LegacyShardStateTreeDiff {
        #[n(0)]
        pub shard: Shard,
        #[n(1)]
        pub diff: LegacyPendingShardStateTreeDiff,
    }

    /// Version 2's record of a block's pending state tree diffs.
    pub struct PendingStateTreeDiffRecordV2Cf;

    impl Cf for PendingStateTreeDiffRecordV2Cf {
        type Key = BlockId;
        type KeyCodec = BlockIdCodec;
        type Prefix = PendingStateTreeDiffRecordPrefix;
        type Value = Vec<LegacyShardStateTreeDiff>;
        type ValueCodec = DefaultCodec<Self::Value>;

        fn name() -> &'static str {
            cf_names::STATE_TREE
        }
    }

    prefixed!(PendingStateTreeDiffPrefix, KeyPrefix::PendingStateTreeDiff);

    /// Version 1's per-shard table.
    pub struct PendingStateTreeDiffCf;

    impl Cf for PendingStateTreeDiffCf {
        type Key = (BlockId, Shard);
        type KeyCodec = (BlockIdCodec, ShardCodec);
        type Prefix = PendingStateTreeDiffPrefix;
        type Value = LegacyPendingShardStateTreeDiff;
        type ValueCodec = DefaultCodec<Self::Value>;

        fn name() -> &'static str {
            cf_names::STATE_TREE
        }
    }
}
