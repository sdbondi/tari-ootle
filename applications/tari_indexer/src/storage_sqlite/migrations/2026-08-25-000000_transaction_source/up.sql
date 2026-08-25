-- Where this indexer first learned of a transaction: 'local' for a direct submission through its
-- API, 'gossip' for one observed on the network-wide transaction topic. Rows written before this
-- column existed were all direct submissions, hence the default.
alter table transactions
    add column source text not null default 'local';

-- `list_recent_transactions` pages backwards by id. Filtering by source without this index walks
-- back through the whole gossip stream to collect a page of local rows.
create index transactions_source_id_idx on transactions (source, id);

-- Rows predating the retention column carry retention_epoch 0. Retention is now on by default, and
-- the pruner's first pass runs seconds after startup, so left at 0 every one of them would be
-- deleted at once on upgrade rather than aged out on the schedule the setting describes. Give them
-- a real terminal epoch: the commit epoch where a receipt has been synced, and otherwise the
-- highest epoch this node has seen, so they age out from here rather than from the beginning.
update transactions
set retention_epoch = coalesce(
    (select json_extract(r.data, '$.epoch')
     from transaction_receipts r
     where r.address = transactions.transaction_id),
    (select max(epoch) from substate_transitions),
    0
)
where retention_epoch = 0;
