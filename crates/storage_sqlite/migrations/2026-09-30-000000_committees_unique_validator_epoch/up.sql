-- A validator holds one committee assignment per epoch. Where assignment ran more than once for an epoch, the
-- most recent row is the one that stands.
DELETE
FROM committees
WHERE id NOT IN (SELECT MAX(id) FROM committees GROUP BY validator_node_id, epoch);

DROP INDEX committees_validator_node_id_epoch_index;
CREATE UNIQUE INDEX committees_validator_node_id_epoch_index ON committees (validator_node_id, epoch);
