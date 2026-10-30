-- Phase 6: the quests a character has completed (the client's QuestCompleteFlags, down 0x1e2 at
-- login and 0x1e3 per change: a bit per quest index, MSB first).

CREATE TABLE quest_complete (
    character_id INTEGER NOT NULL,     -- characters.id
    quest_id     INTEGER NOT NULL,     -- Quest row (0x10000 + n)
    PRIMARY KEY (character_id, quest_id)
);
