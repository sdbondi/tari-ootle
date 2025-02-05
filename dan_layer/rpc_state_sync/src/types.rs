//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_dan_app_utilities::template_manager::interface::TemplateChange;
use tari_engine_types::published_template::PublishedTemplateAddress;
use tari_state_tree::Version;

struct SyncArtifacts {
    pub tree_version: Option<Version>,
    pub template_changes: Vec<TemplateChange>,
}

impl SyncArtifacts {
    pub fn new(tree_version: Option<Version>, template_changes: Vec<TemplateChange>) -> Self {
        Self {
            tree_version,
            template_changes,
        }
    }
}

#[derive(Debug)]
pub struct SyncResult<E> {
    failure: Option<E>,
    template_changes: Vec<TemplateChange>,
}

impl<E> SyncResult<E> {
    pub fn success(template_changes: Vec<TemplateChange>) -> Self {
        Self {
            failure: None,
            template_changes,
        }
    }

    pub fn fail(failure: E, template_changes: Vec<TemplateChange>) -> Self {
        Self {
            failure: Some(failure),
            template_changes,
        }
    }

    pub fn abort(failure: E) -> Self {
        Self {
            failure: Some(failure),
            template_changes: vec![],
        }
    }
}

/// Use this to construct an abort SyncResult from an error.
/// This macro is similar to the try! macro and the ? operator
/// and would not be needed once the std::ops::Try trait is stabilised.
///
/// ```ignore
/// async fn sync(mut self) -> SyncResult<MyError> {
///    try_sync!(do_something_fallible());
///    SyncResult:::success(..)
/// }
/// ```
#[macro_export]
macro_rules! try_sync {
    ($e:expr) => {
        match $e {
            Ok(val) => val,
            Err(err) => return SyncResult::abort(err.into()),
        }
    };
}
