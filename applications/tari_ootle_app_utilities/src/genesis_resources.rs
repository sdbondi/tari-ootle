//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_engine_types::resource::Resource;
use tari_ootle_transaction::Network;
use tari_template_lib::{
    prelude::{LOCKED, OWNER},
    types::{
        AccessRule,
        Metadata,
        ResourceAddress,
        ResourceType,
        SubstateOwnerRule,
        access_rules::ResourceAccessRules,
        constants::{PUBLIC_IDENTITY_RESOURCE_ADDRESS, STEALTH_TARI_RESOURCE_ADDRESS, TOKEN_SYMBOL},
        rule,
    },
};

/// The access rules every non-fungible resource in `network`'s genesis state starts from.
///
/// Esmeralda's genesis state was committed with non-fungible data updates open to every caller. Its genesis
/// resources keep that rule so that the genesis state, and the state roots built on it, stay the same.
pub fn genesis_non_fungible_access_rules(network: Network) -> ResourceAccessRules {
    match network {
        Network::Esmeralda => ResourceAccessRules::new().update_non_fungible_data(AccessRule::AllowAll, OWNER),
        Network::MainNet | Network::StageNet | Network::NextNet | Network::Igor | Network::LocalNet => {
            ResourceAccessRules::new()
        },
    }
}

pub fn get_public_identity_resource(network: Network) -> (ResourceAddress, Resource) {
    let value = Resource::new(
        ResourceType::NonFungible,
        SubstateOwnerRule::None,
        genesis_non_fungible_access_rules(network),
        Metadata::from([(TOKEN_SYMBOL, "ID".to_string())]),
        None,
        None,
        0,
        false,
    );
    (PUBLIC_IDENTITY_RESOURCE_ADDRESS, value)
}

pub fn get_stealth_tari_resource(network: Network) -> (ResourceAddress, Resource) {
    let symbol = if network.is_testnet() { "tTARI" } else { "TARI" };
    let xtr_resource = Resource::new(
        ResourceType::Stealth,
        SubstateOwnerRule::None,
        ResourceAccessRules::new()
            // These are defaults, but just for explicitness
            .mintable(rule!(deny_all), LOCKED)
            .burnable(rule!(deny_all), LOCKED)
            .recallable(rule!(deny_all), LOCKED)
            .freezable(rule!(deny_all), LOCKED),
        Metadata::from([(TOKEN_SYMBOL, symbol)]),
        None,
        None,
        6,
        // Disable total supply tracking for XTR. This is because it is not feasible to include "the fee exhaust" in
        // the tracking (as that would require mutating the resource on every transaction). Tracking supply can
        // be done by summing up the total burn claims (ClaimedOutputTombstone) and subtracting the total exhaust in
        // fee receipts.
        false,
    );
    (STEALTH_TARI_RESOURCE_ADDRESS, xtr_resource)
}

#[cfg(test)]
mod tests {
    use tari_template_lib::types::access_rules::ResourceAuthAction;

    use super::*;

    fn nft_data_rule(network: Network) -> AccessRule {
        let (_, resource) = get_public_identity_resource(network);
        resource
            .access_rules()
            .get_access_rule(&ResourceAuthAction::UpdateNonFungibleData)
            .clone()
    }

    #[test]
    fn esmeralda_genesis_keeps_its_committed_nft_data_rule() {
        assert_eq!(nft_data_rule(Network::Esmeralda), AccessRule::AllowAll);
    }

    #[test]
    fn other_networks_deny_nft_data_updates_in_genesis() {
        for network in [
            Network::MainNet,
            Network::StageNet,
            Network::NextNet,
            Network::Igor,
            Network::LocalNet,
        ] {
            assert_eq!(nft_data_rule(network), AccessRule::DenyAll, "{network}");
        }
    }
}
