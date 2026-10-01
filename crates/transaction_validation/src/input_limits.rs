// Copyright 2026 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use log::warn;
use tari_ootle_transaction::{MAX_TRANSACTION_INPUTS, Transaction};

use crate::{TransactionValidationError, Validator};

const LOG_TARGET: &str = "tari::ootle::mempool::validators::input_limits";

/// Rejects transactions declaring more than [`MAX_TRANSACTION_INPUTS`] input substates.
#[derive(Debug, Clone, Default)]
pub struct InputLimitValidator;

impl InputLimitValidator {
    pub fn new() -> Self {
        Self
    }
}

impl Validator<Transaction> for InputLimitValidator {
    type Context = ();
    type Error = TransactionValidationError;

    fn validate(&self, _context: &(), transaction: &Transaction) -> Result<(), Self::Error> {
        let count = transaction.inputs().len();
        if count > MAX_TRANSACTION_INPUTS {
            let transaction_id = transaction.calculate_id();
            warn!(
                target: LOG_TARGET,
                "InputLimitValidator - FAIL: {transaction_id} declares {count} inputs, maximum is {MAX_TRANSACTION_INPUTS}"
            );
            return Err(TransactionValidationError::TooManyInputs {
                transaction_id,
                max: MAX_TRANSACTION_INPUTS,
                actual: count,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tari_common_types::types::PrivateKey;
    use tari_ootle_common_types::{Epoch, InputDeclaration};
    use tari_template_lib::types::{Amount, ComponentAddress};

    use super::*;

    fn tx_with_inputs(num_inputs: usize) -> Transaction {
        Transaction::builder_localnet(Epoch(10))
            .pay_fee_from_component(ComponentAddress::from_array([0u8; 32]), Amount::new(1000))
            .with_inputs((0..num_inputs).map(|i| {
                let mut address = [0u8; 32];
                address[..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
                InputDeclaration::read(ComponentAddress::from_array(address))
            }))
            .build_and_seal(&PrivateKey::from(1u64))
    }

    #[test]
    fn accepts_inputs_at_the_limit() {
        InputLimitValidator::new()
            .validate(&(), &tx_with_inputs(MAX_TRANSACTION_INPUTS))
            .unwrap();
    }

    #[test]
    fn rejects_inputs_over_the_limit() {
        let over_limit = MAX_TRANSACTION_INPUTS + 1;
        let err = InputLimitValidator::new()
            .validate(&(), &tx_with_inputs(over_limit))
            .unwrap_err();
        assert!(matches!(
            err,
            TransactionValidationError::TooManyInputs { max, actual, .. }
            if max == MAX_TRANSACTION_INPUTS && actual == over_limit
        ));
    }
}
