// Copyright 2026 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

//! The exhaust burn rate as an indexer can see it.
//!
//! A validator reads the rate off the block header of the epoch a transaction is sequenced in. An
//! indexer follows state rather than blocks, and the epoch checkpoints it syncs carry the layer-1
//! shaped header, which holds a metadata hash rather than the extra data the rate lives in. So it
//! resolves the rate the same way a validator does at an epoch boundary: from the governance
//! component, which it can fetch like any other substate.

use log::*;
use tari_engine_types::{
    fees::{ExhaustBurnRate, ExhaustBurnRateSchedule, resolve_exhaust_burn_rate},
    substate::{SubstateId, SubstateValue},
};
use tari_ootle_common_types::{Epoch, SubstateRequirementRef};
use tari_ootle_transaction::Network;
use tari_template_lib_types::{
    constants::BURN_RATE_GOVERNANCE_COMPONENT_ADDRESS,
    governance::{BurnRateChange, BurnRateGovernanceState},
};

use crate::substate_manager::SubstateManager;

const LOG_TARGET: &str = "tari::indexer::exhaust_burn_rate";

/// The rate `epoch` runs at.
///
/// Reporting and dry-run estimation only, so a component that cannot be fetched or decoded falls
/// back to [`ExhaustBurnRateSchedule`] rather than failing the caller. The burn is a share of what
/// was collected and never part of what a transaction is charged, so a stale rate moves what a dry
/// run reports as burned and nothing a caller has to pay.
pub async fn resolve_exhaust_burn_rate_for_epoch(
    substate_manager: &SubstateManager,
    network: Network,
    epoch: Epoch,
) -> ExhaustBurnRate {
    resolve_burn_rate_outlook(substate_manager, network, epoch)
        .await
        .current
}

/// The rate `epoch` runs at, and what the council has scheduled beyond it.
#[derive(Debug, Clone)]
pub struct BurnRateOutlook {
    pub current: ExhaustBurnRate,
    /// Changes that activate after `epoch`, ascending. Entries that have already activated are
    /// folded into `current`.
    pub scheduled: Vec<BurnRateChange>,
    /// The first epoch the release-scheduled table governs again, once the council has retired.
    pub retired_from: Option<u64>,
}

/// One read of the governance component serving both the rate in force and the schedule ahead.
/// Falls back to the release-scheduled table with nothing scheduled, as
/// [`resolve_exhaust_burn_rate_for_epoch`] does.
pub async fn resolve_burn_rate_outlook(
    substate_manager: &SubstateManager,
    network: Network,
    epoch: Epoch,
) -> BurnRateOutlook {
    match read_governance_state(substate_manager).await {
        Ok(state) => {
            let governance = state.rate_at(epoch.as_u64()).and_then(ExhaustBurnRate::try_new);
            let scheduled = state
                .schedule
                .iter()
                .copied()
                .filter(|change| change.activation_epoch > epoch.as_u64())
                .filter(|change| state.retired_from.is_none_or(|from| change.activation_epoch < from))
                .collect();
            BurnRateOutlook {
                current: resolve_exhaust_burn_rate(network, epoch, governance),
                scheduled,
                retired_from: state.retired_from,
            }
        },
        Err(err) => {
            debug!(
                target: LOG_TARGET,
                "Burn rate governance component unavailable ({err}). Reporting {epoch} at the scheduled rate."
            );
            BurnRateOutlook {
                current: ExhaustBurnRateSchedule::at(network, epoch),
                scheduled: Vec::new(),
                retired_from: None,
            }
        },
    }
}

async fn read_governance_state(substate_manager: &SubstateManager) -> Result<BurnRateGovernanceState, anyhow::Error> {
    let substate_id = SubstateId::Component(BURN_RATE_GOVERNANCE_COMPONENT_ADDRESS);
    let substate = substate_manager
        .get_substate(SubstateRequirementRef::unversioned(&substate_id))
        .await?;

    let SubstateValue::Component(component) = substate.substate_value() else {
        anyhow::bail!("{substate_id} is not a component");
    };

    Ok(tari_bor::from_value(component.state())?)
}
