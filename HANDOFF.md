# Handoff

State of CrystalCore-Server for the next session. Repo: `C:\Users\tyler\Documents\code\CrystalCore-Server`
(branch `main`). The client is `C:\Users\tyler\Documents\code\CrystalCore` (branch `2013.08.09.672261`).
Read [ROADMAP.md](ROADMAP.md) first.

## Where it stands (2026-10-25)

Phase 0 is done. Phase 1's lobby works against the real β client up to the game login: encryption
handshake, Start, character select, character creation, and the client's game-login request (which
waits for the zone server, Phase 2). Characters are kept in memory only; storing them is next.

| crate | what is there |
|---|---|
| `crystal-common` | `Config` (figment: defaults → `crystal.toml` → `CRYSTAL_*` env), `Role`, `LobbyConfig`, `log::init` |
| `crystal-net` | `FrameHeader` (0x20 bytes in this build), `SegmentHeader` (0x10), `SegmentType` (with the client's fixed sizes per type), `FrameCodec`/`RawFrameCodec`, zlib, `Listener` + `serve` |
| `crystal-db` | `Databases { login, characters, world }` over `sqlx::Any`, PostgreSQL or SQLite (WAL); migrations per db per backend in `sql/<db>/<backend>/` |
| `crystal-state` | `State` enum: `Memory` or `Redis`. get/set/set_if_absent/delete with TTL, publish/subscribe |
| `crystal-lobby` | `crypt` (MD5 key, the client's Blowfish), `ipc` (header, opcodes, every packet below), `Lobby::handle` per connection with a `HostRole` |
| `crystal-runtime` | `Server::start(config)` / `local_addr(role)` / `shutdown()`; binds one lobby port, or two (one per client lobby host) |
| `apps/crystal-server` | clap (`--config`, `--role`), mimalloc, Ctrl-C shutdown |

Tests (23). CI is `.github/workflows/ci.yml` (Windows, Linux with Postgres + Redis, MSRV 1.94).
Remote: `origin` = https://github.com/zeruth/CrystalCore-Server.

## The lobby protocol, as found in the client

Every address below is ffxiv.exe 672261 at its preferred base; each is named, with the evidence, in
`CrystalCore/data/names.json` (and `structs.json` for `LobbyCryptEngine`, `Blowfish`, `MD5Context`).
The doc comments in `crates/crystal-lobby/src/{crypt/mod.rs,ipc.rs,lib.rs}` say the same per field.

1. **Handshake.** Client: segment 9 (key phrase @0x24, key = its `time()` @0x64). Key =
   `MD5(0x12345678 | key | 1000 | phrase[32])` (LE). Blowfish is standard but with LE block halves
   and a sign-extending key schedule; verified against the client's own code under emulation.
   Server: segment 10, 0x290 bytes, payload encrypted, its u32 @0 the connection id (non-zero).
   IPC payloads are then encrypted in their leading multiple of 32 bytes.
2. **IPC** header 0x10: `u16 0x14 | u16 opcode | 0 | u16 server | u32 time | 0`.
   - up 0x05 client version → down 0x0c service accounts (0x50-byte entries @0x10)
   - up 0x03 service login → down 0x15 worlds (0x54-byte entries @0x18; flags @4: 4 = open for new
     characters) and 0x0d characters (0x450-byte entries @0x18, two a packet; @0x10 u16 per-world
     limit, @0x12 u16 account slots, @8 bit 0 last page)
   - up 0x0b make character (step @0x19: 1 reserve name, 2 make with CharaMake JSON) → down 0x0e
     (@9 = 1, step @0xa, the character entry @0x10)
   - up 0x04 game login (content id @8) → down 0x0f (Phase 2), or 0x02 error (code @8, text @0x14)
3. **Quirks that cost time:**
   - The character list must hold at least one entry: character select waits for every entry's
     sheet rows, and with none the waiter never fires. An entry with content id 0 is a placeholder.
   - The client makes characters through its first region (lobby host 01, "JP") and its world
     picker shows that region's worlds. So host 01 shows a world "Lobby" (open, login refused) and
     host 02 ("NA") shows "Crystal" (login, no new characters); the server places characters made
     on Lobby on Crystal in its reply. That needs `DEV.LobbyPort02=54995` (`lobby.bind_2`).
   - The exe is relocated by ASLR (seen at 0xf40000): subtract the base and add 0x400000 before
     looking an address up.

## Next

1. **Store characters** (characters db): account from the session, the CharaMake JSON, content id;
   the character list from the db (still with the placeholder when empty); keep the Lobby → Crystal
   move. Then retainers (0x17) and 0x16 if the UI wants them.
2. **Phase 2**: answer game login (0x0f, read @0xe408c0) with the zone server, and the zone's own
   handshake.
3. Commit `CrystalCore/data/names.json` and `structs.json` in the client repo (left uncommitted
   there) and update its launch arguments (`rhipper.json`, MANIFEST) to `DEV.LobbyPort02=54995`.

## Tools (in this session's scratchpad; worth moving into `tools/`)

- corpus queries over `CrystalCore/build/corpus.jsonl` (names, grep, show, callers);
- a Win32 debug-API tracer: attaches to the running `ffxiv.exe`, rebases, logs breakpoint hits
  (re-armed by single-step), dumps memory at an argument;
- a 32-bit Unicorn harness that runs the client's MD5/Blowfish for test vectors.

## Environment notes

- **No WSL** (memory). Dev services are portable Windows builds via `tools/dev.ps1` (Redis 8.10.2 msys2
  build, PostgreSQL 18.4 EDB zip). User `crystal`, trust auth, 127.0.0.1 only: `tools/dev.ps1 start`.
- Builds and tests with `-j 2` (the machine is shared with translation jobs).
- To try the client: run `crystal-server` (ports 54994 and 54995), then start
  `CrystalCore/archive/installs/beta-672261/game/ffxiv.exe` with the launcher arguments in
  `CrystalCore/archive/MANIFEST.md` but `DEV.LobbyPort02=54995`. Only one client runs at a time.
  Set `CRYSTAL_LOBBY__CAPTURE_DIR` to save the traffic. The user drives the client.
- Hooks on this machine block shell file deletion and heredoc writes; use the Write/Edit tools. The
  Bash tool returns no output here: use PowerShell.
