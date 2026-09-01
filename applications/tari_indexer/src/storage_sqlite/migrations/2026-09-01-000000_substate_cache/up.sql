-- Each substate's head version as this indexer last observed it, held here rather than in a side
-- store so that invalidation commits in the same transaction as the state sync watermark it is
-- derived from. An entry is served until a transition for its substate retires it, so the two must
-- never be visible out of step.
--
-- One row per substate, never per version. A live version is always the substate's head - upping a
-- substate downs its predecessor - so a cached head settles every lower version too: they are all
-- down, permanently, and are answered without consulting a validator or the sync watermark.
create table substate_cache
(
    substate_id      text    not null primary key,
    version          integer not null,
    verified         boolean not null,
    substate_result  blob    not null,
    cached_at        bigint  not null
);

create index idx_substate_cache_evict on substate_cache (cached_at);

-- Substates a synced transition has touched, retained only long enough to span a committee fetch.
-- A fetch that started before its shard reached `state_version` may have observed the substate as it
-- was beforehand, so a result landing afterwards must not be recorded as the head.
create table substate_cache_invalidations
(
    substate_id    text   not null primary key,
    state_version  bigint not null,
    invalidated_at bigint not null
);

create index idx_substate_cache_invalidations_expiry on substate_cache_invalidations (invalidated_at);
