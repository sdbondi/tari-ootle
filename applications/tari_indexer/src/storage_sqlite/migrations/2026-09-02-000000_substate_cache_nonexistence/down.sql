drop table substate_cache;

create table substate_cache
(
    substate_id      text    not null primary key,
    version          integer not null,
    verified         boolean not null,
    substate_result  blob    not null,
    cached_at        bigint  not null
);

create index idx_substate_cache_evict on substate_cache (cached_at);
