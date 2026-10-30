-- Phase 6: a character's active quests, as the client keeps them (30 slots; down 0x1e0/0x1e1 send
-- each as its id's low 16 bits and a sequence, which the client's scripts read through
-- GetQuestSequence: 0 not accepted, 255 SEQ_FINISH).

CREATE TABLE quests (
    character_id BIGINT  NOT NULL,     -- characters.id
    slot         INTEGER NOT NULL,     -- 0-29
    quest_id     INTEGER NOT NULL,     -- Quest row (0x10000 + n)
    sequence     INTEGER NOT NULL,
    PRIMARY KEY (character_id, slot)
);
