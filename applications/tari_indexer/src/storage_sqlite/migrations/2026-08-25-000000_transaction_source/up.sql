-- Where this indexer first learned of a transaction: 'local' for a direct submission through its
-- API, 'gossip' for one observed on the network-wide transaction topic. Rows written before this
-- column existed were all direct submissions, hence the default.
alter table transactions
    add column source text not null default 'local';

-- `list_recent_transactions` pages backwards by id. Filtering by source without this index walks
-- back through the whole gossip stream to collect a page of local rows.
create index transactions_source_id_idx on transactions (source, id);
