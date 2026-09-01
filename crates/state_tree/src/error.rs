//   Copyright 2024 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_jellyfish::{JmtStorageError, Version};
use tari_ootle_common_types::optional::IsNotFoundError;

#[derive(Debug, thiserror::Error)]
pub enum StateTreeError {
    #[error("JMT Storage error: {0}")]
    JmtStorageError(#[from] JmtStorageError),
    #[error(
        "Refusing to write state tree version {next_version} on top of current version {current_version}: the next \
         version must be greater than the current version"
    )]
    NonMonotonicVersion {
        current_version: Version,
        next_version: Version,
    },
}

impl IsNotFoundError for StateTreeError {
    fn is_not_found_error(&self) -> bool {
        matches!(self, StateTreeError::JmtStorageError(JmtStorageError::NotFound(_)))
    }
}
