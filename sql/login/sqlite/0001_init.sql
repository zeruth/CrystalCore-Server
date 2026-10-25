-- Accounts, and the worlds the lobby lists (AzerothCore's account and realmlist).
-- Sessions are not stored here: they live in the shared state (Redis) with an expiry.
-- Times are unix seconds in BIGINT, the same on both backends.

CREATE TABLE account (
    id            INTEGER  PRIMARY KEY,
    username      TEXT     NOT NULL,
    password_hash TEXT     NOT NULL,
    gm_level      SMALLINT NOT NULL DEFAULT 0,
    created_at    BIGINT   NOT NULL,
    last_login_at BIGINT,
    banned_until  BIGINT
);
CREATE UNIQUE INDEX account_username ON account (lower(username));

CREATE TABLE world (
    id        INTEGER PRIMARY KEY,   -- the id the client sees
    name      TEXT    NOT NULL UNIQUE,
    build     INTEGER NOT NULL,      -- client build it serves, e.g. 672261
    zone_host TEXT    NOT NULL,
    zone_port INTEGER NOT NULL,
    chat_host TEXT    NOT NULL,
    chat_port INTEGER NOT NULL,
    enabled   BOOLEAN NOT NULL DEFAULT TRUE
);
