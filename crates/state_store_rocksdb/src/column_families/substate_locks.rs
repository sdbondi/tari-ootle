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
use tari_engine_types::substate::SubstateId;
use tari_ootle_common_types::{Epoch, NodeHeight};
use tari_ootle_storage::consensus_models::SubstateLock;

use crate::{
    codecs::{BlockIdCodec, DefaultCodec, KeyPrefix},
    column_families::cf_names,
    prefixed,
    traits::Cf,
};

/// The locks a block granted that are still held.
#[derive(Debug, Clone, Serialize, Encode, Decode, CborLen)]
pub struct BlockLockSet {
    #[n(0)]
    pub block_epoch: Epoch,
    #[n(1)]
    pub block_height: NodeHeight,
    #[n(2)]
    pub substates: Vec<SubstateLockGrants>,
}

/// A substate's locks in the order the block granted them.
#[derive(Debug, Clone, Serialize, Encode, Decode, CborLen)]
pub struct SubstateLockGrants {
    #[n(0)]
    pub substate_id: SubstateId,
    #[n(1)]
    pub locks: Vec<SubstateLock>,
}

prefixed!(BlockLockSetPrefix, KeyPrefix::SubstateLockSets);

pub struct BlockLockSetCf;

impl Cf for BlockLockSetCf {
    type Key = BlockId;
    type KeyCodec = BlockIdCodec;
    type Prefix = BlockLockSetPrefix;
    type Value = BlockLockSet;
    type ValueCodec = DefaultCodec<Self::Value>;

    fn name() -> &'static str {
        cf_names::SUBSTATES
    }
}

/// The per-lock tables substate locks were stored in up to schema version 0. Only the migration to version 1 reads
/// them, moving their locks into [`BlockLockSetCf`].
pub mod legacy {
    use serde::Serialize;
    use tari_consensus_types::BlockId;
    use tari_engine_types::substate::SubstateId;
    use tari_ootle_common_types::{Epoch, NodeHeight, SubstateLockType};
    use tari_ootle_storage::consensus_models::SubstateLock;
    use tari_ootle_transaction::TransactionId;

    use crate::{
        codecs::{
            BlockIdCodec,
            DefaultCodec,
            EpochCodec,
            KeyPrefix,
            NodeHeightCodec,
            NumberCodec,
            SubstateIdCodec,
            SubstateLockKeyCodec,
            TransactionIdCodec,
            UnitCodec,
        },
        column_families::cf_names,
        prefixed,
        traits::Cf,
    };

    /// Identifies one lock a block granted. `grant_seq` is the lock's position in the sequence its block granted for
    /// the substate.
    #[derive(Debug, PartialEq, Eq, Serialize)]
    pub struct SubstateLockKey {
        pub block_id: BlockId,
        pub block_epoch: Epoch,
        pub block_height: NodeHeight,
        pub substate_id: SubstateId,
        pub transaction_id: TransactionId,
        pub grant_seq: u32,
    }

    prefixed!(SubstateLockPrefix, KeyPrefix::SubstateLocks);

    /// One record per lock.
    pub struct SubstateLockModel;

    impl Cf for SubstateLockModel {
        type Key = SubstateLockKey;
        type KeyCodec = SubstateLockKeyCodec<(TransactionId, SubstateId, BlockId, Epoch, NodeHeight, u32)>;
        type Prefix = SubstateLockPrefix;
        type Value = SubstateLock;
        type ValueCodec = DefaultCodec<Self::Value>;

        fn name() -> &'static str {
            cf_names::SUBSTATES
        }
    }

    prefixed!(SubstateLockChainOrderPrefix, KeyPrefix::SubstateLockChainOrderIndex);

    pub struct ChainOrderIndex;

    impl Cf for ChainOrderIndex {
        type Key = (SubstateId, Epoch, NodeHeight, BlockId, u32);
        type KeyCodec = (
            SubstateIdCodec,
            EpochCodec,
            NodeHeightCodec,
            BlockIdCodec,
            NumberCodec<u32>,
        );
        type Prefix = SubstateLockChainOrderPrefix;
        type Value = TransactionId;
        type ValueCodec = TransactionIdCodec;

        fn name() -> &'static str {
            cf_names::SUBSTATES
        }
    }

    prefixed!(SubstatesBlockIdIndexPrefix, KeyPrefix::SubstateLocksBlockIdIndex);

    pub struct BlockIdIndex;

    impl Cf for BlockIdIndex {
        type Key = SubstateLockKey;
        type KeyCodec = SubstateLockKeyCodec<(BlockId, SubstateId, TransactionId, Epoch, NodeHeight, u32)>;
        type Prefix = SubstatesBlockIdIndexPrefix;
        type Value = ();
        type ValueCodec = UnitCodec;

        fn name() -> &'static str {
            cf_names::SUBSTATES
        }
    }

    prefixed!(SubstateIdIndexPrefix, KeyPrefix::SubstateLockSubstateIdIndex);

    pub struct SubstateIdIndex;

    impl Cf for SubstateIdIndex {
        type Key = SubstateLockKey;
        type KeyCodec = SubstateLockKeyCodec<(SubstateId, TransactionId, BlockId, Epoch, NodeHeight, u32)>;
        type Prefix = SubstateIdIndexPrefix;
        type Value = SubstateLockType;
        type ValueCodec = DefaultCodec<Self::Value>;

        fn name() -> &'static str {
            cf_names::SUBSTATES
        }
    }
}
