-- Phase 6: a quest's six variable bytes (UI8A-UI8F, the client's slot +8 - +0xd), packed little
-- endian (UI8A the lowest byte).

ALTER TABLE quests ADD COLUMN vars BIGINT NOT NULL DEFAULT 0;
