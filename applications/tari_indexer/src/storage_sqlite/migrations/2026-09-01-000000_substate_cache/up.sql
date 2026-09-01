-- Substates fetched from validator committees to serve client reads, held here rather than in a
-- side store so that invalidation commits in the same transaction as the state sync watermark it is
-- derived from. An entry is served until a transition for its substate retracts it, so the two must
-- never be visible out of step.
--
-- A row is keyed by (substate_id, version) and its value never changes. `is_latest` is the only
-- claim that is ever retracted: it marks the row as the answer to an unversioned read.
create table substate_cache
(
    substate_id      text    not null,
    version          integer not null,
    is_latest        boolean not null,
    verified         boolean not null,
    substate_result  blob    not null,
    cached_at        bigint  not null,
    primary key (substate_id, version)
);

create index idx_substate_cache_latest on substate_cache (substate_id) where is_latest;
create index idx_substate_cache_evict on substate_cache (cached_at);

-- Substates a synced transition has touched, retained only long enough to span a committee fetch.
-- A fetch that started before its shard reached `state_version` may have observed the substate as it
-- was beforehand, so a result landing afterwards must not be recorded as the latest version.
create table substate_cache_invalidations
(
    substate_id    text   not null primary key,
    state_version  bigint not null,
    invalidated_at bigint not null
);

create index idx_substate_cache_invalidations_expiry on substate_cache_invalidations (invalidated_at);
