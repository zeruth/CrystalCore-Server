-- Server-only content and overrides of the client's EXD rows. Every content row will name the
-- game patch it belongs to, so one database can serve several client builds.

CREATE TABLE patch (
    version TEXT    PRIMARY KEY,   -- yyyy.mm.dd.pppp.rrrr, as in the patch file's name
    build   INTEGER NOT NULL,
    name    TEXT    NOT NULL
);
INSERT INTO patch (version, build, name) VALUES ('2013.08.09.0001.0000', 672261, 'A Realm Reborn beta');
