-- What the client knows a character by, and what it was made with (Phase 1); where it stands
-- (Phase 2: NULL until it first enters a zone, then the starting point is used).

ALTER TABLE characters ADD COLUMN content_id BIGINT;           -- the id the client sees (u64)
ALTER TABLE characters ADD COLUMN appearance TEXT NOT NULL DEFAULT '';  -- the CharaMake JSON
ALTER TABLE characters ADD COLUMN zone_id INTEGER;
ALTER TABLE characters ADD COLUMN pos_x DOUBLE PRECISION;
ALTER TABLE characters ADD COLUMN pos_y DOUBLE PRECISION;
ALTER TABLE characters ADD COLUMN pos_z DOUBLE PRECISION;
ALTER TABLE characters ADD COLUMN rotation DOUBLE PRECISION;
CREATE UNIQUE INDEX characters_content_id ON characters (content_id);
