//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use diesel::Insertable;
use tari_indexer_client::types::TransactionSource;
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
    pub fn new(transaction: &Transaction, source: TransactionSource) -> Result<Self, StorageError> {
        Ok(Self {
            transaction_id: serialize_hex(transaction.calculate_id()),
            body: serialize_json(transaction)?,
            // Until a receipt supplies a commit epoch, `max_epoch` is the retention key: past it the
            // transaction can no longer be sequenced, so it will never reach a terminal state.
            retention_epoch: transaction.max_epoch().as_u64() as i64,
            source: source.as_str(),
        })
    }
}
