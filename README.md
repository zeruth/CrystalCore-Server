# CrystalCore-Server

A modular, database-driven server for the CrystalCore client (the x64 port of the FFXIV: A Realm Reborn
β client, `2013.08.09.0001.0000`, build 672261), written in Rust.

- [ROADMAP.md](ROADMAP.md): decisions and phases.
- [HANDOFF.md](HANDOFF.md): where the work stands and what is next.

## Run

```powershell
cargo run -p crystal-server            # lobby on 127.0.0.1:54994, SQLite in var/, in-process state
```

Configuration is `crystal.toml`, overridden by `CRYSTAL_<SECTION>__<KEY>` environment variables
(`CRYSTAL_STATE__REDIS_URL`, `CRYSTAL_DATABASE__LOGIN__URL`, `CRYSTAL_LOBBY__CAPTURE_DIR`, ...).

## Hosted setup locally (Windows, no WSL)

`tools/dev.ps1` downloads portable Redis 8 and PostgreSQL 18 into `.dev/` (gitignored) and runs them:

```powershell
tools/dev.ps1 install    # once: download, init the cluster, create crystal_login/_characters/_world
tools/dev.ps1 start | stop | status
```

## Test

```powershell
cargo test -j 2 --workspace
# with the hosted backends too:
$env:CRYSTAL_TEST_POSTGRES = 'postgres://crystal@127.0.0.1:5432'
$env:CRYSTAL_TEST_REDIS    = 'redis://127.0.0.1:6379'
cargo test -j 2 --workspace
```

Use `-j 2`: the machine is shared with long translation jobs.
