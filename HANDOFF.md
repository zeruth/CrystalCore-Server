# Handoff

State of CrystalCore-Server for the next session. Repo: `C:\Users\tyler\Documents\code\CrystalCore-Server`
(branch `main`, pushed to https://github.com/zeruth/CrystalCore-Server). The client is
`C:\Users\tyler\Documents\code\CrystalCore`. Read [ROADMAP.md](ROADMAP.md) first.

## Where it stands (2026-10-26)

Phases 0-2 work against the real β client, unmodified: log in, make a character (stored), pick it,
enter Limsa Lominsa, see its event NPCs, move (position saved), walk through a zone exit into Middle
La Noscea, log out cleanly, log back in where you left. Chat is read but not relayed (no other
players yet: Phase 3).

| crate | what is there |
|---|---|
| `crystal-common` | `Config` (`[lobby]`, `[zone]`, `[data] game`), `chara` (CharaMake / ClientSelectData JSON, start points) |
| `crystal-net` | frame/segment codec, `SegmentType` with the client's fixed sizes, the IPC header (`ipc`) |
| `crystal-db` | three databases on Postgres or SQLite; `characters` repository (content id, CharaMake text, position) |
| `crystal-state` | Redis / in-process key-value; the lobby's game-login session handoff |
| `crystal-data` | SqPack, EXD sheets, LGB layouts; `WorldData`: territory layouts, zone exits (exit range → pop range), event NPC placements (`Level`) |
| `crystal-lobby` | encryption, service accounts, worlds, characters (from the db; one placeholder entry when none), making characters (Lobby world → Crystal), game login (0x0f → zone) |
| `crystal-zone` | session init, InitZone, the player's spawn, event NPC spawns, position updates, zone exits, ping, logout |
| `crystal-runtime` | `Server::start`: lobby (two ports) and zone, one process |

29+ tests; the game-data tests run when `CRYSTAL_TEST_GAME` names the install's `game` folder.

## The protocol, as found in the client

Every finding is in `CrystalCore/data/names.json` with its evidence (55 functions) and in the doc
comments of `crates/crystal-lobby/src/{crypt,ipc}` and `crates/crystal-zone/src/{lib,ipc}.rs`.

- **Lobby** (encrypted, see crypt): 0x05 version → 0x0c accounts; 0x03 service login → 0x15
  worlds, 0x0d characters; 0x0b make (steps 1, 2) → 0x0e; 0x04 game login → 0x0f (zone host,
  port, actor id).
- **Zone** (not encrypted; the client opens a zone and a chat connection to the same address): the
  session init (type 1, actor id as text) → type 2 with the actor id; up 0x66 Init → down 0x19a
  InitZone; the client sends 0x69 FinishLoading → down 0x190 spawn (the player as its own actor id;
  event NPCs as kind 3 with their ENpc id); up 0x19a position; up 0x190 zone exit (exit range id) →
  InitZone for the destination; up 0x65 ping → 0x65; up 0x67 chat; up 0x263 logout → down 0x2d6.
- **Quirks**: the character list must hold an entry (a placeholder); the client makes characters
  through its first region (host 01 shows "Lobby", host 02 "Crystal"); the opening territories
  (181-183) wait for their opening event, so new characters start in the city zones; the exe is
  relocated by ASLR (0xf40000 here).

## Next

1. **Phase 3**: more than one player: a world/zone task that owns actors, spawning players for each
   other by range, relaying movement and chat (down 0x67). Zones should own NPC actor ids too (they
   are per connection now).
2. Sapphire sends player setup, stats and class before InitZone (`PlayerMgr::onMoveZone`); this
   client went in without them, but the UI (HP bars, class, gear) will want them: find this
   build's packets (Sapphire's 4.0 opcodes don't apply; the client's `PacketDispatcher` table is in
   `tools/re/zonetable.py`).
3. Gear: the spawn's model fields are zero (no equipment); class starting gear from the game data.
4. Commit `CrystalCore/data/names.json` and `structs.json` in the client repo, and its launch
   arguments (`DEV.LobbyPort02=54995`).

## Environment notes

- **No WSL**. Dev services via `tools/dev.ps1` (Redis, PostgreSQL); stopped at the end of sessions.
- Builds and tests with `-j 2`.
- Run: `$env:CRYSTAL_DATA__GAME = "<CrystalCore>/archive/installs/beta-672261/game"`, then
  `cargo run -p crystal-server` (lobby 54994/54995, zone 54992). Client: `ffxiv.exe` with the
  MANIFEST's arguments but `DEV.LobbyPort02=54995`. One client at a time; the user drives it.
- RE tools are in `tools/re/` (see its README). Sapphire is cloned at `code/3rdparty/Sapphire` as a
  cross-check (4.0 era; the client is the authority).
- Hooks block shell file deletion and heredoc writes; the Bash tool returns no output here (use
  PowerShell).
