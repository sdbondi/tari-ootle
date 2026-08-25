drop index transactions_source_id_idx;
alter table transactions
    drop column source;
