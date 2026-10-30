# Notice

CrystalCore-Server is free software under the GNU Affero General Public License, version 3 or
later ([LICENSE.md](LICENSE.md)).

## Third-party data

`data/672261/bnpc_spawn.csv` (battle NPC spawn points) is derived from the `battlenpc` table of
[Sapphire](https://github.com/SapphireServer/Sapphire) (AGPL-3.0,
`sql/migrations/20211217125038_UpdateBattleNpc.sql`), filtered to what the FFXIV: A Realm Reborn β
client (build 672261) has: its territories, `BNpcBase` and `BNpcName` rows, and layout layers.
Regenerate it with `crystal-tools import sapphire-bnpcs` then `crystal-tools export`.

`data/672261/gathering.json` (gathering point positions) is derived from the gathering node
data (`libs/data/src/lib/json/nodes.json`: each node's map, map coordinates and radius) of
[FFXIV Teamcraft](https://github.com/ffxiv-teamcraft/ffxiv-teamcraft) (MIT, Copyright (c) 2017
Flavien Normand), placed on the β's own `GatheringPoint` rows; the positions inside each area
are this project's guesses. Regenerate it with `crystal-tools import gathering-nodes`.

FINAL FANTASY XIV is a registered trademark of Square Enix. This project is not affiliated with
Square Enix; it contains none of the game's files and reads them from an install the user has.
