-- Admit "this substate does not exist" as a cache entry, recorded as a row with no version.
--
-- A `DoesNotExist` lookup is the one answer that cannot stop at the first good response: it is
-- settled by f+1 agreement, so it walks that many committee members every time. Caching it removes
-- the most expensive lookup the indexer makes.
--
-- Nullable rather than a sentinel version. A version is a u32 and the column is a signed integer, so
-- every value that could stand in for "no version" is one a real head could take.
--
-- The cache is rebuilt from the committee on demand, so this recreates the table rather than
-- migrating rows into it.
drop table substate_cache;

create table substate_cache
(
    substate_id      text    not null primary key,
    -- The substate's head version, or null when the substate does not exist.
    version          integer null,
    verified         boolean not null,
    substate_result  blob    not null,
    cached_at        bigint  not null
);

create index idx_substate_cache_evict on substate_cache (cached_at);
