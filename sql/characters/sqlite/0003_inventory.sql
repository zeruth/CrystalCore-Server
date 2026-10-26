-- Phase 5: a character's items, one row per filled slot (container and slot as the client
-- numbers them: bags 0-3, equipped 1000, currency 2000, armoury 3200-3500).

CREATE TABLE inventory (
    character_id INTEGER NOT NULL,     -- characters.id
    container    INTEGER NOT NULL,
    slot         INTEGER NOT NULL,
    item_id      BIGINT  NOT NULL,     -- Item row
    quantity     BIGINT  NOT NULL,
    hq           INTEGER NOT NULL DEFAULT 0,
    condition    INTEGER NOT NULL DEFAULT 30000,
    spiritbond   INTEGER NOT NULL DEFAULT 0,
    stain        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (character_id, container, slot)
);
