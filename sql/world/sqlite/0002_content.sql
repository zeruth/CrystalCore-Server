-- Phase 4: overrides of the client's EXD rows, and the content the client never had. Every row
-- names the patch it belongs to and where it came from (source: 'sapphire', 'capture', 'hand').

-- A column of an EXD row, replaced (or a row added) for the server: value is text, read as the
-- column's type. AzerothCore's *_dbc tables, for every sheet at once.
CREATE TABLE exd_override (
    sheet   TEXT    NOT NULL,
    row_id  INTEGER NOT NULL,
    col     INTEGER NOT NULL,
    value   TEXT    NOT NULL,
    patch   TEXT    NOT NULL DEFAULT '2013.08.09.0001.0000',
    comment TEXT,
    PRIMARY KEY (sheet, row_id, col, patch)
);

-- Battle NPCs: where they stand and what they are (BNpcBase, BNpcName rows), how they come back.
CREATE TABLE bnpc_spawn (
    id           INTEGER PRIMARY KEY,
    territory    INTEGER NOT NULL,           -- TerritoryType row
    instance_id  BIGINT,                     -- the layout instance it came from; NULL if hand placed
    layer        TEXT    NOT NULL DEFAULT '',
    base_id      INTEGER NOT NULL,           -- BNpcBase
    name_id      INTEGER NOT NULL,           -- BNpcName
    level        INTEGER NOT NULL,
    x            REAL    NOT NULL,
    y            REAL    NOT NULL,
    z            REAL    NOT NULL,
    rotation     REAL    NOT NULL DEFAULT 0,
    wander_range REAL    NOT NULL DEFAULT 0,
    respawn_secs INTEGER NOT NULL DEFAULT 0, -- 0: the default
    pop_weather  INTEGER NOT NULL DEFAULT 0,
    pop_time_start INTEGER NOT NULL DEFAULT 0,
    pop_time_end INTEGER NOT NULL DEFAULT 0,
    loot_table   INTEGER NOT NULL DEFAULT 0, -- loot_table.id; 0 none
    link_group   INTEGER NOT NULL DEFAULT 0,
    link_family  INTEGER NOT NULL DEFAULT 0,
    link_range   INTEGER NOT NULL DEFAULT 0,
    normal_ai    INTEGER NOT NULL DEFAULT 0,
    source       TEXT    NOT NULL,
    patch        TEXT    NOT NULL DEFAULT '2013.08.09.0001.0000'
);
CREATE INDEX bnpc_spawn_territory ON bnpc_spawn (territory);
CREATE UNIQUE INDEX bnpc_spawn_instance ON bnpc_spawn (territory, instance_id, patch);

CREATE TABLE loot_table (
    id   INTEGER PRIMARY KEY,
    name TEXT    NOT NULL
);

CREATE TABLE loot_entry (
    loot_table INTEGER NOT NULL,             -- loot_table.id
    item_id    INTEGER NOT NULL,             -- Item
    chance     REAL    NOT NULL,             -- 0..1
    min_count  INTEGER NOT NULL DEFAULT 1,
    max_count  INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (loot_table, item_id)
);

-- Gathering points: the kind is the client's GatheringPoint row; where it stands is the server's.
CREATE TABLE gathering_point (
    id           INTEGER PRIMARY KEY,
    territory    INTEGER NOT NULL,
    point_id     INTEGER NOT NULL,           -- GatheringPoint
    x            REAL    NOT NULL,
    y            REAL    NOT NULL,
    z            REAL    NOT NULL,
    respawn_secs INTEGER NOT NULL DEFAULT 0,
    source       TEXT    NOT NULL,
    patch        TEXT    NOT NULL DEFAULT '2013.08.09.0001.0000'
);
CREATE INDEX gathering_point_territory ON gathering_point (territory);

-- Shops (this build has no GilShop sheet): who sells, and what.
CREATE TABLE shop (
    id    INTEGER PRIMARY KEY,
    name  TEXT    NOT NULL,
    enpc  INTEGER NOT NULL DEFAULT 0,        -- the ENpcResident who keeps it; 0 none yet
    patch TEXT    NOT NULL DEFAULT '2013.08.09.0001.0000'
);

CREATE TABLE shop_item (
    shop    INTEGER NOT NULL,                -- shop.id
    item_id INTEGER NOT NULL,                -- Item
    price   INTEGER,                         -- NULL: the Item sheet's price
    stock   INTEGER,                         -- NULL: unlimited
    PRIMARY KEY (shop, item_id)
);
