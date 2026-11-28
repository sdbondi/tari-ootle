//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use log::*;
use tari_ootle_common_types::is_fee_pool_address_of_claim_key;
use tari_ootle_transaction::Transaction;

use crate::{TransactionValidationError, Validator};

const LOG_TARGET: &str = "tari::ootle::mempool::validators::fee_pool_input_signer";

/// Rejects a transaction that declares a validator fee pool as an input without a signature from the pool's claim
/// key.
///
/// The only use a transaction has for a fee pool is to claim from it, and a claim is authorized by the claim key
/// alone. A multishard transaction locks its inputs when it is prepared and executes once every shard group has
/// pledged, so a fee pool input signed by anyone else holds the pool locked against its owner's claims until the
/// claim fails its ownership check.
///
/// A fee pool's address carries its claim key, so the address and the signers decide this with no reference to any
/// node's view of state: refusing it at ingress cannot false-reject. The signers counted are those the engine
/// authorizes with, the seal signer when the transaction authorizes it and every authorization signer.
#[derive(Debug, Clone, Default)]
pub struct FeePoolInputSignerValidator;

impl FeePoolInputSignerValidator {
    pub fn new() -> Self {
        Self
    }
}

impl Validator<Transaction> for FeePoolInputSignerValidator {
    type Context = ();
    type Error = TransactionValidationError;

    fn validate(&self, _context: &(), transaction: &Transaction) -> Result<(), Self::Error> {
        let seal_signer = transaction
            .is_seal_signer_authorized()
            .then_some(transaction.seal_signature().public_key());
        let unsigned_pool = transaction
            .all_inputs_iter()
            .filter_map(|input| input.substate_id().as_validator_fee_pool_address())
            .find(|address| {
                !seal_signer
                    .into_iter()
                    .chain(transaction.signatures().iter().map(|signature| signature.public_key()))
                    .any(|signer| is_fee_pool_address_of_claim_key(address, signer))
            });

        if let Some(address) = unsigned_pool {
            warn!(target: LOG_TARGET, "FeePoolInputSignerValidator - FAIL: fee pool {address} declared as an input without its claim key's signature");
            return Err(TransactionValidationError::FeePoolInputNotSignedByClaimKey {
                transaction_id: transaction.calculate_id(),
                address,
            });
        }

        debug!(target: LOG_TARGET, "FeePoolInputSignerValidator - OK");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tari_common_types::types::PrivateKey;
    use tari_crypto::{keys::PublicKey as _, ristretto::RistrettoPublicKey, tari_utilities::ByteArray as _};
    use tari_engine_types::substate::SubstateId;
    use tari_ootle_common_types::{Epoch, NumPreshards, derive_fee_pool_address, shard::Shard};
    use tari_ootle_transaction::{MainSigner, TransactionBuilder};
    use tari_template_lib::types::{
        Amount,
        ComponentAddress,
        ValidatorFeePoolAddress,
        crypto::RistrettoPublicKeyBytes,
    };

    use super::*;

    fn secret(n: u64) -> PrivateKey {
        PrivateKey::from(n)
    }

    fn public_key(secret: &PrivateKey) -> RistrettoPublicKeyBytes {
        RistrettoPublicKeyBytes::from_bytes(RistrettoPublicKey::from_secret_key(secret).as_bytes())
            .expect("a Ristretto public key is 32 bytes")
    }

    fn pool_of(claim_secret: &PrivateKey, shard: u32) -> ValidatorFeePoolAddress {
        derive_fee_pool_address(&public_key(claim_secret), NumPreshards::P256, Shard::from(shard)).unwrap()
    }

    fn builder() -> TransactionBuilder {
        Transaction::builder_localnet(Epoch(1))
            .pay_fee_from_component(ComponentAddress::from_array([1u8; 32]), Amount::new(1000))
    }

    /// A claim as a wallet builds it: the instruction and the pool it withdraws from declared as an input.
    fn claim(builder: TransactionBuilder, pool: ValidatorFeePoolAddress) -> TransactionBuilder {
        builder
            .claim_validator_fees(pool)
            .add_input(SubstateId::ValidatorFeePool(pool))
    }

    fn validate(transaction: &Transaction) -> Result<(), TransactionValidationError> {
        FeePoolInputSignerValidator::new().validate(&(), transaction)
    }

    fn assert_refused(transaction: &Transaction, pool: ValidatorFeePoolAddress) {
        match validate(transaction) {
            Err(TransactionValidationError::FeePoolInputNotSignedByClaimKey { address, .. }) => {
                assert_eq!(address, pool)
            },
            other => panic!("expected the fee pool input to be refused, got {other:?}"),
        }
    }

    #[test]
    fn it_accepts_a_claim_sealed_by_the_claim_key() {
        let claim_key = secret(1);
        let transaction = claim(builder(), pool_of(&claim_key, 7)).build_and_seal(&claim_key);
        validate(&transaction).unwrap();
    }

    #[test]
    fn it_accepts_claims_from_the_pools_of_one_key_in_several_shards() {
        let claim_key = secret(1);
        let transaction =
            claim(claim(builder(), pool_of(&claim_key, 1)), pool_of(&claim_key, 256)).build_and_seal(&claim_key);
        validate(&transaction).unwrap();
    }

    #[test]
    fn it_accepts_a_claim_authorized_by_the_claim_key_and_sealed_by_another() {
        let claim_key = secret(1);
        let sealer = secret(2);
        let transaction = claim(builder(), pool_of(&claim_key, 7))
            .authorize(&public_key(&sealer), [&claim_key])
            .seal(&sealer);
        validate(&transaction).unwrap();
    }

    #[test]
    fn it_refuses_a_claim_from_a_pool_whose_claim_key_did_not_sign() {
        let pool = pool_of(&secret(1), 7);
        let transaction = claim(builder(), pool).build_and_seal(&secret(2));
        assert_refused(&transaction, pool);
    }

    #[test]
    fn it_refuses_a_pool_input_with_no_claim_instruction_whose_claim_key_did_not_sign() {
        let pool = pool_of(&secret(1), 7);
        let transaction = builder()
            .add_input(SubstateId::ValidatorFeePool(pool))
            .build_and_seal(&secret(2));
        assert_refused(&transaction, pool);
    }

    /// A pool that is not an input is never locked, so the claim only fails when it executes.
    #[test]
    fn it_accepts_a_claim_instruction_whose_pool_is_not_declared_as_an_input() {
        let transaction = builder()
            .claim_validator_fees(pool_of(&secret(1), 7))
            .build_and_seal(&secret(2));
        validate(&transaction).unwrap();
    }

    #[test]
    fn it_refuses_the_pool_whose_claim_key_did_not_sign_among_pools_that_are_signed_for() {
        let claim_key = secret(1);
        let other_pool = pool_of(&secret(3), 7);
        let transaction = claim(claim(builder(), pool_of(&claim_key, 7)), other_pool).build_and_seal(&claim_key);
        assert_refused(&transaction, other_pool);
    }

    /// The engine issues no badge for a seal signer the transaction does not authorize, so its seal cannot claim.
    #[test]
    fn it_refuses_a_claim_sealed_by_a_claim_key_that_the_transaction_does_not_authorize() {
        let claim_key = secret(1);
        let authorizer = secret(2);
        let pool = pool_of(&claim_key, 7);
        let transaction = claim(builder(), pool)
            .with_main_signer(MainSigner::Authorizer(public_key(&authorizer)))
            .authorize(&public_key(&claim_key), [&authorizer])
            .seal(&claim_key);
        assert_refused(&transaction, pool);
    }

    #[test]
    fn it_accepts_a_transaction_with_no_fee_pool_input() {
        let transaction = builder()
            .add_input(SubstateId::Component(ComponentAddress::from_array([2u8; 32])))
            .build_and_seal(&secret(1));
        validate(&transaction).unwrap();
    }
}
