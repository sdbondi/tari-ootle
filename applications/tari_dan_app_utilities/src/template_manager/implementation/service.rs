//  Copyright 2023. The Tari Project
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

use std::{collections::HashMap, sync::Arc, time::Duration};

use futures::StreamExt;
use log::*;
use tari_common_types::types::{FixedHash, PublicKey};
use tari_dan_common_types::{
    committee::Committee,
    services::template_provider::TemplateProvider,
    Epoch,
    NodeAddressable,
    PeerAddress,
    SubstateAddress,
};
use tari_dan_engine::function_definitions::FlowFunctionDefinition;
use tari_dan_p2p::proto::rpc::{SyncTemplatesRequest, TemplateType};
use tari_dan_storage::global::{DbTemplateType, DbTemplateUpdate, TemplateStatus};
use tari_engine_types::{
    calculate_template_binary_hash,
    hashing::template_hasher32,
    published_template::PublishedTemplateAddress,
    substate::SubstateId,
};
use tari_epoch_manager::{base_layer::EpochManagerHandle, EpochManagerReader};
use tari_shutdown::ShutdownSignal;
use tari_template_lib::{models::TemplateAddress, Hash};
use tari_validator_node_client::types::{ArgDef, FunctionDef, TemplateAbi};
use tari_validator_node_rpc::{
    client::{TariValidatorNodeRpcClientFactory, ValidatorNodeClientFactory},
    rpc_service::ValidatorNodeRpcClient,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use super::{
    downloader::{DownloadRequest, DownloadResult},
    TemplateManager,
};
use crate::template_manager::{
    implementation::{
        sync_worker::TemplateSyncRequest,
        template_sync_task::{template_sync_task, TemplateSyncClientTask},
    },
    interface::{
        SyncTemplatesResult,
        Template,
        TemplateChange,
        TemplateExecutable,
        TemplateManagerError,
        TemplateManagerRequest,
    },
};

const LOG_TARGET: &str = "tari::dan::template_manager";

pub struct TemplateManagerService<TAddr> {
    rx_request: mpsc::Receiver<TemplateManagerRequest>,
    manager: TemplateManager<TAddr>,
    epoch_manager: EpochManagerHandle<PeerAddress>,
    completed_downloads: mpsc::Receiver<DownloadResult>,
    download_queue: mpsc::Sender<DownloadRequest>,
    client_factory: Arc<TariValidatorNodeRpcClientFactory>,
    periodic_template_sync_interval: Duration,
}

impl<TAddr: NodeAddressable + 'static> TemplateManagerService<TAddr> {
    pub fn spawn(
        rx_request: mpsc::Receiver<TemplateManagerRequest>,
        manager: TemplateManager<TAddr>,
        epoch_manager: EpochManagerHandle<PeerAddress>,
        download_queue: mpsc::Sender<DownloadRequest>,
        completed_downloads: mpsc::Receiver<DownloadResult>,
        client_factory: TariValidatorNodeRpcClientFactory,
        periodic_template_sync_interval: Duration,
        shutdown: ShutdownSignal,
    ) -> JoinHandle<anyhow::Result<()>> {
        tokio::spawn(async move {
            Self {
                rx_request,
                manager,
                epoch_manager,
                download_queue,
                completed_downloads,
                client_factory: Arc::new(client_factory),
                periodic_template_sync_interval,
            }
            .run(shutdown)
            .await?;
            Ok(())
        })
    }

    pub async fn run(mut self, mut shutdown: ShutdownSignal) -> Result<(), TemplateManagerError> {
        self.on_startup().await?;
        let mut auto_template_sync_interval = tokio::time::interval(self.periodic_template_sync_interval);
        loop {
            tokio::select! {
                Some(req) = self.rx_request.recv() => self.handle_request(req).await,
                Some(download) = self.completed_downloads.recv() => {
                    if let Err(err) = self.handle_completed_download(download) {
                        error!(target: LOG_TARGET, "Error handling completed download: {}", err);
                    }
                },
                _ = auto_template_sync_interval.tick() => {
                    if let Err(error) = self.sync_pending_templates().await {
                        error!(target: LOG_TARGET, "Error syncing pending templates: {}", error);
                    }
                }
                _ = shutdown.wait() => {
                    dbg!("Shutting down epoch manager");
                    break;
                }
            }
        }
        Ok(())
    }

    /// Triggers syncing of pending templates.
    /// Please note that this is a non-blocking call, after trigger it returns immediately.
    async fn sync_pending_templates(&mut self) -> Result<(), TemplateManagerError> {
        let templates = self.manager.fetch_pending_templates()?;
        let template_addresses: Vec<TemplateAddress> =
            templates.iter().map(|template| template.template_address).collect();
        if !template_addresses.is_empty() {
            self.handle_templates_sync_request(template_addresses).await?;
            info!(target: LOG_TARGET, "⏳️️ {} templates are triggered to sync from network", templates.len());
        }

        Ok(())
    }

    async fn on_startup(&mut self) -> Result<(), TemplateManagerError> {
        let templates = self.manager.fetch_pending_templates()?;

        // trigger syncing templates the old way too
        for template in templates {
            if template.status == TemplateStatus::Pending {
                let _ignore = self
                    .download_queue
                    .send(DownloadRequest {
                        template_type: template.template_type,
                        address: Hash::from(template.template_address.into_array()),
                        expected_binary_hash: template.expected_hash,
                        url: template.url.unwrap(),
                    })
                    .await;
                info!(
                    target: LOG_TARGET,
                    "⏳️️ Template {} queued for download", template.template_address
                );
            }
        }
        Ok(())
    }

    async fn handle_request(&mut self, req: TemplateManagerRequest) {
        #[allow(clippy::enum_glob_use)]
        use TemplateManagerRequest::*;
        match req {
            AddTemplate {
                author_public_key,
                template_address,
                template,
                template_name,
                epoch,
                reply,
            } => {
                handle(
                    reply,
                    self.handle_add_template(author_public_key, template_address, template, template_name, epoch)
                        .await,
                );
            },
            GetTemplate { address, reply } => {
                handle(reply, self.manager.fetch_template(&address));
            },
            GetTemplates { limit, reply } => handle(reply, self.manager.fetch_template_metadata(limit)),
            LoadTemplateAbi { address, reply } => handle(reply, self.handle_load_template_abi(address)),
            TemplateExists { address, status, reply } => handle(reply, self.handle_template_exists(&address, status)),
            GetTemplatesByAddresses { addresses, reply } => {
                handle(reply, self.handle_get_templates_by_addresses(addresses))
            },
            SyncTemplates { addresses, reply } => handle(reply, self.handle_templates_sync_request(addresses).await),
            EnqueueTemplateChanges {
                template_changes,
                reply,
            } => handle(
                reply,
                self.handle_enqueue_template_changes_request(template_changes).await,
            ),
        }
    }

    fn handle_load_template_abi(&mut self, address: TemplateAddress) -> Result<TemplateAbi, TemplateManagerError> {
        let loaded = self
            .manager
            .get_template_module(&address)?
            .ok_or(TemplateManagerError::TemplateNotFound { address })?;
        Ok(TemplateAbi {
            template_name: loaded.template_def().template_name().to_string(),
            functions: loaded
                .template_def()
                .functions()
                .iter()
                .map(|f| FunctionDef {
                    name: f.name.clone(),
                    arguments: f
                        .arguments
                        .iter()
                        .map(|a| ArgDef {
                            name: a.name.to_string(),
                            arg_type: a.arg_type.to_string(),
                        })
                        .collect(),
                    output: f.output.to_string(),
                    is_mut: f.is_mut,
                })
                .collect(),
            version: loaded.template_def().tari_version().to_string(),
        })
    }

    fn handle_completed_download(&mut self, download: DownloadResult) -> Result<(), TemplateManagerError> {
        match download.result {
            Ok(bytes) => {
                info!(
                    target: LOG_TARGET,
                    "✅ Template {} downloaded successfully", download.template_address
                );

                // validation of the downloaded template binary hash
                let actual_binary_hash = calculate_template_binary_hash(&bytes);
                let template_status = if actual_binary_hash.as_slice() == download.expected_binary_hash.as_slice() {
                    info!(
                        target: LOG_TARGET,
                        "✅ Template {} is active", download.template_address,
                    );
                    TemplateStatus::Active
                } else {
                    warn!(
                        target: LOG_TARGET,
                        "⚠️ Template {} hash mismatch", download.template_address
                    );
                    // TODO: For now, let's just accept this so that we can update the binary at the URL without
                    // re-registering
                    TemplateStatus::Active
                    // TemplateStatus::Invalid
                };

                let update = match download.template_type {
                    DbTemplateType::Wasm => DbTemplateUpdate {
                        compiled_code: Some(bytes.to_vec()),
                        status: Some(template_status),
                        ..Default::default()
                    },
                    DbTemplateType::Flow => {
                        // make sure it deserializes correctly
                        let mut status = TemplateStatus::Invalid;
                        match serde_json::from_slice::<FlowFunctionDefinition>(&bytes) {
                            Ok(_) => status = template_status,
                            Err(e) => {
                                warn!(
                                    target: LOG_TARGET,
                                    "⚠️ Template {} is not valid json: {}", download.template_address, e
                                );
                            },
                        };

                        DbTemplateUpdate {
                            flow_json: Some(String::from_utf8(bytes.to_vec())?),
                            status: Some(status),
                            ..Default::default()
                        }
                    },
                    DbTemplateType::Manifest => todo!(),
                };
                self.manager.update_template(download.template_address, update)?;
            },
            Err(err) => {
                warn!(target: LOG_TARGET, "🚨 Failed to download template: {}", err);
                self.manager
                    .update_template(download.template_address, DbTemplateUpdate {
                        status: Some(TemplateStatus::DownloadFailed),
                        ..Default::default()
                    })?;
            },
        }
        Ok(())
    }

    /// Handling template exists request.
    fn handle_template_exists(
        &mut self,
        template_address: &TemplateAddress,
        status: Option<TemplateStatus>,
    ) -> Result<bool, TemplateManagerError> {
        self.manager.template_exists(template_address, status)
    }

    /// Handling fetching templates by addresses.
    fn handle_get_templates_by_addresses(
        &mut self,
        addresses: Vec<TemplateAddress>,
    ) -> Result<Vec<Template>, TemplateManagerError> {
        self.manager.fetch_templates_by_addresses(addresses)
    }

    async fn handle_add_template(
        &mut self,
        author_public_key: PublicKey,
        template_address: tari_engine_types::TemplateAddress,
        template: TemplateExecutable,
        template_name: Option<String>,
        epoch: Epoch,
    ) -> Result<(), TemplateManagerError> {
        let template_status = if matches!(template, TemplateExecutable::DownloadableWasm(_, _)) {
            TemplateStatus::New
        } else {
            TemplateStatus::Active
        };
        self.manager.add_template(
            author_public_key,
            template_address,
            template.clone(),
            template_name,
            Some(template_status),
            epoch,
        )?;

        // TODO: remove when we remove support for base layer template registration
        // update template status and add to download queue if it's a downloadable template
        if let TemplateExecutable::DownloadableWasm(url, expected_binary_hash) = template {
            // We could queue this up much later, at which point we'd update to pending
            self.manager.update_template(template_address, DbTemplateUpdate {
                status: Some(TemplateStatus::Pending),
                ..Default::default()
            })?;

            let _ignore = self
                .download_queue
                .send(DownloadRequest {
                    address: template_address,
                    template_type: DbTemplateType::Wasm,
                    url: url.to_string(),
                    expected_binary_hash,
                })
                .await;
        }

        Ok(())
    }

    async fn handle_enqueue_template_changes_request(
        &mut self,
        changes: Vec<TemplateChange>,
    ) -> Result<(), TemplateManagerError> {
        let mut sync_requests = vec![];
        for change in changes {
            match change {
                TemplateChange::Add {
                    template_address,
                    binary_hash,
                } => {
                    sync_requests.push(TemplateSyncRequest {
                        address: template_address.as_hash(),
                        expected_binary_hash: binary_hash,
                    });
                },
                TemplateChange::Deprecate { template_address } => {
                    self.manager.deprecate_template(&template_address.as_hash())?;
                },
            }
        }
        self.sync_worker.enqueue_all(sync_requests).await?;
        Ok(())
    }

    /// Starts an async task to synchronize templates from the right committees.
    /// This method returns a [`JoinHandle`] which can be .await-ed to get the results, or it can be ignored,
    /// the process will be running anyway async.
    #[allow(clippy::mutable_key_type)]
    #[allow(clippy::too_many_lines)]
    async fn handle_templates_sync_request(
        &self,
        mut addresses: Vec<TemplateAddress>,
    ) -> Result<SyncTemplatesResult, TemplateManagerError> {
        info!(target: LOG_TARGET, "New templates sync request for {} templates.", addresses.len());

        // check for existing templates
        let mut templates_to_sync = vec![];
        for address in addresses {
            // TODO: there are other status which may warrant not syncing
            if !self.manager.template_exists(&address, Some(TemplateStatus::Active))? {
                templates_to_sync.push(address);
            }
        }

        // sync
        let client_factory = self.client_factory.clone();
        let template_manager = self.manager.clone();
        let epoch_manager = self.epoch_manager.clone();

        // start a task to not block other calls in service
        Ok(tokio::spawn(
            TemplateSyncClientTask::new(client_factory, template_manager, epoch_manager, templates_to_sync).run(),
        ))
    }

    /// Creates a new validator node client.
    async fn vn_client(
        client_factory: Arc<TariValidatorNodeRpcClientFactory>,
        addr: &PeerAddress,
    ) -> Result<ValidatorNodeRpcClient, TemplateManagerError> {
        let mut rpc_client = client_factory.create_client(addr);
        let client = rpc_client.client_connection().await?;
        Ok(client)
    }
}

fn handle<T>(reply: oneshot::Sender<Result<T, TemplateManagerError>>, result: Result<T, TemplateManagerError>) {
    if let Err(ref e) = result {
        error!(target: LOG_TARGET, "Request failed with error: {}", e);
    }
    if reply.send(result).is_err() {
        error!(target: LOG_TARGET, "Requester abandoned request");
    }
}
