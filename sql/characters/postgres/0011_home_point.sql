-- A character's home point: the aetheryte Return takes it to (chosen at an aetheryte's menu).

CREATE TABLE home_points (
    character_id BIGINT NOT NULL PRIMARY KEY REFERENCES characters (id),
    aetheryte_id INTEGER NOT NULL
);
