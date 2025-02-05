//   Copyright 2023. The Tari Project
//
//   Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
//   following conditions are met:
//
//   1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
//   disclaimer.
//
//   2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
//   following disclaimer in the documentation and/or other materials provided with the distribution.
//
//   3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
//   products derived from this software without specific prior written permission.
//
//   THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
//   INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//   DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//   SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//   SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
//   WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
//   USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

// TODO: rewrite downloader to get template from other peer(s) OR completely drop this concept and implement somewhere
// else

use std::{
    future::poll_fn,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{future::BoxFuture, stream::FuturesUnordered};
use prost::bytes;
use tari_common_types::types::FixedHash;
use tari_dan_storage::global::DbTemplateType;
use tari_template_lib::models::TemplateAddress;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

pub struct TemplateSyncRequest {
    pub address: TemplateAddress,
    pub expected_binary_hash: FixedHash,
}

pub enum SyncWorkerEvent {
    SyncCompleted { template_address: TemplateAddress },
}

pub(super) struct TemplateSyncWorker {
    sync_queue: mpsc::Receiver<TemplateSyncRequest>,
    // pending_sync: FuturesUnordered<BoxFuture<'static, DownloadResult>>,
    pending_sync: Option<BoxFuture<'static, TemplateSyncResult>>,
    pending_events: Vec<SyncWorkerEvent>,
}

impl TemplateSyncWorker {
    pub fn new(sync_queue: mpsc::Receiver<TemplateSyncRequest>) -> Self {
        Self {
            sync_queue,
            pending_sync: None,
            pending_events: Vec::new(),
        }
    }

    pub async fn next(&mut self) -> Option<SyncWorkerEvent> {
        poll_fn(|cx| self.poll_next(cx)).await
    }

    pub fn poll_next(&mut self, cx: &mut Context) -> Poll<Option<SyncWorkerEvent>> {
        loop {
            if let Some(event) = self.pending_events.pop() {
                return Poll::Ready(Some(event));
            }
            shrink_array(&mut self.pending_events);

            // Work on syncing item
            if let Some(mut pending) = self.pending_sync.take() {
                match pending.as_mut().poll(cx) {
                    Poll::Ready(result) => self.pending_events.push(SyncWorkerEvent::SyncCompleted {
                        template_address: result.template_address,
                    }),
                    Poll::Pending => {
                        self.pending_sync = Some(pending);
                        return Poll::Pending;
                    },
                }
            }

            loop {
                match self.sync_queue.try_recv() {
                    Ok(req) => {
                        self.pending_sync = Some(Box::pin(do_sync(req)));
                    },
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => return Poll::Ready(None),
                }
            }
        }
    }
}

async fn do_sync(req: TemplateSyncRequest) -> TemplateSyncResult {
    async fn inner(req: TemplateSyncRequest) -> Result<Bytes, TemplateSyncError> {
        let resp = reqwest::get(&req.url).await?;
        let resp = resp.error_for_status()?;
        let bytes = resp.bytes().await?;
        Ok(bytes)
    }

    TemplateSyncResult {
        template_address: req.address,
        template_type: req.template_type.clone(),
        result: inner(req).await,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateSyncError {
    #[error("Failed to download template: {0}")]
    DownloadFailed(#[from] reqwest::Error),
}

#[derive(Debug)]
pub struct TemplateSyncResult {
    pub template_address: TemplateAddress,
    pub template_type: DbTemplateType,
    pub result: Result<Bytes, TemplateSyncError>,
}

fn shrink_array<T>(vec: &mut Vec<T>) {
    const MAX_VEC_SHRINK_SIZE: usize = 500;
    let cap = vec.capacity();
    let len = vec.len();
    if len > MAX_VEC_SHRINK_SIZE {
        // Shrink once items are removed
        return;
    }
    if cap > MAX_VEC_SHRINK_SIZE {
        vec.shrink_to(MAX_VEC_SHRINK_SIZE);
    }
}
