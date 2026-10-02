//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::HashMap,
    ops::Deref,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::anyhow;
use futures::{StreamExt, TryStreamExt};
use log::warn;
use reqwest::{IntoUrl, StatusCode, Url};
use tari_engine_types::{
    Utxo,
    substate::{Substate, SubstateId},
};
use tari_indexer_client::{
    error::IndexerRestClientError,
    event::{IndexerEvent, TransactionFinalizedEvent},
    protobuf,
    rest_api_client::IndexerRestApiClient,
    types::{
        GetSubstateRequest,
        GetSubstatesRequest,
        GetTransactionResultRequest,
        GetUtxoUpdatesRequest,
        GetUtxosRequest,
        IndexerTransactionFinalizedResult,
        ListWatchedSubstatesRequest,
        SubmitTransactionRequest,
        WatchedSubstateItem,
    },
};
use tari_ootle_common_types::{
    Epoch,
    StateVersion,
    SubstateVersion,
    array_utils::copy_fixed_checked,
    displayable::Displayable,
    optional::IsNotFoundError,
    response_status::{ResponseErrorStatus, TransactionStatusResponseError},
    shard::Shard,
};
use tari_ootle_transaction::{Transaction, TransactionEnvelope, TransactionId};
use tari_ootle_wallet_sdk::{
    models::{EndOfShard, StartOfShard, UtxoBurnt, UtxoSpent, UtxoUnspent, UtxoUpdatePayload, WalletUtxoUpdate},
    network::{
        SubstateQueryResult,
        TransactionFinalizedNotification,
        TransactionFinalizedResult,
        TransactionFinalizedStream,
        TransactionQueryResult,
        UtxoUpdateStream,
        WalletNetworkInterface,
    },
};
use tari_template_lib_types::{
    ResourceAddress,
    TemplateAddress,
    UtxoId,
    crypto::{RistrettoPublicKeyBytes, UtxoTag},
};
use time::{OffsetDateTime, PrimitiveDateTime};
use url::ParseError;

const LOG_TARGET: &str = "tari::ootle::wallet_services::indexer_rest_api";
const INVALID_REQUEST_CODE: i64 = 400;

/// Consecutive unavailability failures on the active indexer after which the next configured indexer is used.
const FAILOVER_THRESHOLD: u32 = 3;
/// How long to wait for a TCP connection to an indexer. Bounds how long an unreachable host holds up a request before
/// it counts towards failover. Only the connect is bounded: SSE subscriptions and long polls legitimately stay open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The wallet's connection to the network through one of a set of indexers.
///
/// One indexer is active at a time, chosen at random from the set so that wallets sharing a configuration spread
/// across it. After [`FAILOVER_THRESHOLD`] consecutive requests find the active indexer unavailable, the next indexer
/// in the set becomes active. A deployment behind a load balancer is configured as a single URL and never fails over.
#[derive(Debug, Clone)]
pub struct IndexerRestApiNetworkInterface {
    client: IndexerRestApiClient,
    endpoints: Arc<Mutex<EndpointPool>>,
}

#[derive(Debug)]
struct EndpointPool {
    urls: Vec<Url>,
    active: usize,
    consecutive_failures: u32,
}

impl EndpointPool {
    /// A pool over `urls` with `preferred` active if it is among them, otherwise one chosen at random.
    fn init(urls: Vec<Url>, preferred: Option<&Url>) -> Result<Self, IndexerRestApiNetworkInterfaceError> {
        if urls.is_empty() {
            return Err(IndexerRestApiNetworkInterfaceError::NoIndexerEndpoints);
        }
        let active = preferred
            .and_then(|preferred| urls.iter().position(|url| url == preferred))
            .unwrap_or_else(|| rand::random_range(0..urls.len()));
        Ok(Self {
            urls,
            active,
            consecutive_failures: 0,
        })
    }

    fn active_url(&self) -> &Url {
        &self.urls[self.active]
    }
}

/// How a request's result bears on whether its indexer is serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexerHealth {
    /// The indexer answered.
    Answered,
    /// The indexer could not be reached, or a gateway in front of it reports it unavailable: a 502 or 504, or a 503
    /// without the indexer's own error body.
    Unavailable,
    /// Nothing can be concluded, e.g. the indexer is rate limiting this wallet.
    Inconclusive,
}

impl IndexerHealth {
    fn of<T>(result: &Result<T, IndexerRestClientError>) -> Self {
        let Err(err) = result else {
            return Self::Answered;
        };
        match err {
            IndexerRestClientError::RequestFailed { source } => match source.status() {
                None => Self::Unavailable,
                Some(status) => Self::of_status(status, false),
            },
            IndexerRestClientError::ErrorResponse { source, details } => match source.status() {
                None => Self::Unavailable,
                Some(status) => Self::of_status(status, details.is_some()),
            },
            _ => Self::Inconclusive,
        }
    }

    /// The indexer itself answers 503 when its dry-run slots are busy or the network cannot serve a request. Neither
    /// means the indexer is down, so only a 503 without the indexer's error body is a gateway reporting an outage.
    fn of_status(status: StatusCode, has_indexer_error_body: bool) -> Self {
        match status {
            StatusCode::TOO_MANY_REQUESTS => Self::Inconclusive,
            StatusCode::BAD_GATEWAY | StatusCode::GATEWAY_TIMEOUT => Self::Unavailable,
            StatusCode::SERVICE_UNAVAILABLE if !has_indexer_error_body => Self::Unavailable,
            _ => Self::Answered,
        }
    }
}

/// A client paired with the indexer that was active when it was handed out. Its results count towards that indexer
/// only while it is still active; once another indexer is active they are discarded.
struct TrackedClient {
    client: IndexerRestApiClient,
    endpoint: Url,
}

impl Deref for TrackedClient {
    type Target = IndexerRestApiClient;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl IndexerRestApiNetworkInterface {
    pub fn new<T: IntoUrl>(url: T) -> Self {
        Self::init(vec![url.into_url().expect("Malformed indexer URL")]).expect("Malformed indexer URL")
    }

    /// Connects through the indexers at `endpoints`, starting from one chosen at random.
    pub fn init(endpoints: Vec<Url>) -> Result<Self, IndexerRestApiNetworkInterfaceError> {
        let pool = EndpointPool::init(endpoints, None)?;
        let client = IndexerRestApiClient::connect_with_connect_timeout(pool.active_url().clone(), CONNECT_TIMEOUT)?;
        Ok(Self {
            client,
            endpoints: Arc::new(Mutex::new(pool)),
        })
    }

    /// Replaces the set of indexers. The active indexer stays active if it is in the new set; otherwise one of the new
    /// set is activated at random. Requests already in flight finish against the indexer they were sent to.
    pub fn set_endpoints(&self, endpoints: Vec<Url>) -> Result<(), IndexerRestApiNetworkInterfaceError> {
        let mut current = self.endpoints.lock().unwrap();
        let pool = EndpointPool::init(endpoints, Some(current.active_url()))?;
        self.client.set_endpoint(pool.active_url().clone())?;
        *current = pool;
        Ok(())
    }

    /// The configured set of indexers.
    pub fn get_endpoints(&self) -> Vec<Url> {
        self.endpoints.lock().unwrap().urls.clone()
    }

    /// The indexer requests are currently sent to.
    pub fn get_endpoint(&self) -> Url {
        self.client.endpoint()
    }

    fn tracked_client(&self) -> TrackedClient {
        let pool = self.endpoints.lock().unwrap();
        TrackedClient {
            client: self.client.clone(),
            endpoint: pool.active_url().clone(),
        }
    }

    /// Records what `result` says about the indexer `client` was sent to, failing over once the active indexer has
    /// been unavailable [`FAILOVER_THRESHOLD`] times in a row, and passes `result` through.
    fn observe<T>(
        &self,
        client: &TrackedClient,
        result: Result<T, IndexerRestClientError>,
    ) -> Result<T, IndexerRestClientError> {
        self.record_health(&client.endpoint, IndexerHealth::of(&result));
        result
    }

    fn record_health(&self, endpoint: &Url, health: IndexerHealth) {
        let mut pool = self.endpoints.lock().unwrap();
        // A result from an indexer that is no longer active says nothing about the active one.
        if pool.active_url() != endpoint {
            return;
        }
        match health {
            IndexerHealth::Answered => pool.consecutive_failures = 0,
            IndexerHealth::Inconclusive => {},
            IndexerHealth::Unavailable => {
                if pool.urls.len() < 2 {
                    return;
                }
                pool.consecutive_failures += 1;
                if pool.consecutive_failures < FAILOVER_THRESHOLD {
                    return;
                }
                let failed = pool.active;
                pool.active = (failed + 1) % pool.urls.len();
                pool.consecutive_failures = 0;
                let next = pool.active_url().clone();
                warn!(
                    target: LOG_TARGET,
                    "Indexer {} was unavailable for {FAILOVER_THRESHOLD} consecutive requests, switching to {next}",
                    pool.urls[failed]
                );
                if let Err(err) = self.client.set_endpoint(next) {
                    warn!(target: LOG_TARGET, "Failed to switch to the next indexer: {err}");
                }
            },
        }
    }
}

impl WalletNetworkInterface for IndexerRestApiNetworkInterface {
    type Error = IndexerRestApiNetworkInterfaceError;

    async fn query_substate(
        &self,
        substate_id: &SubstateId,
        version: Option<SubstateVersion>,
        local_search_only: bool,
    ) -> Result<SubstateQueryResult, Self::Error> {
        let client = self.tracked_client();
        let result = self.observe(
            &client,
            client
                .get_substate(substate_id, GetSubstateRequest {
                    version,
                    local_search_only,
                })
                .await,
        )?;
        Ok(SubstateQueryResult {
            version: result.version,
            substate: result.substate,
        })
    }

    async fn get_substates(&self, substate_ids: Vec<SubstateId>) -> Result<HashMap<SubstateId, Substate>, Self::Error> {
        // The indexer's substates/fetch endpoint accepts at most this many IDs per request, so larger
        // requests are split into sequential batches. Substates the indexer cannot find are omitted from
        // the result, not an error.
        const MAX_IDS_PER_REQUEST: usize = 20;

        let client = self.tracked_client();
        let mut substates = HashMap::with_capacity(substate_ids.len());
        for chunk in substate_ids.chunks(MAX_IDS_PER_REQUEST) {
            let resp = self.observe(
                &client,
                client
                    .fetch_substates(GetSubstatesRequest {
                        requests: chunk.to_vec().try_into().map_err(|_| {
                            IndexerRestApiNetworkInterfaceError::IndexerClientError(
                                IndexerRestClientError::RequestInvariant {
                                    details: "Too many substate IDs requested".to_string(),
                                },
                            )
                        })?,
                        cached_only: false,
                    })
                    .await,
            )?;
            substates.extend(resp.substates);
        }

        Ok(substates)
    }

    async fn submit_transaction(&self, transaction: Transaction) -> Result<TransactionId, Self::Error> {
        let transaction = TransactionEnvelope::encode(transaction)?;
        self.submit_transaction_envelope(transaction).await
    }

    async fn submit_transaction_envelope(
        &self,
        transaction: TransactionEnvelope,
    ) -> Result<TransactionId, Self::Error> {
        let client = self.tracked_client();
        let result = self.observe(
            &client,
            client
                .submit_transaction(SubmitTransactionRequest { transaction })
                .await,
        )?;
        Ok(result.transaction_id)
    }

    async fn submit_dry_run_transaction(
        &self,
        transaction: Transaction,
    ) -> Result<TransactionQueryResult, Self::Error> {
        if !transaction.is_dry_run() {
            return Err(IndexerRestApiNetworkInterfaceError::IndexerClientError(
                IndexerRestClientError::RequestFailedWithStatus {
                    code: INVALID_REQUEST_CODE,
                    message: "Transaction must be marked as dry-run".to_string(),
                },
            ));
        }

        let client = self.tracked_client();
        let resp = self.observe(
            &client,
            client
                .submit_transaction_dry_run(SubmitTransactionRequest {
                    transaction: TransactionEnvelope::encode(transaction)?,
                })
                .await,
        )?;

        Ok(TransactionQueryResult {
            transaction_id: resp.transaction_id,
            // TODO: clean this up
            result: TransactionFinalizedResult::Finalized {
                final_decision: (&resp.result.finalize.result).into(),
                execution_time: resp.result.execution_time,
                execution_result: Some(Box::new(resp.result)),
                finalized_time: now(),
                abort_details: None,
            },
        })
    }

    async fn query_transaction_result(
        &self,
        transaction_id: TransactionId,
    ) -> Result<TransactionQueryResult, Self::Error> {
        let client = self.tracked_client();
        let resp = self.observe(
            &client,
            client
                .get_transaction_result(GetTransactionResultRequest { transaction_id })
                .await,
        )?;

        Ok(TransactionQueryResult {
            transaction_id,
            result: convert_indexer_result_to_wallet_result(resp.result),
        })
    }

    async fn subscribe_transaction_finalized(&self) -> Result<TransactionFinalizedStream<Self::Error>, Self::Error> {
        let client = self.tracked_client();
        let events = self.observe(&client, client.sse_events().await)?;
        let stream = events
            .map_err(|e| IndexerRestApiNetworkInterfaceError::StreamDecodeError(e.into()))
            .try_filter_map(|event| async move {
                if event.event_type != IndexerEvent::TRANSACTION_FINALIZED_EVENT_NAME {
                    return Ok(None);
                }
                // A payload this client cannot decode is skipped rather than ending the subscription: the wallet
                // queries a transaction's result after it has stayed silent, so a skipped event costs latency, not
                // correctness.
                let event: TransactionFinalizedEvent = match event.try_parse_event() {
                    Ok(event) => event,
                    Err(e) => {
                        warn!(
                            target: LOG_TARGET,
                            "Skipping undecodable {} event: {e}",
                            IndexerEvent::TRANSACTION_FINALIZED_EVENT_NAME
                        );
                        return Ok(None);
                    },
                };
                Ok(Some(TransactionFinalizedNotification {
                    transaction_id: event.transaction_id,
                    outcome: event.outcome,
                }))
            });
        Ok(stream.boxed())
    }

    async fn fetch_template_definition(
        &self,
        template_address: TemplateAddress,
    ) -> Result<tari_template_abi::TemplateDef, Self::Error> {
        let client = self.tracked_client();
        let resp = self.observe(&client, client.get_template_definition(template_address).await)?;
        Ok(resp.definition)
    }

    async fn stream_stealth_utxo_updates(
        &self,
        from_epoch: Epoch,
        resource_address: ResourceAddress,
        shard_state_versions: Vec<(Shard, StateVersion)>,
        unspent_only: bool,
    ) -> Result<UtxoUpdateStream<Self::Error>, Self::Error> {
        let client = self.tracked_client();
        let stream = self.observe(
            &client,
            client
                .stream_utxo_updates_protobuf(GetUtxoUpdatesRequest {
                    from_epoch,
                    shard_state_versions,
                    resource_address,
                    unspent_only,
                    per_shard_limit: 1000,
                })
                .await,
        )?;
        let stream = stream
            .map_err(|e| IndexerRestApiNetworkInterfaceError::StreamDecodeError(e.into()))
            .and_then(|res| async move {
                let sos = res.sos.map(|sos| StartOfShard {
                    shard: Shard::from(sos.shard),
                    max_state_version: StateVersion::from(sos.max_state_version),
                    has_more: sos.has_more,
                });
                let update = res
                    .update
                    .map(|u| match u {
                        protobuf::WalletUtxoUpdate::Unspent(unspent) => {
                            let public_nonce =
                                RistrettoPublicKeyBytes::from_bytes(&unspent.public_nonce).map_err(|e| {
                                    IndexerRestApiNetworkInterfaceError::StreamDecodeError(anyhow!(
                                        "Failed to decode public nonce: {e}"
                                    ))
                                })?;
                            Ok::<_, IndexerRestApiNetworkInterfaceError>(WalletUtxoUpdate::Unspent(UtxoUnspent {
                                tag: unspent.tag.into(),
                                public_nonce,
                            }))
                        },
                        protobuf::WalletUtxoUpdate::Spent(spent) => {
                            let id_arr = copy_fixed_checked(&spent.id).ok_or_else(|| {
                                IndexerRestApiNetworkInterfaceError::StreamDecodeError(anyhow!(
                                    "Failed to decode UTXO ID, incorrect length"
                                ))
                            })?;
                            Ok::<_, IndexerRestApiNetworkInterfaceError>(WalletUtxoUpdate::Spent(UtxoSpent {
                                id: UtxoId::from_array(id_arr),
                                version: SubstateVersion::new(spent.version),
                            }))
                        },
                        protobuf::WalletUtxoUpdate::Burnt(burnt) => {
                            let id_arr = copy_fixed_checked(&burnt.id).ok_or_else(|| {
                                IndexerRestApiNetworkInterfaceError::StreamDecodeError(anyhow!(
                                    "Failed to decode UTXO ID, incorrect length"
                                ))
                            })?;
                            Ok::<_, IndexerRestApiNetworkInterfaceError>(WalletUtxoUpdate::Burnt(UtxoBurnt {
                                id: UtxoId::from_array(id_arr),
                                version: SubstateVersion::new(burnt.version),
                            }))
                        },
                    })
                    .transpose()?;
                let eos = res.eos.map(|eos| EndOfShard {
                    max_state_version: eos.max_state_version.into(),
                });

                Ok(UtxoUpdatePayload { sos, update, eos })
            });
        Ok(stream.boxed())
    }

    async fn get_unspent_utxos(
        &self,
        resource_address: ResourceAddress,
        tag_and_nonce_pairs: Vec<(UtxoTag, RistrettoPublicKeyBytes)>,
    ) -> Result<Vec<(UtxoId, Utxo)>, Self::Error> {
        let client = self.tracked_client();
        // TODO: Given the potential size of substates protobuf, json + hex encoding may be too inefficient. Consider
        // supporting the application/x-protobuf content type in the indexer REST API.
        let resp = self.observe(
            &client,
            client
                .get_utxos(GetUtxosRequest {
                    resource_address,
                    tag_and_nonce_pairs,
                })
                .await,
        )?;
        Ok(resp.utxos)
    }

    async fn list_watched_substates(
        &self,
        template_address: Option<TemplateAddress>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> Result<Vec<WatchedSubstateItem>, Self::Error> {
        let client = self.tracked_client();

        let resp = self.observe(
            &client,
            client
                .list_watched_substates(ListWatchedSubstatesRequest {
                    template_address,
                    limit,
                    offset,
                })
                .await,
        )?;

        Ok(resp.substates)
    }

    async fn get_current_epoch(&self) -> Result<Epoch, Self::Error> {
        let client = self.tracked_client();
        let stats = self.observe(&client, client.get_epoch_manager_stats().await)?;
        Ok(stats.consensus_epoch.unwrap_or(stats.current_epoch))
    }

    async fn wait_until_ready(&self) -> Result<(), Self::Error> {
        let client = self.tracked_client();
        self.observe(&client, client.wait_until_ready().await)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IndexerRestApiNetworkInterfaceError {
    #[error("Indexer client error: {0}")]
    IndexerClientError(#[from] IndexerRestClientError),
    #[error("Indexer parse error : {0}")]
    IndexerParseError(#[from] ParseError),
    #[error("Stream decode error: {0}")]
    StreamDecodeError(anyhow::Error),
    #[error("Transaction encode error: {source}")]
    EncodeError {
        #[from]
        source: tari_bor::BorError,
    },
    #[error("At least one indexer URL is required")]
    NoIndexerEndpoints,
}

impl IsNotFoundError for IndexerRestApiNetworkInterfaceError {
    fn is_not_found_error(&self) -> bool {
        match self {
            IndexerRestApiNetworkInterfaceError::IndexerClientError(err) => err.is_not_found_error(),
            _ => false,
        }
    }
}

impl TransactionStatusResponseError for IndexerRestApiNetworkInterfaceError {
    fn get_status(&self) -> ResponseErrorStatus {
        match self {
            IndexerRestApiNetworkInterfaceError::IndexerClientError(err) => {
                if err.is_not_found_error() {
                    return ResponseErrorStatus::NotFound {
                        message: "The requested resource was not found".to_string(),
                    };
                }
                match err {
                    IndexerRestClientError::RequestFailedWithStatus { code, message }
                        if *code == INVALID_REQUEST_CODE =>
                    {
                        ResponseErrorStatus::TransactionRejected {
                            message: message.clone(),
                        }
                    },
                    IndexerRestClientError::RequestFailedWithStatus { code, message } => {
                        ResponseErrorStatus::InternalError {
                            message: format!("Indexer request failed with status {code}: {message}"),
                        }
                    },
                    IndexerRestClientError::ErrorResponse { source, details } => {
                        if source.status().map(|s| s.as_u16()) == Some(INVALID_REQUEST_CODE as u16) {
                            ResponseErrorStatus::TransactionRejected {
                                message: format!("{}. Details: {}", source, details.display()),
                            }
                        } else {
                            ResponseErrorStatus::InternalError {
                                message: format!("Indexer error: {}", source),
                            }
                        }
                    },
                    _ => ResponseErrorStatus::InternalError {
                        message: format!("Indexer client error: {err}"),
                    },
                }
            },
            IndexerRestApiNetworkInterfaceError::IndexerParseError(e) => ResponseErrorStatus::InternalError {
                message: format!("Indexer parse error: {e}"),
            },
            IndexerRestApiNetworkInterfaceError::StreamDecodeError(e) => ResponseErrorStatus::InternalError {
                message: format!("Indexer stream decode error: {e}"),
            },
            IndexerRestApiNetworkInterfaceError::EncodeError { source } => ResponseErrorStatus::InternalError {
                message: format!("Transaction encode error: {source}"),
            },
            IndexerRestApiNetworkInterfaceError::NoIndexerEndpoints => ResponseErrorStatus::InternalError {
                message: self.to_string(),
            },
        }
    }

    fn get_error_message(&self) -> String {
        self.to_string()
    }
}

/// These types are identical, however in order to keep the wallet decoupled from the indexer, we define two types and
/// this conversion function.
// TODO: the common interface and types between the wallet and indexer could be made into a shared "view of the network"
// interface and we can avoid defining two types.
fn convert_indexer_result_to_wallet_result(result: IndexerTransactionFinalizedResult) -> TransactionFinalizedResult {
    match result {
        IndexerTransactionFinalizedResult::Pending => TransactionFinalizedResult::Pending,
        IndexerTransactionFinalizedResult::Finalized {
            final_decision,
            execution_result,
            finalized_time,
            execution_time,
            abort_details,
        } => TransactionFinalizedResult::Finalized {
            final_decision,
            execution_result,
            execution_time,
            finalized_time,
            abort_details,
        },
        IndexerTransactionFinalizedResult::Rejected { details, rejected_time } => {
            TransactionFinalizedResult::Rejected { details, rejected_time }
        },
    }
}

fn now() -> PrimitiveDateTime {
    let now = OffsetDateTime::now_utc();
    PrimitiveDateTime::new(now.date(), now.time())
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    fn urls(n: usize) -> Vec<Url> {
        (0..n)
            .map(|i| Url::parse(&format!("http://indexer-{i}.example:18300/")).unwrap())
            .collect()
    }

    fn active(network: &IndexerRestApiNetworkInterface) -> Url {
        network.endpoints.lock().unwrap().active_url().clone()
    }

    fn record(network: &IndexerRestApiNetworkInterface, health: IndexerHealth, times: u32) {
        for _ in 0..times {
            network.record_health(&active(network), health);
        }
    }

    #[test]
    fn the_next_indexer_is_used_after_the_active_one_is_unavailable_repeatedly() {
        let network = IndexerRestApiNetworkInterface::init(urls(3)).unwrap();
        let first = network.endpoints.lock().unwrap().active;

        record(&network, IndexerHealth::Unavailable, FAILOVER_THRESHOLD - 1);
        assert_eq!(network.endpoints.lock().unwrap().active, first);

        record(&network, IndexerHealth::Unavailable, 1);
        let next = (first + 1) % 3;
        assert_eq!(network.endpoints.lock().unwrap().active, next);
        assert_eq!(network.get_endpoint(), urls(3)[next]);
    }

    #[test]
    fn an_answer_resets_the_failure_count() {
        let network = IndexerRestApiNetworkInterface::init(urls(2)).unwrap();
        let first = active(&network);

        record(&network, IndexerHealth::Unavailable, FAILOVER_THRESHOLD - 1);
        record(&network, IndexerHealth::Answered, 1);
        record(&network, IndexerHealth::Unavailable, FAILOVER_THRESHOLD - 1);

        assert_eq!(active(&network), first);
    }

    #[test]
    fn an_inconclusive_result_neither_counts_nor_resets() {
        let network = IndexerRestApiNetworkInterface::init(urls(2)).unwrap();
        let first = active(&network);

        record(&network, IndexerHealth::Unavailable, FAILOVER_THRESHOLD - 1);
        record(&network, IndexerHealth::Inconclusive, FAILOVER_THRESHOLD);
        assert_eq!(active(&network), first);

        record(&network, IndexerHealth::Unavailable, 1);
        assert_ne!(active(&network), first);
    }

    #[test]
    fn results_from_a_previously_active_indexer_are_ignored() {
        let network = IndexerRestApiNetworkInterface::init(urls(2)).unwrap();
        let first = active(&network);
        record(&network, IndexerHealth::Unavailable, FAILOVER_THRESHOLD);
        let second = active(&network);

        for _ in 0..FAILOVER_THRESHOLD {
            network.record_health(&first, IndexerHealth::Unavailable);
        }

        assert_eq!(active(&network), second);
    }

    #[test]
    fn a_single_indexer_is_kept_however_often_it_fails() {
        let network = IndexerRestApiNetworkInterface::init(urls(1)).unwrap();

        record(&network, IndexerHealth::Unavailable, FAILOVER_THRESHOLD * 3);

        assert_eq!(network.get_endpoint(), urls(1)[0]);
    }

    #[test]
    fn an_empty_set_of_indexers_is_refused() {
        let err = IndexerRestApiNetworkInterface::init(vec![]).unwrap_err();
        assert!(matches!(err, IndexerRestApiNetworkInterfaceError::NoIndexerEndpoints));
        let network = IndexerRestApiNetworkInterface::init(urls(2)).unwrap();
        network.set_endpoints(vec![]).unwrap_err();
        assert_eq!(network.get_endpoints(), urls(2));
    }

    #[test]
    fn replacing_the_set_keeps_the_active_indexer_if_it_remains() {
        let network = IndexerRestApiNetworkInterface::init(urls(2)).unwrap();
        let active_before = active(&network);
        let mut extended = urls(2);
        extended.push(Url::parse("http://other.example:18300/").unwrap());

        for _ in 0..10 {
            network.set_endpoints(extended.clone()).unwrap();
            assert_eq!(network.get_endpoint(), active_before);
        }
    }

    #[test]
    fn replacing_the_set_activates_one_of_the_new_indexers() {
        let network = IndexerRestApiNetworkInterface::init(urls(2)).unwrap();
        let replacement = vec![Url::parse("http://other.example:18300/").unwrap()];

        network.set_endpoints(replacement.clone()).unwrap();

        assert_eq!(network.get_endpoints(), replacement);
        assert_eq!(network.get_endpoint(), replacement[0]);
    }

    /// Serves every request with `status` and `body`.
    async fn spawn_status_server(status: &'static str, body: &'static str) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ignore = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ignore = socket.write_all(response.as_bytes()).await;
            }
        });
        url
    }

    async fn health_of_request_to(url: Url) -> IndexerHealth {
        let client = IndexerRestApiClient::connect(url).unwrap();
        IndexerHealth::of(&client.get_epoch_manager_stats().await)
    }

    #[tokio::test]
    async fn responses_are_classified_by_what_they_say_about_the_indexer() {
        assert_eq!(
            health_of_request_to(spawn_status_server("503 Service Unavailable", "{}").await).await,
            IndexerHealth::Unavailable
        );
        assert_eq!(
            health_of_request_to(spawn_status_server("502 Bad Gateway", "{}").await).await,
            IndexerHealth::Unavailable
        );
        assert_eq!(
            health_of_request_to(spawn_status_server("504 Gateway Timeout", "<html></html>").await).await,
            IndexerHealth::Unavailable
        );
        // The indexer's own 503 (dry-run slots busy, no committee for a shard) is an answer.
        assert_eq!(
            health_of_request_to(spawn_status_server("503 Service Unavailable", r#"{"error":"busy"}"#).await).await,
            IndexerHealth::Answered
        );
        assert_eq!(
            health_of_request_to(spawn_status_server("429 Too Many Requests", "{}").await).await,
            IndexerHealth::Inconclusive
        );
        assert_eq!(
            health_of_request_to(spawn_status_server("404 Not Found", "{}").await).await,
            IndexerHealth::Answered
        );
        assert_eq!(
            health_of_request_to(spawn_status_server("500 Internal Server Error", "{}").await).await,
            IndexerHealth::Answered
        );
    }

    #[tokio::test]
    async fn an_unreachable_indexer_is_unavailable() {
        let unreachable = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap()
        };
        assert_eq!(health_of_request_to(unreachable).await, IndexerHealth::Unavailable);
    }
}
