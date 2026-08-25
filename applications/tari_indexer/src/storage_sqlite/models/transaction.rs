//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use diesel::Insertable;
use tari_indexer_client::types::TransactionSource;
use tari_ootle_common_types::Epoch;
use tari_ootle_storage::StorageError;
use tari_ootle_transaction::Transaction;

use crate::storage_sqlite::{
    schema::transactions,
    serialization::{serialize_hex, serialize_json},
};

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = transactions)]
pub(crate) struct NewTransaction {
    pub transaction_id: String,
    pub body: String,
    pub retention_epoch: i64,
    pub source: &'static str,
}

impl NewTransaction {
    /// `retention_ceiling` caps the recorded retention epoch. Until a receipt supplies a commit
    /// epoch, `max_epoch` is the retention key: past it the transaction can no longer be sequenced,
    /// so it will never reach a terminal state. `max_epoch` is chosen by whoever authored the
    /// transaction, though, so left alone it lets one message claim a row that no retention window
    /// ever reaches. The ceiling is the last epoch a transaction admitted now could still be
    /// sequenced in, which is the furthest out a retention key can honestly sit.
    pub fn new(
        transaction: &Transaction,
        source: TransactionSource,
        retention_ceiling: Epoch,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            transaction_id: serialize_hex(transaction.calculate_id()),
            body: serialize_json(transaction)?,
            retention_epoch: encode_retention_epoch(transaction.max_epoch().min(retention_ceiling)),
            source: source.as_str(),
        })
    }
}

/// Encodes an epoch for the `retention_epoch` column.
///
/// Other `u64` columns are stored by reinterpreting the bits as `i64`, which round-trips exactly.
/// This column is different: it is filtered and ordered by SQL, so its encoding has to preserve
/// order as well as value. A bit-reinterpreted epoch above `i64::MAX` reads as negative and sorts
/// below every real epoch, which would delete the row on the pruner's next pass rather than keep it.
/// Saturating instead keeps the column monotonic in the epoch. With the ceiling applied at every
/// call site the saturation point is unreachable in practice.
pub(crate) fn encode_retention_epoch(epoch: Epoch) -> i64 {
    i64::try_from(epoch.as_u64()).unwrap_or(i64::MAX)
}
