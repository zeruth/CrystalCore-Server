-- Phase 6: the aetherytes a character is attuned to (`Aetheryte` rows; the client's bits @0x14b4ed0,
-- filled from the player's status @0xe8 and set by order 0x1fd) and its unlocks (the client's bits
-- @0x14b4ec0, from the status @0xd8, set by order 0x29: 1 Return, 4 Teleport).

CREATE TABLE aetherytes (
    character_id BIGINT NOT NULL REFERENCES characters (id),
    aetheryte_id INTEGER NOT NULL,
    PRIMARY KEY (character_id, aetheryte_id)
);

CREATE TABLE unlocks (
    character_id BIGINT NOT NULL REFERENCES characters (id),
    unlock_id INTEGER NOT NULL,
    PRIMARY KEY (character_id, unlock_id)
);
