-- Phase 5: a character's level and experience per class slot (ClassJob col 4: the 19 slots the
-- client's PlayerStatus carries; a class and its job share one).

CREATE TABLE class_progress (
    character_id BIGINT  NOT NULL,     -- characters.id
    slot         INTEGER NOT NULL,     -- 0-18
    level        INTEGER NOT NULL,
    exp          BIGINT  NOT NULL DEFAULT 0,
    PRIMARY KEY (character_id, slot)
);
