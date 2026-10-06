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
        constants::{
            NFT_FAUCET_COMPONENT_ADDRESS,
            NFT_FAUCET_RESOURCE_ADDRESS,
            PUBLIC_IDENTITY_RESOURCE_ADDRESS,
            STEALTH_TARI_RESOURCE_ADDRESS,
            TOKEN_SYMBOL,
            XTR_FAUCET_CLAIM_RESOURCE_ADDRESS,
            XTR_FAUCET_COMPONENT_ADDRESS,
        },
        rule,
    },
};

/// The access rules every resource in `network`'s genesis state starts from.
///
/// Esmeralda's genesis state was committed with non-fungible data updates open to every caller. Its genesis
/// resources keep that rule so that the genesis state, and the state roots built on it, stay the same.
pub fn genesis_resource_access_rules(network: Network) -> ResourceAccessRules {
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
        genesis_resource_access_rules(network),
        Metadata::from([(TOKEN_SYMBOL, "ID".to_string())]),
        None,
        None,
        0,
        false,
    );
    (PUBLIC_IDENTITY_RESOURCE_ADDRESS, value)
}

/// The tTARI faucet's claim receipt resource: one NFT per claimant public key, burned as soon as it is minted. The
/// burned substate key persists on-chain, preventing duplicate claims.
pub fn get_xtr_faucet_claim_resource(network: Network) -> (ResourceAddress, Resource) {
    let resource = Resource::new(
        ResourceType::NonFungible,
        SubstateOwnerRule::None,
        genesis_resource_access_rules(network)
            .mintable(rule!(component(XTR_FAUCET_COMPONENT_ADDRESS)), LOCKED)
            .burnable(rule!(component(XTR_FAUCET_COMPONENT_ADDRESS)), LOCKED),
        Metadata::new(),
        None,
        None,
        0,
        false,
    );
    (XTR_FAUCET_CLAIM_RESOURCE_ADDRESS, resource)
}

pub fn get_nft_faucet_resource(network: Network) -> (ResourceAddress, Resource) {
    let resource = Resource::new(
        ResourceType::NonFungible,
        SubstateOwnerRule::None,
        genesis_resource_access_rules(network).mintable(rule!(component(NFT_FAUCET_COMPONENT_ADDRESS)), LOCKED),
        Metadata::from([("name", "NFT Faucet"), (TOKEN_SYMBOL, "tNFT")]),
        None,
        None,
        0,
        true,
    );
    (NFT_FAUCET_RESOURCE_ADDRESS, resource)
}

pub fn get_stealth_tari_resource(network: Network) -> (ResourceAddress, Resource) {
    let symbol = if network.is_testnet() { "tTARI" } else { "TARI" };
    let xtr_resource = Resource::new(
        ResourceType::Stealth,
        SubstateOwnerRule::None,
        genesis_resource_access_rules(network)
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
    use tari_engine_types::{Epoch, SubstateVersion, substate::hash_substate};
    use tari_template_lib::types::access_rules::ResourceAuthAction;

    use super::*;

    fn nft_data_rule(network: Network) -> AccessRule {
        let (_, resource) = get_public_identity_resource(network);
        resource
            .access_rules()
            .get_access_rule(&ResourceAuthAction::UpdateNonFungibleData)
            .clone()
    }

    /// Pins Esmeralda's genesis resources to the substate hashes its genesis state was committed with. A change to
    /// any of them changes Esmeralda's genesis state roots.
    #[test]
    fn esmeralda_genesis_resource_hashes_are_pinned() {
        let hashes = [
            get_public_identity_resource(Network::Esmeralda),
            get_stealth_tari_resource(Network::Esmeralda),
            get_xtr_faucet_claim_resource(Network::Esmeralda),
            get_nft_faucet_resource(Network::Esmeralda),
        ]
        .map(|(_, resource)| {
            hash_substate(
                Network::Esmeralda,
                &resource.into(),
                SubstateVersion::ZERO,
                Epoch::zero(),
            )
            .to_string()
        });
        assert_eq!(hashes, [
            "8f1fd34d82253e61b05ee47f766f1f54edd836df10226b503733adc7855a37a1",
            "81c89bf3f153f96cdda650e45f376dbd69d5b4771313d46136d0c95ee9b0e063",
            "f5c69d18813fd4a0d75eddf6a5d6044d328d9de1c2e204b17ac0374fd3d31f14",
            "7d172c41821c747c92a76e38cb7d58c6623e16487c622586490669767bdaea12",
        ]);
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
