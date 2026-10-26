-- Whether a battle NPC attacks players who come near (its source's ActiveType 0) or only
-- fights back (1).

ALTER TABLE bnpc_spawn ADD COLUMN aggressive INTEGER NOT NULL DEFAULT 0;
