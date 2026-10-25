# CrystalCore-Server roadmap

The server side of CrystalCore. The first target is the client the port is built from: the
**FFXIV: A Realm Reborn β client, game `2013.08.09.0001.0000`, build 672261** (`ffxiv.exe` linked
2013-08-08). The client repo tracks it on the branch `2013.08.09.672261`.

Later targets, in order: later 2.x clients found on the patch server (`D2013.12.05`, `D2014.05.27`),
then the shut-down CN mobile game (水晶世界). The server is built so each of these is a protocol
module, not a fork.

## Decisions

### Language: Rust

Rust is the right choice. Reasons that matter for this project specifically:

- **One codebase for hosted and embedded.** The server has to run as a Linux/Windows service *and*
  inside the client on Android and iOS. Rust cross-compiles to `aarch64-linux-android` and
  `aarch64-apple-ios` with no extra work (the Android targets are already installed here), and builds
  as a library with a C ABI that the C port can link.
- **The client is C.** Linking the recomp into a Rust process (or the reverse) is plain FFI through
  `cc`/`cmake` crates and `extern "C"`. Nothing is lost against C++ here.
- **The network layer parses hostile input.** Memory safety pays for itself in packet parsing, and
  `tokio` covers the connection model.
- **Prior art.** Rust already has FFXIV tooling: *physis* (SqPack, EXD, LGB and model formats) and
  *Kawari* (a Rust server for current patches). Both are reference material at least; physis may be
  usable as a dependency (it is GPL-3.0, so licence the server compatibly if we take it). *Sapphire*
  (C++, ARR 3.x+) is the best reference for packet layouts and game rules.

The one real cost is compile time, which hurts content iteration. That is why content lives in
scripts (below), not in Rust.

### Shape: AzerothCore's, with Square's service split

Keep from AzerothCore:

| AzerothCore | CrystalCore-Server |
|---|---|
| `src/` core + `modules/mod-*` dropped in and built | Cargo workspace; modules are crates under `modules/`, enabled in `modules.toml`, registered at link time |
| `ScriptMgr` hooks (`PlayerScript`, `WorldScript`, ...) | Hook traits per subsystem (`PlayerHooks`, `ZoneHooks`, `ChatHooks`, `LobbyHooks`, ...), callable from Rust modules and Lua |
| `acore_auth` / `acore_characters` / `acore_world` | `login` / `characters` / `world` schemas |
| DBC files read from the client, `*_dbc` tables override them | EXD sheets read from the client's SqPack; `world.*_override` tables override rows |
| `world` DB holds what the client never had (spawns, loot, SmartAI) | `world` holds BNpc spawns, loot, gathering nodes, shop stock, AI and other server-only data |
| SQL updates applied on startup | Versioned migrations (`sqlx migrate`) applied on startup |
| `worldserver.conf` | `crystal.toml` with env overrides |

Take from how Square built it, because the client expects it or because it is better:

- **Separate connections: lobby, zone, chat.** The 2.x client connects to the lobby (port 54994 in our
  launcher arguments), and after world select opens a zone connection and a chat connection. The
  server has those three services; one binary runs any or all of them (`--role lobby,zone,chat`).
- **The 3-second server tick** for DoT/HoT, regen and status timers. The game's feel depends on it.
- **The client's data is the source of truth.** Quests, items, actions, ENpcs and maps are in EXD and
  LGB; the server reads them from the same install instead of copying them into SQL.
- **Zones are owned by single-threaded loops**, like AzerothCore's `Map::Update` and like Square's zone
  servers: each zone (or instance) is one task that owns its actors; cross-zone traffic goes
  through messages. No locks in game logic.
- **Lua for content.** 1.x's server ran its content in Lua, and the client still carries Lua. Content
  scripts (quests, NPC talk, duties, events) are Lua through `mlua` (Lua 5.4 or Luau, vendored, works on
  Android and iOS), hot-reloadable. Rust modules are for systems; Lua is for content.

### Database

- **PostgreSQL** for hosted servers, **SQLite** for the embedded build (a phone has no database
  server). Both through `sqlx`. Migrations are written in the common subset; where they cannot be,
  each backend gets its own file.
- Characters are loaded into the zone that owns them and saved on a timer and on logout, the same
  way AzerothCore saves players. The database is never queried in the middle of a tick.
- The three databases have separate URLs, so they can live on different hosts (and `characters`
  can later be split per world).

### Shared state and scaling: Redis

- **Redis (or Valkey)** holds what several processes must see at once and what expires: sessions
  and lobby→zone handoff tokens, presence (who is online, in which zone), and pub/sub for chat,
  tells, linkshells and cross-zone messages. `crystal-state` wraps it; the embedded build uses its
  in-process backend with the same semantics (the `redis` cargo feature drops the dependency).
- One process per role is the unit of scaling: lobbies are stateless (sessions in Redis) and can sit
  behind a load balancer; zone processes own sets of zones and instances; chat fans out over pub/sub.
  The same binary runs any mix of roles.
- Performance rules: no I/O on a tick; per-zone single-threaded loops; one multiplexed Redis
  connection for commands and one for subscriptions per process, fanned out locally; zero-copy
  packet payloads (`bytes`); mimalloc in the binary; thin LTO in release.

### Embedded mode ("the client as a server module")

The core is a library first (`crystal-server` is a thin binary over it). The embedded build:

1. links the recomp's C code into a `crystal-embed` crate (or links `crystal-embed` into the C port as a
   static library: decide by which side owns `main` on Android/iOS),
2. starts lobby, zone and chat in-process on SQLite,
3. replaces the sockets with an in-memory transport, keeping the packet bytes identical, so the client
   code that sends and receives is the same code that talks to a hosted server.

Rules that follow from it, from day one:

- no dependency that does not build for Android and iOS (`rustls`, not OpenSSL; no JIT-based
  scripting such as wasmtime, which iOS forbids);
- no global state; a server is a value that can be created and dropped;
- no assumption about the filesystem layout beyond a data root passed in.

### Protocol versions

Opcodes and packet layouts change with every client build. Every packet definition is tied to a
client build, and an opcode table per build lives in data (`protocol/672261/opcodes.toml`), not in
code. A connection picks its protocol module from the version the client reports at the lobby.

**The opcode tables come from the client, not from guesswork.** We have the port: the client's IPC
dispatch (the switch over opcodes for each connection type) is in the translated code. Phase 1
starts by extracting every opcode, its handler, and the size the handler reads, with Rhipper and
Ghidra, the same way the port itself is made. Captures and Sapphire's layouts are cross-checks.

## Workspace layout (planned)

```
CrystalCore-Server/
├─ crates/
│  ├─ crystal-common     config, logging (tracing), ids, time, errors
│  ├─ crystal-net        framing, segments, Blowfish (lobby), zlib, connection tasks, transports
│  ├─ crystal-protocol   packet definitions and opcode tables, one module per client build
│  ├─ crystal-data       SqPack, EXD, LGB, navmesh readers (physis or our own)
│  ├─ crystal-db         sqlx pools, migrations, repositories
│  ├─ crystal-state      Redis / in-process key-value with expiry and pub/sub
│  ├─ crystal-runtime    a server as a value: storage + configured services, start/shutdown
│  ├─ crystal-world      actors, zones, instances, the tick, game systems
│  ├─ crystal-script     hook registry and the Lua host
│  ├─ crystal-lobby      lobby service
│  ├─ crystal-zone       zone service
│  ├─ crystal-chat       chat service
│  └─ crystal-embed      C ABI and the in-process transport
├─ apps/
│  ├─ crystal-server     the binary
│  └─ crystal-tools      db import, packet replay, opcode dump, EXD browser
├─ modules/              optional crates (mod-* in AzerothCore terms)
├─ scripts/              Lua content
├─ sql/                  migrations: login, characters, world
└─ protocol/             per-build opcode tables and captures
```

## Phases

Each phase ends with something you can do in the client. The client running against the server is the
test.

### Phase 0: Skeleton (done 2026-10-25)

- Cargo workspace with the crates above (empty where not yet needed), `crystal.toml`, `tracing`.
- `sqlx` with both backends, the three schemas and a first migration (`account`, `session`).
- CI: build and test for `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu` (the Linux job
  against PostgreSQL and Redis), plus the MSRV. The Android check waits for Phase 8, when there is
  an Android client to embed into.
- `crystal-server --role lobby` listens on 54994 and logs what the client sends (replaces
  `CrystalCore/tools/lobby_sink.py`).

**Done when:** the β client's lobby connection is accepted and its first packets are logged and
decoded as frames, by the Rust server.

### Phase 1: Protocol and lobby

- Extract the opcode tables for 672261 from the port (lobby, zone, chat; client to server and server
  to client) into `protocol/672261/`.
- Packet framing: bundle header, segments (session init, IPC, keepalive, encryption init), zlib.
- Lobby encryption: the client's encryption init and the Blowfish key derivation, read out of the
  client's code.
- Login with the test ticket (`DEV.TestSID=0`, "Test Ticket Data"); a real account and session token
  later.
- Service account list, world list, character list.
- Character creation (name check, the creation data), character deletion.
- Packet replay tool: feed a saved session through a protocol module and compare.

**Done when:** the client reaches character select against the server, creates a character, and
sees it in the list after a restart.

### Phase 2: Entering the world

- World select hands the client the zone and chat addresses.
- Zone connection: init, player setup, the first zone (the character's starting city), player spawn.
- Movement in, position persisted; zone lines and teleports between zones.
- Chat connection: say, shout, tell, linkshell later.
- ENpcs placed from the zone's LGB data.

**Done when:** a character logs in, runs around Gridania/Limsa/Ul'dah, changes zones, logs out, and
logs back in where it left.

### Phase 3: Many players and the world model

- Actor visibility (spawn/despawn by range), other players' movement and appearance.
- Decide the actor model: plain structs per zone first; adopt `bevy_ecs` or `hecs` only if modules
  need to attach state to actors and plain hooks cannot do it.
- Commands: GM commands as a module (`.tele`, `.item`, `.level`, in AzerothCore's style).
- Load testing with headless clients built from `crystal-protocol`.

### Phase 4: Data

- EXD reader and the override tables.
- Import tools for server-only data: BNpc spawns, loot, gathering points, shop stock. None of this is
  in the client; sources are captures, Sapphire's data where its patch matches, and hand placement.
- An EXD browser in `crystal-tools`.

### Phase 5: Core gameplay

- Inventory, equipment, the armoury chest, gil, shops.
- Classes and jobs, levels, experience.
- Actions and combat: targeting, the GCD as the client reports it, damage and healing, status effects
  on the 3 s tick, death and raise.
- BNpc AI: aggro, leash, pathing on the zone's navmesh, scripted abilities in Lua.
- Loot and the treasure coffer.

### Phase 6: Content

- The event system: talk, cutscenes, the client's event handlers as server events.
- The Lua content API, mirroring what the client's quest scripts expect; check whether the β data
  carries quest/event Lua that tells us the server side's sequence steps.
- Main scenario and class quests to the end of 2.0, levequests, FATEs, gathering and crafting.
- Hot reload of scripts; a test harness that runs a quest script against a fake player.

### Phase 7: Group content and social

- Party, the duty finder, instanced duties (an instance is a zone loop of its own).
- Free companies, linkshells, friend list, the market board, retainers.

### Phase 8: Embedded mode

- `crystal-embed`: in-process transport, SQLite, the C ABI.
- Single-player build of the PC port: one process, no network.
- Android build of the server library; iOS when the client gets there.

**Done when:** the PC port starts with no external server and plays Phase 6's content.

### Phase 9: Later clients

- `2013.12.05` and `2014.05.27` clients: a protocol module and opcode table each, and the data
  differences between patches. Content scripts are tagged with the patch they belong to.
- The server serves more than one build at once (per-world build).

### Phase 10: Crystal World (CN mobile)

- Capture and preserve what is left of the mobile protocol and data first; that part is urgent and
  can happen in parallel with everything above.
- A protocol module for the mobile client; the game rules it shares with 2.x reuse `crystal-world`.

## Working method

Same as the client: find the truth in the binary, fix the tool rather than the output.

- Packet layouts and opcodes come from the client's code. When the server and the client disagree,
  the client is right.
- Every protocol fix gets a replay test (a captured or recorded session that has to round-trip).
- Every gameplay system gets tests that drive it through the hook API, not through internals.
