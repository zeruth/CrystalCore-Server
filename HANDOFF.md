# Handoff

State of CrystalCore-Server for the next session. Repo: `C:\Users\tyler\Documents\code\CrystalCore-Server`
(branch `main`, pushed to https://github.com/zeruth/CrystalCore-Server). The client is
`C:\Users\tyler\Documents\code\CrystalCore`. Read [ROADMAP.md](ROADMAP.md) first.

## Where it stands (2026-10-30)

Phases 0-2 work against the real β client, unmodified: log in, make a character (stored), pick it,
enter Limsa Lominsa, see its event NPCs, move (position saved), walk through a zone exit into Middle
La Noscea, log out cleanly, log back in where you left. Seen in the client since: chat in the log,
battle NPCs with their bodies, event NPC talk (no longer hangs).

**Everything from Phase 3 on has only been run headless** (crystal-bot, the end-to-end test in
`crystal-runtime/tests/world.rs`), never with the real client. What exists:

- Phase 3: other players spawn/move/despawn (0x190/0x192/0x191), chat relayed (down 0x67), GM
  commands, the server console, `crystal-tools bots`.
- Phase 4: EXD overrides, Sapphire's battle NPCs imported and seeded, `.bnpc`, the EXD browser.
- Phase 5, partly: the player's setup before InitZone; inventory (load, grant, send, save; item
  *moves* only: split, discard and sort are answered but change nothing); gear changes (Equip);
  levels and experience (`.level`, `.exp`, kills pay); targeting, auto-attack, damage (placeholder
  numbers, `combat.rs`), death (Order 6) and self-revive; battle NPCs fight back, chase in a
  straight line (no navmesh), leash, aggressive ones aggro; HP back on the 3 s tick; actions
  (0x196) by potency and recast from the Action sheet, hostile ones only; **statuses** (below).
- Statuses (2026-10-30): an actor's 30 slots in `crystal-world` (`ZoneState::set_status`, worn off
  on the tick, cleared on death), carried in every HudParam (0x140; the client replaces all 30
  slots with each, @0xa555b0 → @0xb7b840) and sent to the player as Status 0x148 (@0xa4e4b0 →
  @0xb7ba00). `.status <id> (seconds) (param)`, `.status off <id>`, `.status clear`. Nothing puts
  statuses on yet (no action → status data) and none tick damage or healing.
- Casts and heals (2026-10-30): an action with a cast time (Action col 33) starts a cast, sent
  as RequestCast 0x196 down (layout from @0xa52d10: action, kind, the action again, time @8,
  target @0xc, rotation @0x10; the player's own cast bar @0xac3e10), and lands on the tick it
  ends, checked again then. Moving past half a yalm cancels it and dying interrupts it, giving the
  recast back, sent as Order category 0xf (@0xa55f80 case 0xf: LogMessage row, action kind,
  action, interrupted; 537 "you cancel <action>", 540 "<action> is interrupted"). A friendly
  action with a potency heals (Cure: "Cure Potency: 400", which `potency_in` now reads) a player
  in range, its user when no player is targeted, as ActionResult1 with an HP-gain effect (kind 4,
  Sapphire's: unverified in this client) and the target's HudParam. Numbers: `combat::heal_amount`.
- MP and TP (2026-10-30): tracked per player in the zone, sent in every HudParam. Costs
  (Action col 27 type, col 28 cost): TP (5) as is, MP (3) that percent of the level's MP base,
  `ParamGrow` col 5 (Sapphire's rule): Cure 5 MP at level 1, 133 at 50. Checked when an action
  starts, paid when it lands. Back each tick: MP 6% of max out of a fight, 2% in one (rounded
  down), TP 60. Max MP: `col5 * (main + PIE) / (2 main) * ClassJob MP / 100`, fitted to 2.x's
  measured level-50 lines (WHM `7.90 PIE + 1596`, SCH `7.57 + 1530`, BLM `8.23 + 1662`,
  forum.square-enix.com thread 174125, June 2014): exact at 50, the same rule unmeasured below.
  The server used col 4 before (about a ninth of the MP). Base HP still reads col 5: unverified.
- Missing in Phase 5: gil spending/earning, shops (no GilShop sheet in this build), loot and
  coffers (tables exist, empty), raise, class change (down 0x1a4), hate lists, Lua (no
  `crystal-script` crate).

Research: 2.0-2.x sources are worth the most (the β is 2.0); later ones changed numbers (5.0
rescaled MP to 10,000, so "Cure 400 MP" is not 2.x). Dated 2.x patch notes (Lodestone 2.1, 2.3)
give potencies, TP costs and recasts; the forums give measurements. GameFAQs and the Wayback
Machine refuse agent fetches.

Test with the client next, in this order: run the server (`tools/run-server.ps1`), in its console
`gm Null Crystal 3`, log in; `.gm`, `.help`, `.tele 129`; `cargo run -p crystal-tools -- bots -n 5`
(bots render and move?); `.status 50 30` (a status icon on the party list and the HUD, counting
down?); attack a battle NPC in Middle La Noscea (hits, its HP bar, death, experience); Fast Blade;
as a conjurer, take a hit and cast Cure (the cast bar, the heal's number, the HP bar).

| crate | what is there |
|---|---|
| `crystal-common` | `Config` (`[lobby]`, `[zone]`, `[data] game`), `chara` (CharaMake / ClientSelectData JSON, start points) |
| `crystal-net` | frame/segment codec, `SegmentType` with the client's fixed sizes, the IPC header (`ipc`) |
| `crystal-db` | three databases on Postgres or SQLite; `characters` repository (content id, CharaMake text, position) |
| `crystal-state` | Redis / in-process key-value; the lobby's game-login session handoff |
| `crystal-data` | SqPack, EXD sheets, LGB layouts; `WorldData`: territory layouts, zone exits (exit range → pop range), event NPC placements (`Level`) |
| `crystal-lobby` | encryption, service accounts, worlds, characters (from the db; one placeholder entry when none), making characters (Lobby world → Crystal), game login (0x0f → zone) |
| `crystal-protocol` | zone packets both ways (spawn, despawn, actor move, chat up/down, position, InitZone, session init, setup, items, HudParam, Status, actions, orders) |
| `crystal-world` | territories as single-threaded tasks owning actors: visibility by range in 99 slots, move and chat relay, presence; combat, battle NPC AI, statuses (`zone`), `combat`, `inventory`, `progress` |
| `crystal-zone` | session init, InitZone, spawns, position updates into the world, zone exits, chat and commands (`commands`: GM level from `login.account.gm_level`, re-read per command; `.gm on/off`), ping, logout |
| `mod-gm` | `.gm`, `.help`, `.gps`, `.who`, `.tele`, `.goto`, `.bnpc`, `.level`, `.exp`, `.item`, `.status` |
| `crystal-console` | the binary's terminal UI: logs, status, `help`/`who`/`gm`/`gm-account`/`log`/`quit`. GM levels are set only here; `--plain` (or no terminal) for plain logs |
| `crystal-bot` / `crystal-tools bots` | headless zone clients; the load test (bot characters on the `bots` account) |
| `crystal-tools exd` / `override` / `import` / `export` / `prune` | the EXD browser, `world.exd_override` editing, Sapphire's battle NPC import, the seed file, removing content this build lacks |
| `data/672261` | seed data a fresh server loads into empty world tables (9,679 battle NPCs, filtered to the β: territory, BNpcBase, BNpcName, layout layer). The repo is AGPL-3.0 (`NOTICE.md` credits Sapphire) |
| `crystal-runtime` | `Server::start`: lobby (two ports) and zone, one process |

91 tests; the game-data tests run when `CRYSTAL_TEST_GAME` names the install's `game` folder.
Untested: `crystal-zone/src/lib.rs` has no unit tests (the end-to-end test covers it), `mod-gm`
none of its own, `crystal-data/src/world.rs` only with the install.

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

0. **Phases 3-5 with the client** (the list under "Where it stands"): other players render (kind
   1 spawn in a slot) and moves animate (speed 60/24 from Sapphire; the flags passed through
   unverified; the snap bit's caller wants a `bptrace.py` run, see e0ff1e5), command
   replies show, statuses show and count down, combat's packets (ActionResult1, HudParam, Order 6)
   play. Load-test numbers with `crystal-tools bots`.
1. Statuses that do something: which statuses actions put on (no such data in the client's sheets
   for the β: Sapphire's action data where it matches, as with battle NPCs), DoTs and HoTs on the
   3 s tick (with a death through `strike`'s path), and Status's effects on stats.
2. The rest of Phase 5 (the missing list above): class change (down 0x1a4), then loot. The MP
   cost rule can be checked against the client's own tooltip ("MP Cost"), which computes it.
   Cols 12-14 of Action look like the self/party/friendly target flags
   (all true on Cure, hostile 15 false): verify on more rows before heals use them.
3. Unit tests for `crystal-zone/src/lib.rs` (1100 lines covered only end to end) and `mod-gm`.

## Environment notes

- **No WSL**. Dev services via `tools/dev.ps1` (Redis, PostgreSQL); stopped at the end of sessions.
- Builds and tests with `-j 2`.
- Run: `tools/run-server.ps1` (sets the game data, clears `NO_COLOR`, which agent shells set and which turns the console's colors off); in the console, `client` starts the β client pointed at the lobby. By hand: `$env:CRYSTAL_DATA__GAME = "<CrystalCore>/archive/installs/beta-672261/game"`, then
  `cargo run -p crystal-server` (lobby 54994/54995, zone 54992). Client: `ffxiv.exe` with the
  MANIFEST's arguments but `DEV.LobbyPort02=54995`. One client at a time; the user drives it.
- RE tools are in `tools/re/` (see its README). Sapphire is cloned at `code/3rdparty/Sapphire` as a
  cross-check (4.0 era; the client is the authority).
- Hooks block shell file deletion and heredoc writes; the Bash tool returns no output here (use
  PowerShell).
