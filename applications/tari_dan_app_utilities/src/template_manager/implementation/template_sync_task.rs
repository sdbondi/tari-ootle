//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{collections::HashMap, sync::Arc};

use log::{error, info, warn};
use tari_common_types::types::FixedHash;
use tari_dan_common_types::{Epoch, PeerAddress, ShardGroup, SubstateAddress};
use tari_dan_p2p::{
    proto::rpc::{SyncTemplatesRequest, TemplateType},
    TariMessagingSpec,
};
use tari_dan_storage::global::{DbTemplateType, DbTemplateUpdate, TemplateStatus};
use tari_engine_types::{
    hashing::template_hasher32,
    published_template::PublishedTemplateAddress,
    substate::SubstateId,
};
use tari_epoch_manager::{
    base_layer::{EpochManagerHandle, NetworkCommitteeInfo},
    EpochManagerReader,
};
use tari_template_lib::models::TemplateAddress;
use tari_validator_node_rpc::{
    client::{TariValidatorNodeRpcClient, TariValidatorNodeRpcClientFactory, ValidatorNodeClientFactory},
    rpc_service,
};
use tokio::task::JoinHandle;

use crate::template_manager::{
    implementation::TemplateManager,
    interface::{SyncTemplatesResult, TemplateManagerError},
};

const LOG_TARGET: &str = "tari::dan::template_manager::sync_task";

pub struct TemplateSyncClientTask {
    client_factory: Arc<TariValidatorNodeRpcClientFactory>,
    template_manager: TemplateManager<PeerAddress>,
    epoch_manager: EpochManagerHandle<PeerAddress>,
    templates_to_sync: Vec<TemplateAddress>,
    rpc_client_cache: HashMap<ShardGroup, TariValidatorNodeRpcClient<TariMessagingSpec>>,
}

impl TemplateSyncClientTask {
    pub fn new(
        client_factory: Arc<TariValidatorNodeRpcClientFactory>,
        template_manager: TemplateManager<PeerAddress>,
        epoch_manager: EpochManagerHandle<PeerAddress>,
        templates_to_sync: Vec<TemplateAddress>,
    ) -> Self {
        Self {
            client_factory,
            template_manager,
            epoch_manager,
            templates_to_sync,
            rpc_client_cache: HashMap::new(),
        }
    }

    pub async fn run(mut self) -> Result<Vec<TemplateAddress>, TemplateManagerError> {
        let address_batches = self.templates_to_sync.chunks(100);
        let current_epoch = self.epoch_manager.current_epoch().await?;
        let network_info = self.epoch_manager.get_network_committee_info(current_epoch).await?;
        let mut failed_addresses = Vec::new();
        for addresses in address_batches {
            for address in addresses {
                let mut sync_successful = false;
                let mut client = match self
                    .try_get_client_for_template_address(address, &network_info, current_epoch)
                    .await?
                {
                    Ok(client) => client,
                    Err(error) => {
                        error!(target: LOG_TARGET, "Failed to get client for template address: {error}");
                        failed_addresses.push(*address);
                        continue;
                    },
                };

                // syncing current part of batch
                if let Err(err) = self.try_sync_templates(&mut client, addresses).await {
                    error!(target: LOG_TARGET, "Failed to sync templates: {error}");
                    failed_addresses.extend(addresses);
                    continue;
                }
                match client
                    .sync_templates(SyncTemplatesRequest {
                        addresses: addresses.iter().map(|address| address.to_vec()).collect(),
                    })
                    .await
                {
                    Ok(mut stream) => {
                        while let Some(result) = stream.next().await {
                            match result {
                                Ok(resp) => {
                                    // code
                                    let mut compiled_code = None;
                                    let mut flow_json = None;
                                    let mut manifest = None;
                                    let template_type: DbTemplateType;
                                    let bin_hash = FixedHash::from(
                                        template_hasher32().chain(resp.binary.as_slice()).result().into_array(),
                                    );
                                    match resp.template_type() {
                                        TemplateType::Wasm => {
                                            compiled_code = Some(resp.binary);
                                            template_type = DbTemplateType::Wasm;
                                        },
                                        TemplateType::Manifest => {
                                            manifest = Some(String::from_utf8(resp.binary)?);
                                            template_type = DbTemplateType::Manifest;
                                        },
                                        TemplateType::Flow => {
                                            flow_json = Some(String::from_utf8(resp.binary)?);
                                            template_type = DbTemplateType::Flow;
                                        },
                                    }

                                    // get template address
                                    let template_address_result = TemplateAddress::try_from_vec(resp.address);
                                    if let Err(error) = template_address_result {
                                        error!(target: LOG_TARGET, "Invalid template address: {error:?}");
                                        continue;
                                    }
                                    let template_address = template_address_result.unwrap();

                                    if let Err(error) = template_manager.update_template(
                                        template_address,
                                        DbTemplateUpdate::template(
                                            FixedHash::try_from(resp.author_public_key.to_vec())?,
                                            Some(bin_hash),
                                            resp.template_name,
                                            template_type,
                                            compiled_code,
                                            flow_json,
                                            manifest,
                                        ),
                                    ) {
                                        error!(target: LOG_TARGET, "Failed to add new template: {error:?}");
                                        continue;
                                    }

                                    // remove from addresses to be able to send back a list of not
                                    // synced templates (if any)
                                    for (i, addr) in addresses.iter().enumerate() {
                                        if *addr == template_address {
                                            addresses.remove(i);
                                            break;
                                        }
                                    }

                                    sync_successful = true;
                                    info!(target: LOG_TARGET, "✅ Template synced successfully: {}", template_address);
                                    break;
                                },
                                Err(error) => {
                                    warn!(target: LOG_TARGET, "Can't get stream of templates from VN({addr}): {error:?}");
                                },
                            }
                        }
                    },
                    Err(error) => {
                        warn!(target: LOG_TARGET, "Can't get stream of templates from VN({addr}): {error:?}");
                    },
                }

                if sync_successful {
                    break;
                }
            }
        }
        Ok(addresses)
    }

    async fn try_get_client_for_template_address(
        &mut self,
        address: &TemplateAddress,
        network_info: &NetworkCommitteeInfo,
        current_epoch: Epoch,
    ) -> Result<rpc_service::ValidatorNodeRpcClient, TemplateManagerError> {
        let substate_id = SubstateId::from(PublishedTemplateAddress::from_hash(*address));
        // Version does not affect which committee is selected
        let address = SubstateAddress::from_substate_id(&substate_id, 0);
        let shard_group = address.to_shard_group(network_info.num_preshards, network_info.num_committees);

        if let Some(client) = self.rpc_client_cache.get_mut(&shard_group) {
            match client.client_connection().await {
                Ok(client) => {
                    return Ok(client);
                },
                Err(error) => {
                    error!(target: LOG_TARGET, "Failed to connect to VN: {error}");
                    self.rpc_client_cache.remove(&shard_group);
                },
            }
        }

        let vn = self
            .epoch_manager
            .get_random_committee_member_from_shard_group(current_epoch, shard_group)
            .await?;

        let client = self.client_factory.create_client(vn.address).await?;
        self.rpc_client_cache.insert(shard_group, client.clone());
        Ok(client)
    }
}
