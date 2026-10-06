//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use serde::Serialize;
use tari_bor::BorError;
use tari_consensus_types::BlockId;
use tari_engine_types::limits::MAX_CBOR_NESTING_DEPTH;
use tari_ootle_common_types::Epoch;
use tari_ootle_transaction::Transaction;

#[derive(Debug, Clone, Serialize)]
pub struct MissingTransactionsResponse {
    pub request_id: u32,
    pub epoch: Epoch,
    pub block_id: BlockId,
    pub transactions: Vec<EncodedTransaction>,
}

/// A transaction in its CBOR encoding, as carried by a [`MissingTransactionsResponse`].
///
/// A decoded transaction can take a large multiple of its encoded size, and a response arrives before it can be
/// matched to a request this node made. The transactions therefore stay encoded until consensus has matched the
/// response, so an unsolicited one costs no more than the bytes it arrived as.
#[derive(Debug, Clone, Serialize)]
pub struct EncodedTransaction(Vec<u8>);

impl EncodedTransaction {
    pub fn encode(transaction: &Transaction) -> Result<Self, BorError> {
        tari_bor::encode(transaction).map(Self)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn decode(&self) -> Result<Transaction, BorError> {
        tari_bor::decode_exact_with_max_depth(&self.0, MAX_CBOR_NESTING_DEPTH)
    }
}
