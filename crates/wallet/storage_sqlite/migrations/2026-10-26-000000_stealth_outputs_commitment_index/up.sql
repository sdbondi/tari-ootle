-- Looks up a transaction's stealth inputs and outputs by commitment alone: a
-- stealth transfer may take its resource address from the workspace, so the
-- (resource_address, commitment) index cannot serve the lookup.
CREATE INDEX stealth_outputs_idx_commitment ON stealth_outputs (commitment);
