//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

mod transaction;

use tari_engine_types::substate::SubstateId;
use tari_ootle_transaction::{Blobs, Instruction, TransactionId, TransactionWeight};
use tari_template_lib::types::{Hash32, crypto::RistrettoPublicKeyBytes};

pub struct Instructions {
    pub fee: Vec<Instruction>,
    pub main: Vec<Instruction>,
    /// Blob payloads referenced by `BlobIndex` from instructions and args. Resolved by the
    /// processor before executing each instruction.
    pub blobs: Blobs,
}

pub trait Executable {
    fn to_id(&self) -> TransactionId;

    /// Commitment to everything the signers authorized, recorded in the transaction receipt so that
    /// a holder of the transaction can prove it produced the receipt without revealing the signers.
    fn calculate_intent_commitment(&self) -> Hash32;

    fn all_inputs_iter(&self) -> impl Iterator<Item = SubstateId> + '_;

    /// Returns the main signer of the executable, if any.
    fn main_signer(&self) -> Option<RistrettoPublicKeyBytes> {
        self.signers_iter().next().copied()
    }
    /// Returns an iterator over all signers of the executable including the main signer.
    fn signers_iter(&self) -> impl Iterator<Item = &RistrettoPublicKeyBytes>;

    fn into_instructions(self) -> Instructions;
}

pub trait WeightedExecutable: Executable {
    fn calculate_weight(&self) -> TransactionWeight;
}
