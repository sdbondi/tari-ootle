//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Version 1 stores the substate locks a block granted as one record per block, in place of version 0's record per
//! lock and its three indexes. This migration moves every version 0 lock into its block's record, in grant order, and
//! deletes the version 0 tables.

use std::collections::HashMap;

use log::*;
use tari_consensus_types::BlockId;
use tari_engine_types::substate::SubstateId;
use tari_ootle_common_types::{Epoch, NodeAddressable, NodeHeight};
use tari_ootle_storage::{Ordering, consensus_models::SubstateLock};
use tari_state_store_rocksdb::{
    cf_api::CfContext,
    column_families::substate_locks::legacy::{BlockIdIndex, ChainOrderIndex, SubstateIdIndex, SubstateLockModel},
    error::RocksDbStorageError,
    traits::{Cf, RocksReader, RocksWriter},
    writer::RocksDbStateStoreWriteTransaction,
};

const LOG_TARGET: &str = "tari::validator_node::migrations::v1";

pub fn migrate<TAddr: NodeAddressable + 'static>(
    tx: &mut RocksDbStateStoreWriteTransaction<'_, TAddr>,
) -> anyhow::Result<()> {
    const OPERATION: &str = "migrate_v1";

    let mut grants_by_block = HashMap::<(BlockId, Epoch, NodeHeight), Vec<(SubstateId, u32, SubstateLock)>>::new();
    {
        let db = tx.db();
        for result in db.cf(SubstateLockModel)?.iterator(Ordering::Ascending, OPERATION) {
            let (key, lock) = result?;
            grants_by_block
                .entry((key.block_id, key.block_epoch, key.block_height))
                .or_default()
                .push((key.substate_id, key.grant_seq, lock));
        }
        delete_all(&db.cf(SubstateLockModel)?)?;
        delete_all(&db.cf(BlockIdIndex)?)?;
        delete_all(&db.cf(SubstateIdIndex)?)?;
        delete_all(&db.cf(ChainOrderIndex)?)?;
    }

    let num_blocks = grants_by_block.len();
    for ((block_id, block_epoch, block_height), mut grants) in grants_by_block {
        grants.sort_by(|(a_id, a_seq, _), (b_id, b_seq, _)| (a_id, a_seq).cmp(&(b_id, b_seq)));
        let mut locks = Vec::<(SubstateId, Vec<SubstateLock>)>::new();
        for (substate_id, _, lock) in grants {
            match locks.last_mut() {
                Some((last_id, substate_locks)) if *last_id == substate_id => substate_locks.push(lock),
                _ => locks.push((substate_id, vec![lock])),
            }
        }
        tx.substate_locks_insert_for_block(
            &block_id,
            block_epoch,
            block_height,
            locks.iter().map(|(id, locks)| (id, locks)),
        )?;
    }

    info!(
        target: LOG_TARGET,
        "Moved the substate locks of {num_blocks} block(s) into per-block lock sets"
    );
    Ok(())
}

fn delete_all<CF: Cf, DB: RocksReader + RocksWriter>(cf: &CfContext<'_, DB, CF>) -> Result<(), RocksDbStorageError> {
    const OPERATION: &str = "migrate_v1::delete_all";
    let keys = cf
        .key_iterator(Ordering::Ascending, OPERATION)
        .collect::<Result<Vec<_>, _>>()?;
    for key in keys {
        cf.delete(&key, OPERATION)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tari_ootle_common_types::{NumPreshards, SubstateLockType, SubstateVersion};
    use tari_ootle_storage::{StateStore, StorageError, consensus_models::Block};
    use tari_ootle_transaction::{Network, TransactionId};
    use tari_state_store_rocksdb::{
        DatabaseOptions,
        RocksDbStateStore,
        column_families::substate_locks::{BlockLockSetCf, legacy::SubstateLockKey},
    };

    use super::*;

    /// Every version 0 lock lands in its block's lock set in grant order, whatever order its transaction id sorts in,
    /// and none of the version 0 tables survive. The block's position comes from the version 0 keys, so the block
    /// record is never read.
    #[test]
    fn version_0_locks_are_moved_into_block_lock_sets() {
        const OPERATION: &str = "test";
        let tmp = tempfile::tempdir().unwrap();
        let db = RocksDbStateStore::<String>::open(tmp.path().join("rocksdb"), DatabaseOptions::default()).unwrap();
        let block = Block::zero_block(Network::LocalNet, NumPreshards::P256);
        let substate_id = SubstateId::TransactionReceipt(TransactionId::new([9; 32]).into());
        let mut ids = [TransactionId::new([1; 32]), TransactionId::new([2; 32])];
        ids.sort();
        let [lower_id, higher_id] = ids;
        // Granted in the opposite order to transaction-id order, with a gap left by a released lock between them.
        let granted = [
            (higher_id, 0, SubstateLockType::Read, SubstateVersion::new(0)),
            (lower_id, 2, SubstateLockType::Output, SubstateVersion::new(1)),
        ];

        db.with_write_tx(|tx| {
            let db = tx.db();
            for (transaction_id, grant_seq, lock_type, version) in granted {
                let key = SubstateLockKey {
                    block_id: *block.id(),
                    block_epoch: block.epoch(),
                    block_height: block.height(),
                    substate_id: substate_id.clone(),
                    transaction_id,
                    grant_seq,
                };
                let lock = SubstateLock::new(transaction_id, version, lock_type, true);
                db.cf(SubstateLockModel)?.put(&key, &lock, OPERATION)?;
                db.cf(BlockIdIndex)?.put(&key, &(), OPERATION)?;
                db.cf(SubstateIdIndex)?.put(&key, &lock_type, OPERATION)?;
                db.cf(ChainOrderIndex)?.put(
                    &(
                        substate_id.clone(),
                        block.epoch(),
                        block.height(),
                        *block.id(),
                        grant_seq,
                    ),
                    &transaction_id,
                    OPERATION,
                )?;
            }
            Ok::<_, StorageError>(())
        })
        .unwrap();

        db.with_write_tx(migrate).unwrap();

        let tx = db.create_read_tx().unwrap();
        let db_ctx = tx.db();
        let lock_set = db_ctx.cf(BlockLockSetCf).unwrap().get(block.id(), OPERATION).unwrap();
        assert_eq!(lock_set.substates.len(), 1);
        assert_eq!(lock_set.substates[0].substate_id, substate_id);
        let locks = lock_set.substates[0]
            .locks
            .iter()
            .map(|lock| (*lock.transaction_id(), lock.lock_type()))
            .collect::<Vec<_>>();
        assert_eq!(locks, vec![
            (higher_id, SubstateLockType::Read),
            (lower_id, SubstateLockType::Output),
        ]);

        assert_eq!(db_ctx.cf(SubstateLockModel).unwrap().count(OPERATION).unwrap(), 0);
        assert_eq!(db_ctx.cf(BlockIdIndex).unwrap().count(OPERATION).unwrap(), 0);
        assert_eq!(db_ctx.cf(SubstateIdIndex).unwrap().count(OPERATION).unwrap(), 0);
        assert_eq!(db_ctx.cf(ChainOrderIndex).unwrap().count(OPERATION).unwrap(), 0);
    }
}
