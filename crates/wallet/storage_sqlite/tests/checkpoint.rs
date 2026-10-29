// Copyright 2026 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::fs;

use tari_ootle_wallet_storage_sqlite::SqliteWalletStore;

#[test]
fn a_checkpoint_empties_the_write_ahead_log_into_the_database() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("wallet.sqlite");
    let store = SqliteWalletStore::try_open(&path).unwrap();
    store.run_migrations().unwrap();
    let wal = temp.path().join("wallet.sqlite-wal");
    let database_len = fs::metadata(&path).unwrap().len();
    assert!(fs::metadata(&wal).unwrap().len() > 0, "the migrations are in the log");

    store.checkpoint().unwrap();

    assert_eq!(fs::metadata(&wal).unwrap().len(), 0);
    assert!(fs::metadata(&path).unwrap().len() > database_len);
}
