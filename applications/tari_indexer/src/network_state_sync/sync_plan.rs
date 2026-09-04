//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::collections::HashMap;

use tari_epoch_manager::service::NetworkDescription;
use tari_ootle_common_types::ShardGroup;

use crate::network_state_sync::{committee_client::ValidatorCommitteeRpcPool, sync_progress::SharedSyncProgress};

/// What one epoch's worth of syncing is drawn against: the committees as they stand, a session pool
/// for each, and the progress every stream advances.
pub struct SyncPlan {
    network_description: NetworkDescription,
    sync_progress: SharedSyncProgress,
    committee_pools: HashMap<ShardGroup, ValidatorCommitteeRpcPool>,
}

impl SyncPlan {
    pub fn new(
        network_description: NetworkDescription,
        sync_progress: SharedSyncProgress,
        committee_pools: HashMap<ShardGroup, ValidatorCommitteeRpcPool>,
    ) -> Self {
        Self {
            network_description,
            sync_progress,
            committee_pools,
        }
    }

    pub fn sync_progress(&self) -> &SharedSyncProgress {
        &self.sync_progress
    }

    pub fn committee_pools(&self) -> &HashMap<ShardGroup, ValidatorCommitteeRpcPool> {
        &self.committee_pools
    }

    pub fn network_description(&self) -> &NetworkDescription {
        &self.network_description
    }
}
