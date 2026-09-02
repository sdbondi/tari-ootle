//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{marker::PhantomData, time::Duration};

use log::*;
use tari_epoch_manager::EpochManagerReader;
use tokio::time;

use crate::{
    hotstuff::{
        HotStuffError,
        state_machine::{event::ConsensusStateEvent, worker::ConsensusWorkerContext},
    },
    traits::ConsensusSpec,
};

const LOG_TARGET: &str = "tari::ootle::consensus::sm::initialising";

/// The state a node occupies before it knows anything about the epoch it is in: the base layer has not
/// been scanned, so neither this node's registration nor its committee is resolvable yet, and no view
/// has been entered.
///
/// Every other state can answer for the node in some way - `Idle` says it is registered for no
/// committee this epoch, `Running` that it is participating - and a peer asking what this node knows
/// can be told which. Occupying `Idle` while the answer is simply not known yet conflates the two.
#[derive(Debug, Clone)]
pub struct Initialising<TSpec> {
    _spec: PhantomData<TSpec>,
    delay: bool,
}

impl<TSpec> Initialising<TSpec>
where TSpec: ConsensusSpec
{
    pub fn new() -> Self {
        Self {
            _spec: PhantomData,
            delay: false,
        }
    }

    pub fn with_delay() -> Self {
        Self {
            _spec: PhantomData,
            delay: true,
        }
    }

    pub(super) async fn on_enter(
        &self,
        context: &mut ConsensusWorkerContext<TSpec>,
    ) -> Result<ConsensusStateEvent, HotStuffError> {
        debug!(target: LOG_TARGET, "Entered initialising state with delay: {}", self.delay);
        if self.delay {
            time::sleep(Duration::from_secs(5)).await;
        }
        context.epoch_manager.wait_for_initial_scanning_to_complete().await?;
        Ok(ConsensusStateEvent::Initialised)
    }
}
