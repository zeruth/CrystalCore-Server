# Handoff

State of CrystalCore-Server for the next session. Repo: `C:\Users\tyler\Documents\code\CrystalCore-Server`
(branch `main`). The client is `C:\Users\tyler\Documents\code\CrystalCore` (branch `2013.08.09.672261`).
Read [ROADMAP.md](ROADMAP.md) first.

## Where it stands (2026-10-25)

Phase 0 is done apart from CI. Verified against the real β client: its two lobby connections reach
`crystal-server` (running on PostgreSQL + Redis), every frame decodes, and the frames are captured
byte-exact. The client's Start button stays grayed out: it is waiting for the reply to its encryption
init, which is Phase 1.

| crate | what is there |
|---|---|
| `crystal-common` | `Config` (figment: defaults → `crystal.toml` → `CRYSTAL_*` env), `Role`, `log::init` |
| `crystal-net` | `FrameHeader` (0x20 bytes in this build), `SegmentHeader` (0x10), `FrameCodec`/`RawFrameCodec`, zlib, `Listener` + `serve` (task per connection, cancellation token) |
| `crystal-db` | `Databases { login, characters, world }` over `sqlx::Any`, PostgreSQL or SQLite (WAL); migrations per db per backend in `sql/<db>/<backend>/` |
| `crystal-state` | `State` enum: `Memory` or `Redis` (RESP3 `ConnectionManager`, auto-reconnect and resubscribe, local broadcast fan-out). get/set/set_if_absent/delete with TTL, publish/subscribe |
| `crystal-lobby` | Logs frames, parses `EncryptionInit` (key phrase @0x24, key u32 @0x64 of the segment payload), writes captures. Sends nothing yet |
| `crystal-runtime` | `Server::start(config)` / `local_addr(role)` / `shutdown()`; no globals. Zone and chat roles are refused until Phase 2 |
| `apps/crystal-server` | clap (`--config`, `--role`), mimalloc, Ctrl-C shutdown |

Tests (14): config; SQLite + Postgres migrations and queries; memory + Redis keys/expiry/pubsub;
codec on the real capture (chunked decode, byte-exact re-encode, compression, bad sizes); lobby
parse; an end-to-end `Server` that receives the capture and captures it again. Clippy is clean;
`rustfmt.toml` sets width 120.

## What the client sends (from `protocol/672261/captures/lobby-encryption-init.bin`)

- Frame header 0x20: 16 zero bytes, `size` u32 @0x10, `connection_type` u16 @0x14 (3 = lobby),
  `segment_count` u16 @0x16, u8 @0x18 = 1, `compressed` u8 @0x19, 6 reserved.
  Later ARR clients have a 0x28 header with a timestamp; this one does not.
- One segment, type 9 (encryption init), 632 bytes; "Test Ticket Data" @payload 0x24, key @0x64.
- The client opens two lobby connections (LobbyHost01/02, both on 54994 here) and resends the same
  frame every 2 s on each until answered.
- Field names other than size/type/count are borrowed from later clients and unconfirmed.

## Next

1. **CI** (Phase 0's last item): GitHub Actions on Windows (fmt, clippy, test with SQLite + memory)
   and Linux (Postgres + Redis service containers, `CRYSTAL_TEST_*` set), plus
   `cargo ndk -t arm64-v8a check` for the library crates (`cargo-ndk` and NDK 27/28 are installed
   locally under `%LOCALAPPDATA%\Android\Sdk\ndk`). The Android check has not been run yet. The repo
   has no remote yet.
2. **Phase 1**: pull the opcode tables and the lobby's encryption handling out of the client
   (Rhipper/Ghidra over the port, per the client repo's working method). First target: the lobby's
   reply to the encryption init and the Blowfish key derivation, which should un-gray Start.

## Environment notes

- **No WSL** (memory). Dev services are portable Windows builds via `tools/dev.ps1` (Redis 8.10.2 msys2
  build, PostgreSQL 18.4 EDB zip). User `crystal`, trust auth, 127.0.0.1 only. They were stopped at
  the end of this session: `tools/dev.ps1 start`.
- Builds and tests with `-j 2` (the machine is shared with translation jobs).
- To try the client: run `crystal-server` (port 54994 must be free; `CrystalCore/tools/lobby_sink.py`
  also uses it), then start `CrystalCore/archive/installs/beta-672261/game/ffxiv.exe` with the
  launcher arguments in `CrystalCore/archive/MANIFEST.md`. Set `CRYSTAL_LOBBY__CAPTURE_DIR` to save
  the traffic. Ask the user before launching the client; they may prefer to launch it themselves.
- Hooks on this machine block shell file deletion (`Remove-Item`, `cmd del`) and heredoc writes;
  use the Write/Edit tools. A duplicate capture was moved to `.dev/` instead of deleted.
