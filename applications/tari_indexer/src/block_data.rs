//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_dan_storage::consensus_models::{BlockModel, SubstateUpdate};

#[derive(Clone, Debug)]
pub struct BlockData {
    pub block: BlockModel,
    pub diff: Vec<SubstateUpdate>,
}
