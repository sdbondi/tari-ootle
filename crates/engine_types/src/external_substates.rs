//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use crate::substate::SubstateId;

/// What the executor knows about substates outside a transaction's declared inputs. The engine holds only the
/// declared inputs, so on its own it cannot tell that a substate it creates is already taken.
pub trait ExternalSubstates {
    /// Whether `id` cannot be created by the transaction: some version of it exists in the state the transaction
    /// commits against, or a transaction that commits before it on the same chain creates it.
    fn is_taken(&self, id: &SubstateId) -> Result<bool, ExternalSubstatesError>;
}

/// Knows of no substate beyond the transaction's inputs, so nothing is taken.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoExternalSubstates;

impl ExternalSubstates for NoExternalSubstates {
    fn is_taken(&self, _id: &SubstateId) -> Result<bool, ExternalSubstatesError> {
        Ok(false)
    }
}

impl<F: Fn(&SubstateId) -> bool> ExternalSubstates for F {
    fn is_taken(&self, id: &SubstateId) -> Result<bool, ExternalSubstatesError> {
        Ok(self(id))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Failed to look up substate {id}: {details}")]
pub struct ExternalSubstatesError {
    pub id: SubstateId,
    pub details: String,
}
