//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use ootle_byte_type::FromByteType;
use ootle_network::Network;
use tari_bor::{Deserialize, Serialize};
use tari_engine_types::crypto::MAX_LAZY_BP_AGG_FACTORS;
use tari_ootle_address::OotleAddress;
use tari_ootle_wallet_crypto::{memo::Memo, pay_to::PayTo, stealth::condition_root};
use tari_template_lib::types::{Amount, ComponentAddress, NonFungibleAddress, ResourceAddress};

use crate::apis::{
    confidential_transfer::UtxoInputSelection,
    stealth_outputs::StealthOutputsApiError,
    stealth_transfer::{StealthOutputToCreate, StealthTransferApiError},
};

#[derive(Debug)]
pub struct StealthTransferParams {
    /// Parameters related to fee payment
    pub fee_params: TransferFeeParams,
    /// Strategy for input selection
    pub input_selection: UtxoInputSelection,
    pub outputs: Vec<TransferOutput>,
    pub badge_usage: BadgeUsage,
    /// Address of the resource to transfer
    pub resource_address: ResourceAddress,
    /// Fee to lock for the transaction
    pub max_fee: u64,
    /// Run as a dry run, no funds will be transferred if true
    pub is_dry_run: bool,
}

impl StealthTransferParams {
    pub fn validate(&self, network: Network) -> Result<(), StealthTransferApiError> {
        if self.outputs.is_empty() {
            return Err(StealthTransferApiError::InvalidParameter {
                param: "outputs",
                reason: "At least one output must be specified".to_string(),
            });
        }

        let blinded_count = self.outputs.iter().filter(|o| o.blinded_amount > 0).count();
        if blinded_count > MAX_LAZY_BP_AGG_FACTORS {
            return Err(StealthTransferApiError::InvalidParameter {
                param: "outputs",
                reason: format!(
                    "Number of outputs ({}) exceeds maximum allowed ({})",
                    blinded_count, MAX_LAZY_BP_AGG_FACTORS
                ),
            });
        }

        for output in &self.outputs {
            if output.revealed_amount.is_negative() {
                return Err(StealthTransferApiError::InvalidParameter {
                    param: "revealed_output_amount",
                    reason: "Revealed output amount must be non-negative".to_string(),
                });
            }

            if output.blinded_amount == 0 && output.revealed_amount.is_zero() {
                return Err(StealthTransferApiError::InvalidParameter {
                    param: "blinded_output_amount and revealed_output_amount",
                    reason: "At least one of the amounts must be greater than zero".to_string(),
                });
            }

            if output.address.network() != network {
                return Err(StealthTransferApiError::InvalidParameter {
                    param: "destination_address",
                    reason: format!(
                        "Destination address network ({}) does not match wallet network ({})",
                        output.address.network(),
                        network
                    ),
                });
            }

            output
                .address
                .validate()
                .map_err(|e| StealthTransferApiError::InvalidParameter {
                    param: "destination_address",
                    reason: format!("Invalid destination address: {}", e),
                })?;

            // A condition set that cannot form a tree (empty, or with duplicate leaves) is rejected here so the
            // caller is told which field is malformed, rather than failing deep in output construction after
            // inputs have been locked.
            if let PayTo::Conditions(conditions) = &output.pay_to {
                condition_root(conditions).map_err(|e| StealthTransferApiError::InvalidParameter {
                    param: "pay_to",
                    reason: e.to_string(),
                })?;
            }
        }

        Ok(())
    }

    pub fn total_output_amount(&self) -> Amount {
        self.outputs.iter().map(|o| o.total_output_amount()).sum()
    }

    pub fn total_revealed_output_amount(&self) -> Amount {
        self.outputs.iter().map(|o| o.revealed_amount).sum()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "wallet-types/"))]
pub struct TransferOutput {
    /// Destination address used to derive the UTXO encryption keys, owner signature and the account in which to
    /// deposit revealed funds
    pub address: OotleAddress,
    /// Amount to spend to a revealed output
    pub revealed_amount: Amount,
    /// Amount to spend to a blinded output
    pub blinded_amount: u64,
    /// Optional memo to include a memo in the output. This memo is encrypted and can only be read by the recipient.
    pub memo: Option<Memo>,
    pub pay_to: PayTo,
}

impl TransferOutput {
    pub fn total_output_amount(&self) -> Amount {
        self.revealed_amount + Amount::from(self.blinded_amount)
    }
}

impl<'a> TryFrom<&'a TransferOutput> for StealthOutputToCreate<'a> {
    type Error = StealthOutputsApiError;

    fn try_from(value: &'a TransferOutput) -> Result<Self, Self::Error> {
        Ok(Self {
            owner_address: value.address.try_from_byte_type().map_err(|e| {
                StealthOutputsApiError::InvalidParameter {
                    param: "destination_address",
                    reason: format!("Invalid destination address: {}", e),
                }
            })?,
            amount: value.blinded_amount,
            memo: value.memo.as_ref(),
            pay_to: value.pay_to.clone(),
        })
    }
}
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "wallet-types/"))]
pub enum BadgeUsage {
    /// Do not use a badge
    #[default]
    None,
    /// Use a resource as a badge
    Resource(ResourceAddress),
    /// Use a specific NFT as a badge
    NonFungible(NonFungibleAddress),
    /// Use a specified amount of resource as a badge
    AmountOfResource { resource: ResourceAddress, amount: Amount },
}

impl BadgeUsage {
    pub fn resource_address(&self) -> Option<&ResourceAddress> {
        match self {
            BadgeUsage::None => None,
            BadgeUsage::Resource(addr) => Some(addr),
            BadgeUsage::NonFungible(nft_addr) => Some(nft_addr.resource_address()),
            BadgeUsage::AmountOfResource { resource, .. } => Some(resource),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, BadgeUsage::None)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "wallet-types/"))]
pub struct TransferFeeParams {
    pub input_selection: UtxoInputSelection,
    pub pay_fee_with_swap: Option<PayFeeWithSwapParams>,
}

impl TransferFeeParams {
    pub fn new(input_selection: UtxoInputSelection) -> Self {
        Self {
            input_selection,
            pay_fee_with_swap: None,
        }
    }

    pub fn with_pay_fee_with_swap(mut self, params: PayFeeWithSwapParams) -> Self {
        self.pay_fee_with_swap = Some(params);
        self
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "wallet-types/"))]
pub struct PayFeeWithSwapParams {
    pub pool_address: ComponentAddress,
    pub input_resource: ResourceAddress,
    pub input_amount: Amount,
    pub min_xtr_output_amount: Amount,
}

#[cfg(test)]
mod tests {
    use ootle_byte_type::ToByteType;
    use tari_crypto::{keys::PublicKey as _, ristretto::RistrettoPublicKey};
    use tari_template_lib::types::{
        constants::STEALTH_TARI_RESOURCE_ADDRESS,
        stealth::{AtomicCondition, BuiltinPredicate, SpendCondition},
    };

    use super::*;

    const NETWORK: Network = Network::LocalNet;

    fn address() -> OotleAddress {
        let (_, view_only) = RistrettoPublicKey::random_keypair(&mut rand::rng());
        let (_, account) = RistrettoPublicKey::random_keypair(&mut rand::rng());
        OotleAddress::new(NETWORK, view_only.to_byte_type(), account.to_byte_type())
    }

    /// A single-leaf condition, distinct per `epoch`.
    fn a_condition(epoch: u64) -> SpendCondition {
        SpendCondition::all([AtomicCondition::Builtin(BuiltinPredicate::AfterEpoch(epoch))])
    }

    fn params_paying_to(pay_to: PayTo) -> StealthTransferParams {
        StealthTransferParams {
            fee_params: TransferFeeParams::new(UtxoInputSelection::PreferConfidential),
            input_selection: UtxoInputSelection::PreferConfidential,
            outputs: vec![TransferOutput {
                address: address(),
                revealed_amount: Amount::zero(),
                blinded_amount: 100,
                memo: None,
                pay_to,
            }],
            badge_usage: BadgeUsage::None,
            resource_address: STEALTH_TARI_RESOURCE_ADDRESS,
            max_fee: 1000,
            is_dry_run: false,
        }
    }

    fn assert_rejects_pay_to(pay_to: PayTo) {
        let err = params_paying_to(pay_to)
            .validate(NETWORK)
            .expect_err("a condition set that cannot form a tree must be rejected");
        assert!(
            matches!(err, StealthTransferApiError::InvalidParameter { param: "pay_to", .. }),
            "expected an InvalidParameter naming pay_to, got: {err}"
        );
    }

    #[test]
    fn rejects_an_empty_condition_set() {
        assert_rejects_pay_to(PayTo::Conditions(vec![]));
    }

    #[test]
    fn rejects_duplicate_condition_leaves() {
        let condition = a_condition(1);
        assert_rejects_pay_to(PayTo::Conditions(vec![condition.clone(), condition]));
    }

    #[test]
    fn accepts_a_well_formed_condition_set() {
        params_paying_to(PayTo::Conditions(vec![a_condition(1), a_condition(2)]))
            .validate(NETWORK)
            .expect("distinct condition leaves form a tree");
    }
}
