-- Phase 6: the map parts a character has revealed (`Map` row, part: a map range's data @0x2c), sent
-- back in the player's status (@0xf7 / @0x237) and answered one by one with down 0x28c.

CREATE TABLE discovery (
    character_id BIGINT NOT NULL REFERENCES characters (id),
    map_id INTEGER NOT NULL,
    part INTEGER NOT NULL,
    PRIMARY KEY (character_id, map_id, part)
);
