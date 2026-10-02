-- The proof each cached substate version was verified with, so that a request for it need not ask the
-- committee again. A proof is served only while its version is the head `substate_cache` holds, and
-- the pruner removes those that no longer match a cached head.
create table substate_cache_proofs
(
    substate_id  text   not null,
    version      bigint not null,
    -- CBOR-encoded SubstateValueProof
    value_proof  blob   not null,
    -- CBOR-encoded CommittedBlockProof anchoring `value_proof`
    commit_proof blob   not null,
    -- The epoch the substate value hash was computed in
    proof_epoch  bigint not null,
    primary key (substate_id, version)
);
