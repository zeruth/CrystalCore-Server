-- Characters. The creation data (appearance, class, city) arrives with Phase 1, when its layout is
-- read out of the client.

CREATE TABLE characters (
    id         INTEGER PRIMARY KEY,
    account_id BIGINT  NOT NULL,   -- login.account.id (another database: no foreign key)
    world_id   INTEGER NOT NULL,   -- login.world.id
    name       TEXT    NOT NULL,
    created_at BIGINT  NOT NULL,
    deleted_at BIGINT
);
CREATE UNIQUE INDEX characters_world_name ON characters (world_id, lower(name)) WHERE deleted_at IS NULL;
CREATE INDEX characters_account ON characters (account_id);
