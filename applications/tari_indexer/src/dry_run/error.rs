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

use tari_engine::state_store::StateStoreError;
use tari_epoch_manager::EpochManagerError;
use tari_indexer_lib::error::IndexerError;
use tari_ootle_app_utilities::transaction_executor::TransactionProcessorError;
use tari_ootle_transaction_validation::TransactionValidationError;
use tari_rpc_framework::RpcStatus;
use thiserror::Error;

use crate::substate_manager::SubstateManagerError;

#[derive(Error, Debug)]
pub enum DryRunTransactionProcessorError {
    #[error("EpochManager error: {0}")]
    EpochManager(#[from] EpochManagerError),
    #[error("Rpc error: {0}")]
    RpcRequestFailed(#[from] RpcStatus),
    #[error("TransactionProcessor error: {0}")]
    PayloadProcessor(#[from] TransactionProcessorError),
    #[error("Indexer error : {0}")]
    IndexerError(#[from] IndexerError),
    #[error("StateStore error: {0}")]
    StateStoreError(#[from] StateStoreError),
    #[error("Not a dry run transaction")]
    NonDryRunTransaction,
    #[error("Failed to spawn blocking task: {0}")]
    SpawnBlockingTaskError(#[from] tokio::task::JoinError),
    #[error("SubstateManager error: {0}")]
    SubstateManagerError(#[from] SubstateManagerError),
    #[error("Invalid transaction: {0}")]
    InvalidTransaction(#[from] TransactionValidationError),
    #[error("Transaction declares {num_inputs} inputs, more than the {max_inputs} a dry-run accepts")]
    TooManyInputs { num_inputs: usize, max_inputs: usize },
}

impl DryRunTransactionProcessorError {
    /// True when the fault lies with the submitted transaction.
    pub fn is_invalid_transaction(&self) -> bool {
        matches!(
            self,
            Self::NonDryRunTransaction |
                Self::InvalidTransaction(_) |
                Self::TooManyInputs { .. } |
                Self::SubstateManagerError(
                    SubstateManagerError::InputSubstateIsDown { .. } |
                        SubstateManagerError::InputSubstateDoesNotExist { .. }
                )
        )
    }
}
