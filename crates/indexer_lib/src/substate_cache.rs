// Copyright 2023. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use std::future::Future;

use tari_engine_types::substate::SubstateId;
use tari_validator_node_rpc::client::SubstateResult;

#[derive(thiserror::Error, Debug)]
#[error("Failed substate cache operation {0}")]
pub struct SubstateCacheError(pub String);

/// A point in a shard's state transition stream up to which the cache holds every transition.
///
/// The cache is served on a completeness argument rather than a timer: an entry answers for the
/// substate's latest version because every commit that would supersede or destroy it reaches the
/// cache through that shard's stream. A watermark is what makes the argument checkable — it is
/// captured before a committee fetch and handed back to [`SubstateCache::write`], so a transition
/// that arrived while the fetch was in flight can veto the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FetchWatermark(u64);

impl FetchWatermark {
    pub const fn new(state_version: u64) -> Self {
        Self(state_version)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct SubstateCacheEntry {
    pub version: u32,
    pub substate_result: SubstateResult,
    pub cached_at: u64,
    /// True if the value was committee-verified when it was fetched.
    pub verified: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SubstateCacheEntryRef<'a> {
    pub version: u32,
    pub substate_result: &'a SubstateResult,
    pub cached_at: u64,
    pub verified: bool,
}

pub trait SubstateCache: Send + Sync {
    /// The watermark for the shard owning `id`, or `None` when that shard's completeness cannot be
    /// established - it has never been synced, or the last sync of it is too far behind to serve
    /// from. Nothing may be cached for a substate whose shard has no watermark.
    fn watermark(
        &self,
        id: &SubstateId,
    ) -> impl Future<Output = Result<Option<FetchWatermark>, SubstateCacheError>> + Send;

    /// The cached answer for `id` at `version`, or for its latest version when `version` is `None`.
    /// Returns `None` when nothing is cached or the shard has no watermark.
    ///
    /// A version below the cached one is answered without any watermark: see
    /// [`SubstateCache::write`] for what the cache holds and why that conclusion cannot go stale.
    fn read(
        &self,
        id: &SubstateId,
        version: Option<u32>,
    ) -> impl Future<Output = Result<Option<SubstateCacheEntry>, SubstateCacheError>> + Send;

    /// Records `entry` as the substate's head version, provided no transition for `id` has arrived
    /// since `watermark`. A write vetoed that way is not an error: the caller still has its freshly
    /// fetched value, and the next read fetches again.
    ///
    /// The cache holds one entry per substate, its head. Only a result that establishes the head may
    /// be written: an `Up` version is always the head, since upping a substate downs its predecessor,
    /// and a lookup that named no version answers with the head by definition. A named version that
    /// came back `Down` establishes only that the head is higher, not what it is.
    fn write(
        &self,
        id: &SubstateId,
        entry: SubstateCacheEntryRef<'_>,
        watermark: FetchWatermark,
    ) -> impl Future<Output = Result<(), SubstateCacheError>> + Send;
}
