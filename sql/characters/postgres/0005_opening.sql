-- Phase 6: how far a character is through its town's opening (the scripts' OPENING_SEQ_0-2, as
-- the client registers them @0x6efc70): 0 until the opening's cutscenes (scene 0) have played.

ALTER TABLE characters ADD COLUMN opening INTEGER NOT NULL DEFAULT 0;
