//    Copyright 2026 The Tari Project
//    SPDX-License-Identifier: BSD-3-Clause

//! Static pricing for a stealth transfer that sources its own fee.
//!
//! A wallet cannot choose a transfer's shape without knowing what the shape costs: input selection
//! targets `amount + max_fee`, so the fee decides which UTXOs are spent and whether any change is
//! left over, and a change output is another stealth output with its own verification and storage.
//! Learning the cost by dry-running resolves that fixed point over the network, a round trip per
//! iteration. [`MergedStealthTransferShape::estimate_fee`] resolves it locally instead: it is pure
//! arithmetic over the shape, so a builder can settle on a shape before it generates a single proof.
//!
//! What it prices is an upper bound, not a replica of the engine's accounting. The bound is what
//! matters to a builder: settling on a figure at or above the real charge means the shape it selects
//! for is the shape the charge is taken over, so a later exact figure changes the fee without
//! changing the shape. An under-estimate would instead select too little and force the shape to move
//! under it. `stealth_fee_estimate.rs` in `tari_engine`'s test suite holds the bound to real
//! executions across a matrix of shapes — both that it never comes in under, and that it stays
//! close.

use tari_template_lib::types::{
    EncryptedData,
    ResourceAddress,
    UtxoAddress,
    UtxoId,
    crypto::{RistrettoPublicKeyBytes, UtxoTag},
    stealth::SpendAuthorization,
};

use crate::{
    Epoch,
    UtxoOutput,
    crypto::{ElgamalVerifiableBalanceBytes, OutputBody},
    fees::FeeRates,
    stealth::transfer_native_points_for_shape,
    substate::{SubstateId, SubstateValue},
    transaction_receipt::TransactionReceipt,
    utxo::Utxo,
};

/// The shape of a stealth transfer that both moves funds and reveals the fee that pays for it,
/// creating only stealth outputs.
///
/// This is the transaction a wallet builds for the common send: one stealth transfer statement in
/// the fee intent, its revealed remainder paid straight to `pay_fee`. Nothing in it reads the fee
/// amount back — the amount lives inside the statement, which weighs by input count rather than by
/// the amounts it carries — so the cost is a function of this shape alone and
/// [`Self::estimate_fee`] can settle it without executing anything.
///
/// A transfer that pays a recipient in revealed funds, sources its fee from a separate statement,
/// or calls a template is a different shape with charges this does not model, and must not be
/// priced with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergedStealthTransferShape {
    /// Stealth UTXOs the statement spends. A spent UTXO leaves state altogether rather than being
    /// rewritten, so it costs verification but no storage.
    pub num_inputs: usize,
    /// Stealth UTXOs the statement creates, the change output included.
    pub num_outputs: usize,
    /// Bytes the outputs' encrypted data carries beyond [`EncryptedData::min_size`], summed across
    /// them. An output with a memo carries it here; one without adds nothing.
    pub extra_encrypted_data_bytes: usize,
    /// Whether the resource carries a view key, which adds a viewable-balance proof to each output
    /// — verification to pay for, and bytes to store.
    pub has_view_key: bool,
    /// The weight of the transaction this shape builds into. Weight is defined by
    /// `tari_ootle_transaction`, downstream of this crate, so the builder supplies it.
    pub transaction_weight: u64,
    /// Engine host calls the instruction sequence makes, each charged at the flat per-call rate.
    pub num_runtime_calls: u64,
}

impl MergedStealthTransferShape {
    /// An upper bound, in microtari, on what the engine charges a transaction of this shape.
    ///
    /// Sums the charges the engine takes over a shape — transaction weight, host calls, native
    /// verification, persisted storage and substate creation — and applies the exhaust burn over
    /// their total, the same order [`crate::fees::FeeSource`] is accumulated in.
    pub fn estimate_fee(&self, rates: &FeeRates) -> u64 {
        let weight_cost = self
            .transaction_weight
            .saturating_mul(rates.per_transaction_weight_cost());
        let runtime_call_cost = self.num_runtime_calls.saturating_mul(rates.per_module_call_cost());
        let native_cost = rates.execution_cost(transfer_native_points_for_shape(
            self.num_inputs,
            self.num_outputs,
            self.has_view_key,
        ));
        let storage_cost = rates.storage_cost(self.persisted_bytes_upper_bound() as u64);
        // The receipt occupies a created slot of its own, on top of one per stealth output. Spent
        // inputs already exist, so they allocate nothing.
        let create_cost = (self.num_outputs as u64)
            .saturating_add(1)
            .saturating_mul(rates.per_substate_create_cost());

        let base = weight_cost
            .saturating_add(runtime_call_cost)
            .saturating_add(native_cost)
            .saturating_add(storage_cost)
            .saturating_add(create_cost);

        base.saturating_add(rates.exhaust_burn(base))
    }

    /// An upper bound on the bytes of permanent state this shape persists: the UTXOs it creates and
    /// the transaction receipt that records the whole thing.
    ///
    /// The UTXOs it spends contribute nothing. Spending downs a UTXO — it leaves state rather than
    /// being rewritten as spent — so it is neither byte-counted nor listed in the receipt's diff
    /// summary.
    fn persisted_bytes_upper_bound(&self) -> usize {
        self.num_outputs
            .saturating_mul(created_utxo_bytes_upper_bound(self.has_view_key))
            .saturating_add(self.extra_encrypted_data_bytes)
            .saturating_add(self.receipt_bytes_upper_bound())
    }

    /// An upper bound on the receipt this shape finalizes into. Each created UTXO takes one
    /// diff-summary entry; the receipt is absent from its own summary, and the transfer emits no
    /// events and withdraws no validator fees.
    fn receipt_bytes_upper_bound(&self) -> usize {
        let upped = vec![SubstateId::Utxo(widest_utxo_address()); self.num_outputs];
        // The epoch is measured at its widest rather than taken from the caller: a receipt priced
        // for one epoch would otherwise come in under a transaction that lands in a wider one.
        TransactionReceipt::encoded_size_upper_bound(&[], &[], upped.iter(), Epoch(u64::MAX))
    }
}

/// The encoded size of a newly created stealth UTXO carrying the minimum encrypted-data payload.
/// Amount-like fields are measured at full varint width and the authorization at its widest
/// variant, so an output of any authorization comes in under this.
fn created_utxo_bytes_upper_bound(has_view_key: bool) -> usize {
    let widest_key = RistrettoPublicKeyBytes::zero();
    let utxo = Utxo::new(UtxoOutput {
        output: OutputBody {
            public_nonce: widest_key,
            encrypted_data: EncryptedData::try_from(vec![0u8; EncryptedData::min_size()])
                .expect("min_size is a valid encrypted data length"),
            minimum_value_promise: u64::MAX,
            viewable_balance: has_view_key.then(|| ElgamalVerifiableBalanceBytes {
                encrypted: widest_key,
                public_nonce: widest_key,
            }),
        },
        auth: SpendAuthorization::KeyAndScript {
            spend_key: widest_key,
            condition_root: [0xff; 32].into(),
        },
        tag: UtxoTag::new(u32::MAX),
    });
    encoded_len(&SubstateValue::Utxo(utxo))
}

/// A UTXO address at the width every UTXO address takes — its parts are all fixed-size arrays, so
/// the content is immaterial and only the shape of the encoding matters.
fn widest_utxo_address() -> UtxoAddress {
    UtxoAddress::new(
        ResourceAddress::new([0xff; 32].into()),
        UtxoId::from_array([0xff; UtxoId::LENGTH]),
    )
}

/// The bytes a value occupies in persisted state, measured the way the fee module tallies storage.
fn encoded_len<T: minicbor::Encode<()>>(value: &T) -> usize {
    tari_bor::encoded_len_via_writer(value).expect("encoding a canonical value into a byte counter cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fees::ExhaustBurnRate, limits::NativeExecutionPoints as P};

    /// Priced like the shipped tables, so the terms are legible in microtari.
    fn rates(burn_bps: u16) -> FeeRates {
        FeeRates::new(1, 1, 1, 25, 1, 1, 1000, ExhaustBurnRate::new(burn_bps))
    }

    fn shape(num_inputs: usize, num_outputs: usize) -> MergedStealthTransferShape {
        MergedStealthTransferShape {
            num_inputs,
            num_outputs,
            extra_encrypted_data_bytes: 0,
            has_view_key: false,
            transaction_weight: 150,
            num_runtime_calls: 3,
        }
    }

    #[test]
    fn an_output_costs_its_verification_its_storage_and_its_slot() {
        let one = shape(1, 1).estimate_fee(&rates(0));
        let two = shape(1, 2).estimate_fee(&rates(0));

        let native = P::PER_OUTPUT / 1000;
        let slot = 25;
        assert!(
            two - one > native + slot,
            "an added output must cost its verification, its slot and the bytes of both the UTXO and its receipt entry"
        );
    }

    /// An input aggregates a commitment and then leaves state; an output verifies a range proof and
    /// occupies a new slot for good. Nothing about the estimate should suggest otherwise.
    #[test]
    fn an_input_costs_only_its_verification() {
        let extra_input = shape(2, 1).estimate_fee(&rates(0)) - shape(1, 1).estimate_fee(&rates(0));
        assert_eq!(extra_input, P::PER_INPUT / 1000);

        let extra_output = shape(1, 2).estimate_fee(&rates(0)) - shape(1, 1).estimate_fee(&rates(0));
        assert!(extra_output > extra_input);
    }

    #[test]
    fn a_view_key_adds_verification_and_storage_per_output() {
        let plain = shape(1, 2);
        let viewable = MergedStealthTransferShape {
            has_view_key: true,
            ..plain
        };
        let surcharge = viewable.estimate_fee(&rates(0)) - plain.estimate_fee(&rates(0));
        // Two outputs' ElGamal proofs, plus the bytes their viewable balances add to each UTXO.
        assert!(surcharge > 2 * P::PER_OUTPUT_VIEWABLE_SURCHARGE / 1000);
    }

    #[test]
    fn a_memo_is_paid_for_by_the_byte() {
        let plain = shape(1, 2);
        let with_memo = MergedStealthTransferShape {
            extra_encrypted_data_bytes: 100,
            ..plain
        };
        assert_eq!(with_memo.estimate_fee(&rates(0)) - plain.estimate_fee(&rates(0)), 100);
    }

    #[test]
    fn the_burn_is_taken_over_every_other_charge() {
        let base = shape(1, 2).estimate_fee(&rates(0));
        assert_eq!(shape(1, 2).estimate_fee(&rates(10_000)), base * 2);
        assert_eq!(shape(1, 2).estimate_fee(&rates(5_000)), base + base / 2);
    }

    /// The estimate has to move with the shape monotonically, or a settling loop that raises its fee
    /// could select a wider shape that prices lower and oscillate.
    #[test]
    fn a_larger_shape_never_prices_lower() {
        let mut previous = 0;
        for num_outputs in 1..=8 {
            for num_inputs in 0..=8 {
                let estimate = shape(num_inputs, num_outputs).estimate_fee(&rates(500));
                assert!(estimate > 0);
                if num_inputs == 0 {
                    assert!(estimate > previous, "adding an output must not price lower");
                }
                previous = previous.max(estimate);
            }
        }
    }
}
