//    Copyright 2023 The Tari Project
//    SPDX-License-Identifier: BSD-3-Clause

use std::{marker::PhantomData, sync::Arc};

use tari_engine::{
    executables::Executable,
    fees::{FeeModule, FeeTable, WasmMeteringRate},
    runtime::{AuthParams, RuntimeModule},
    state_store::{StateReader, StateStoreError},
    template::LoadedTemplate,
    traits::ClaimProofVerifier,
    transaction::{TransactionError, TransactionProcessor},
};
use tari_engine_types::{commit_result::ExecuteResult, substate::Substate, virtual_substate::VirtualSubstates};
use tari_ootle_common_types::{
    SubstateLockType,
    SubstateRequirement,
    VersionedSubstateId,
    services::template_provider::TemplateProvider,
};
use tari_ootle_storage::consensus_models::VersionedSubstateIdLockIntent;
use tari_ootle_transaction::Transaction;
use tari_template_lib::types::NonFungibleAddress;

const _LOG_TARGET: &str = "tari::ootle::transaction_executor";

pub trait TransactionExecutor<TStore> {
    type Error: std::error::Error + Send + Sync + 'static;

    fn execute(
        &self,
        transaction: &Transaction,
        state_store: TStore,
        virtual_substates: VirtualSubstates,
        burn_rate_bps: u16,
    ) -> Result<ExecutionOutput, Self::Error>;
}

#[derive(Debug, Clone)]
pub struct ExecutionOutput {
    pub result: ExecuteResult,
}

impl ExecutionOutput {
    pub fn resolve_input_locks<'a, I: IntoIterator<Item = (&'a SubstateRequirement, &'a Substate)>>(
        &self,
        inputs: I,
    ) -> Vec<VersionedSubstateIdLockIntent> {
        if let Some(diff) = self.result.finalize.any_accept() {
            inputs
                .into_iter()
                .map(|(substate_req, substate)| {
                    let requested_specific_version = substate_req.version().is_some();
                    let lock_flag = if diff.down_iter().any(|(id, _)| id == substate_req.substate_id()) {
                        // Update all inputs that were DOWNed to be write locked
                        SubstateLockType::Write
                    } else {
                        // Any input not downed, gets a read lock
                        SubstateLockType::Read
                    };
                    VersionedSubstateIdLockIntent::new(
                        VersionedSubstateId::new(substate_req.substate_id().clone(), substate.version()),
                        lock_flag,
                        requested_specific_version,
                    )
                })
                .collect()
        } else {
            // TODO: we might want to have a SubstateLockFlag::None for rejected transactions so that we still know the
            // shards involved but do not lock them. We dont actually lock anything for rejected transactions anyway.
            inputs
                .into_iter()
                .map(|(substate_req, substate)| {
                    VersionedSubstateIdLockIntent::new(
                        VersionedSubstateId::new(substate_req.substate_id().clone(), substate.version()),
                        SubstateLockType::Read,
                        true,
                    )
                })
                .collect()
        }
    }
}

#[derive(Clone)]
pub struct TariTransactionProcessor<TStore, TTemplateProvider> {
    template_provider: Arc<TTemplateProvider>,
    fee_table: FeeTable,
    dry_run: bool,
    claim_burn_proof_verifier: Arc<dyn ClaimProofVerifier + Send + Sync + 'static>,
    wasm_metering_rate: WasmMeteringRate,
    _store: PhantomData<TStore>,
}

impl<TStore: StateReader + 'static, TTemplateProvider> TariTransactionProcessor<TStore, TTemplateProvider> {
    pub fn new(
        template_provider: TTemplateProvider,
        fee_table: FeeTable,
        dry_run: bool,
        claim_burn_proof_verifier: Arc<dyn ClaimProofVerifier + Send + Sync + 'static>,
    ) -> Self {
        let wasm_metering_rate = WasmMeteringRate::from_fee_table(&fee_table);
        Self {
            template_provider: Arc::new(template_provider),
            fee_table,
            dry_run,
            claim_burn_proof_verifier,
            wasm_metering_rate,
            _store: PhantomData,
        }
    }
}

impl<TStore: StateReader + Clone + 'static, TTemplateProvider> TransactionExecutor<TStore>
    for TariTransactionProcessor<TStore, TTemplateProvider>
where TTemplateProvider: TemplateProvider<Template = LoadedTemplate>
{
    type Error = TransactionProcessorError;

    fn execute(
        &self,
        transaction: &Transaction,
        state_store: TStore,
        virtual_substates: VirtualSubstates,
        burn_rate_bps: u16,
    ) -> Result<ExecutionOutput, Self::Error> {
        // Include signature public key badges for all transaction signers in the initial auth scope
        // NOTE: we assume all signatures have already been validated.
        let initial_ownership_proofs = transaction
            .signers_iter()
            .map(|pk| NonFungibleAddress::from_public_key(*pk))
            .collect();
        let auth_params = AuthParams {
            initial_ownership_proofs: Arc::new(initial_ownership_proofs),
        };

        // The burn rate is resolved per-execution for the current epoch, so the fee module is built here rather than
        // shared across executions.
        let modules = vec![
            Box::new(FeeModule::new(0, self.fee_table.clone(), self.dry_run, burn_rate_bps))
                as Box<dyn RuntimeModule<TStore>>,
        ];

        let processor = TransactionProcessor::new(
            self.template_provider.clone(),
            state_store,
            auth_params,
            virtual_substates,
            Arc::from(modules),
            self.claim_burn_proof_verifier.clone(),
            self.wasm_metering_rate,
        );
        let result = processor.execute(transaction.clone())?;

        Ok(ExecutionOutput { result })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TransactionProcessorError {
    #[error(transparent)]
    TransactionError(#[from] TransactionError),
    #[error(transparent)]
    StateStoreError(#[from] StateStoreError),
}
