//! Server tools:
//!
//! - `exd`: browse the client's EXD sheets as the server sees them (overrides applied).
//! - `override`: change an EXD row for the server (`world.exd_override`).
//! - `import`: server-only content into the world database (Sapphire's battle NPCs).
//! - `export`: the world's battle NPC spawns as the repository's seed file.
//! - `prune`: world content that does not belong in this client build, found (and removed).
//! - `file`: raw files out of the client's SqPack by path (quest scripts, layouts).
//! - `coverage`: how much of the β the server serves, domain by domain, as an HTML report.
//! - `bots`: headless clients that log into the zone service, walk circles around a point, ping and
//!   talk, for load tests and for seeing other players from the real client.
//!
//! Each bot is a stored character (`Bot 001`, ...) on the `bots` account, made on first use with
//! the look of an existing character (`--look`), and placed at `--territory`/`--at` before it
//! connects. Bots skip the lobby: the zone takes the actor id game login would give.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, Subcommand};
use crystal_bot::{Bot, Down};
use crystal_common::Config;
use crystal_data::{Overrides, WorldData};
use crystal_db::{BETA_PATCH, Databases, ExdOverride, Position};
use crystal_protocol::zone::chat;
use tracing::{info, warn};

/// Player actor ids are this plus the character's row id (the zone's `PLAYER_ACTOR_BASE`).
const PLAYER_ACTOR_BASE: u32 = 0x1000_0000;

mod coverage;
mod derive_quests;
mod exd;
mod gathering_nodes;
mod import;
mod prune;
mod quest;
mod sapphire_quests;
const BOT_ACCOUNT: &str = "bots";

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// The server's configuration (for the databases and the zone address).
    #[arg(long, default_value = "crystal.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The client's EXD sheets (the game data is `[data] game`, or `CRYSTAL_DATA__GAME`).
    Exd {
        #[command(subcommand)]
        command: exd::ExdCommand,
    },
    /// The server's overrides of EXD rows (applied when the zone service starts).
    Override {
        #[command(subcommand)]
        command: OverrideCommand,
    },
    /// Server-only content into the world database.
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
    /// The world database's battle NPC spawns as the repository's seed file (what a new server
    /// starts with).
    Export {
        #[arg(default_value = "data/672261/bnpc_spawn.csv")]
        file: PathBuf,
        /// Only the spawns from this source (`sapphire`, `hand`, ...): the repository's seed is
        /// made with `--source sapphire`, leaving out what was placed by hand while testing.
        #[arg(long)]
        source: Option<String>,
    },
    /// Finds world database content that does not belong in this client build (territories,
    /// sheets, rows or layout layers it lacks), and with --apply removes it.
    Prune {
        #[arg(long)]
        apply: bool,
    },
    /// Reads files out of the client's SqPack by path (`game_script/quest/000/ManFst000_00083.luab`),
    /// writing each to `--out` (default: the current folder) under its file name, or reports
    /// which of the paths the install lacks.
    File {
        paths: Vec<String>,
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// A territory's layout files: each layer's objects counted by type, with a few instance ids
    /// and positions per type (`--kind` lists every object of that type).
    Layout {
        territory: u16,
        #[arg(long)]
        kind: Option<u32>,
    },
    /// A territory's collision (terrain and background parts): triangles, bounds, time.
    Collision {
        territory: u16,
    },
    /// Any of the client's scripts (`game_script/transport/Aetheryte.luab`): its functions, what
    /// each calls and the numbers it returns.
    Script {
        path: String,
    },
    /// A quest as the β has it: its row's listeners, to-dos and rewards, its script's scenes and
    /// what each calls; `--stats` tallies what every quest script calls.
    Quest {
        /// The quest (`0x10027` or `65575`).
        #[arg(value_parser = parse_id)]
        id: Option<u32>,
        #[arg(long)]
        stats: bool,
    },
    /// How much of the β the server serves, domain by domain (quests, items, shops, NPCs,
    /// combat, social systems, protocol...), linked to the client's data, as an HTML report.
    Coverage {
        #[arg(long, default_value = "var/coverage.html")]
        out: PathBuf,
    },
    /// Runs headless clients against the zone service until Ctrl+C.
    Bots {
        #[arg(long, short = 'n', default_value_t = 10)]
        count: u32,
        /// The territory they walk in.
        #[arg(long, default_value_t = 129)]
        territory: u16,
        /// The middle of their circles, x,y,z (Limsa Lominsa's start by default).
        #[arg(long, default_value = "-62,18,-4", value_parser = parse_point, allow_hyphen_values = true)]
        at: [f32; 3],
        /// How far from the middle they walk (yalms; each bot gets its own circle within this).
        #[arg(long, default_value_t = 15.0)]
        radius: f32,
        /// The character whose look they copy (the first stored character when not given).
        #[arg(long)]
        look: Option<String>,
        /// Seconds between each bot's "say" lines (0: silent).
        #[arg(long, default_value_t = 30)]
        chat_every: u64,
        /// Position updates per second, per bot.
        #[arg(long, default_value_t = 4.0)]
        rate: f32,
    },
}

#[derive(Subcommand)]
enum OverrideCommand {
    /// Lists the overrides.
    List,
    /// Sets column `col` of `row` in `sheet` to `value` (adding the row if the sheet lacks it).
    Set {
        sheet: String,
        row: u32,
        col: u32,
        value: String,
        #[arg(long)]
        comment: Option<String>,
    },
    /// Removes one.
    Remove { sheet: String, row: u32, col: u32 },
}

#[derive(Subcommand)]
enum ImportCommand {
    /// Sapphire's `battlenpc` dump (`sql/migrations/20211217125038_UpdateBattleNpc.sql`), checked
    /// against this build; replaces earlier imports from Sapphire.
    SapphireBnpcs { file: PathBuf },
    /// Sapphire's quest scripts (`src/scripts/quest`) as quest definitions, for the quests the β
    /// has (tagged `sapphire`; what the format cannot say goes to each one's `review`).
    SapphireQuests {
        dir: PathBuf,
        #[arg(long, default_value = "data/672261/quests")]
        out: PathBuf,
    },
    /// Quest definitions derived from the β's own data (offer and turn-in), for every quest that
    /// has none yet (tagged `derived`).
    DeriveQuests {
        #[arg(long, default_value = "data/672261/quests")]
        out: PathBuf,
    },
    /// Places the β's gathering points from Teamcraft's `nodes.json` (each base's map
    /// coordinates; each point within it a guess, tagged so).
    GatheringNodes {
        nodes: PathBuf,
        #[arg(long, default_value = "data/672261/gathering.json")]
        out: PathBuf,
    },
}

/// The game data, with the world database's overrides unless `raw`.
async fn game_data(config: &Config, db: Option<&Databases>, raw: bool) -> anyhow::Result<WorldData> {
    let game = config.data.game.as_ref().context("no game data: set [data] game or CRYSTAL_DATA__GAME")?;
    let mut overrides = Overrides::default();
    if let (Some(db), false) = (db, raw) {
        for o in db.world.exd_overrides(BETA_PATCH).await? {
            overrides.add(&o.sheet, o.row_id, o.col as usize, o.value);
        }
    }
    Ok(WorldData::open_with(game, overrides)?)
}

async fn databases(config: &Config) -> anyhow::Result<Databases> {
    let db = Databases::connect(&config.database).await.context("databases")?;
    db.migrate().await?;
    Ok(db)
}

fn parse_id(s: &str) -> Result<u32, String> {
    match s.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => s.parse(),
    }
    .map_err(|e| format!("{s}: {e}"))
}

fn parse_point(s: &str) -> Result<[f32; 3], String> {
    let v: Vec<f32> =
        s.split(',').map(|p| p.trim().parse().map_err(|_| format!("bad number in {s}"))).collect::<Result<_, _>>()?;
    v.try_into().map_err(|_| "x,y,z".to_string())
}

#[derive(Default)]
struct Stats {
    online: AtomicU64,
    sent: AtomicU64,
    spawns: AtomicU64,
    despawns: AtomicU64,
    moves: AtomicU64,
    chats: AtomicU64,
    rtt_us: AtomicU64,
    rtt_n: AtomicU64,
    rtt_max_us: AtomicU64,
    errors: AtomicU64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    crystal_common::log::init(&config.log.filter);
    match args.command {
        Command::Exd { command } => {
            let db = databases(&config).await?;
            let data = game_data(&config, Some(&db), false).await?;
            db.close().await;
            exd::run(&data, command)?;
        }
        Command::Override { command } => {
            let db = databases(&config).await?;
            match command {
                OverrideCommand::List => {
                    for o in db.world.exd_overrides(BETA_PATCH).await? {
                        let note = o.comment.map(|c| format!("  # {c}")).unwrap_or_default();
                        println!("{} {} {} = {:?}{note}", o.sheet, o.row_id, o.col, o.value);
                    }
                }
                OverrideCommand::Set { sheet, row, col, value, comment } => {
                    // checked against the client's sheet: the column must exist and take the value
                    let data = game_data(&config, None, true).await?;
                    let mut s = (*data.sheet(&sheet)?).clone();
                    if !s.set(row, col as usize, &value) {
                        anyhow::bail!("{sheet} column {col} does not take {value:?}");
                    }
                    let o = ExdOverride { sheet: s.name.clone(), row_id: row, col, value, comment };
                    db.world.set_exd_override(BETA_PATCH, &o).await?;
                    println!("{} {row} {col} = {:?} (the zone service applies it when it starts)", o.sheet, o.value);
                }
                OverrideCommand::Remove { sheet, row, col } => {
                    if !db.world.remove_exd_override(BETA_PATCH, &sheet, row, col).await? {
                        anyhow::bail!("no override {sheet} {row} {col}");
                    }
                }
            }
            db.close().await;
        }
        Command::Import { command } => {
            let db = databases(&config).await?;
            let data = game_data(&config, Some(&db), false).await?;
            match command {
                ImportCommand::SapphireBnpcs { file } => import::run_sapphire_bnpcs(&db, &data, &file).await?,
                ImportCommand::SapphireQuests { dir, out } => {
                    let scripts: std::collections::HashMap<u32, String> = data
                        .sheet("Quest")?
                        .rows
                        .iter()
                        .filter_map(|(id, r)| {
                            let s = r.get(1)?.as_str()?;
                            (!s.is_empty()).then(|| (*id, s.to_owned()))
                        })
                        .collect();
                    let written = sapphire_quests::run(&dir, &out, &scripts)?;
                    println!("{} quest definitions written to {}", written.len(), out.display());
                }
                ImportCommand::DeriveQuests { out } => {
                    let written = derive_quests::run(&data, &out)?;
                    println!("{} derived quest definitions written to {}", written.len(), out.display());
                }
                ImportCommand::GatheringNodes { nodes, out } => {
                    gathering_nodes::run(&data, &db, &nodes, &out).await?;
                }
            }
            db.close().await;
        }
        Command::Export { file, source } => {
            let db = databases(&config).await?;
            let mut spawns = db.world.bnpc_spawns(BETA_PATCH, None).await?;
            if let Some(source) = &source {
                spawns.retain(|s| &s.source == source);
            }
            crystal_db::seed::write_bnpcs(&file, &spawns)?;
            println!("{} spawns written to {}", spawns.len(), file.display());
            db.close().await;
        }
        Command::Prune { apply } => {
            let db = databases(&config).await?;
            let data = game_data(&config, None, true).await?;
            prune::run(&db, &data, apply).await?;
            db.close().await;
        }
        Command::Quest { id, stats } => {
            let data = game_data(&config, None, true).await?;
            quest::run(&data, id, stats)?;
        }
        Command::Coverage { out } => {
            let db = databases(&config).await?;
            let data = game_data(&config, Some(&db), false).await?;
            coverage::run(&data, &db, &out).await?;
            db.close().await;
        }
        Command::Collision { territory } => {
            let data = game_data(&config, None, true).await?;
            let started = Instant::now();
            let tris = data.collision(territory)?;
            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            for v in tris.iter().flatten() {
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
            println!("{} triangles in {:?}; bounds {lo:?} .. {hi:?}", tris.len(), started.elapsed());
            let started = Instant::now();
            let grid = crystal_world::nav::NavGrid::build(&tris);
            println!("walkable grid built in {:?}", started.elapsed());
            // the battle NPCs' recorded heights against the grid's ground under them
            let db = databases(&config).await?;
            let spawns = db.world.bnpc_spawns(crystal_db::BETA_PATCH, Some(territory)).await?;
            let mut diffs: Vec<f32> = Vec::new();
            let mut missing = 0;
            for s in &spawns {
                match grid.ground(s.pos, 6.0) {
                    Some(y) => diffs.push((y - s.pos[1]).abs()),
                    None => missing += 1,
                }
            }
            diffs.sort_by(f32::total_cmp);
            let pct = |p: f32| diffs.get(((diffs.len() as f32 - 1.0) * p) as usize).copied().unwrap_or(f32::NAN);
            println!(
                "{} spawns: ground within 6 yalms for {} (|dy| median {:.2}, 90% {:.2}, max {:.2}), none for {missing}",
                spawns.len(),
                diffs.len(),
                pct(0.5),
                pct(0.9),
                pct(1.0)
            );
            // a path between the first two spawns that have ground
            let walk: Vec<_> = spawns.iter().filter(|s| grid.ground(s.pos, 6.0).is_some()).take(40).collect();
            if let (Some(a), Some(b)) = (walk.first(), walk.last()) {
                let started = Instant::now();
                let path = grid.path(a.pos, b.pos);
                println!(
                    "path {:?} -> {:?}: {:?} in {:?}",
                    a.pos,
                    b.pos,
                    path.as_ref().map(|p| (p.len(), p.last().copied())),
                    started.elapsed()
                );
            }
            db.close().await;
        }
        Command::Script { path } => {
            let data = game_data(&config, None, true).await?;
            let script = crystal_data::luac::Script::parse(&data.pack().read(&path)?)?;
            for (name, f) in &script.functions {
                println!("{name}: returns {:?}", f.returns);
                println!("    {}", f.uses.join(" "));
            }
        }
        Command::Layout { territory, kind } => {
            let data = game_data(&config, None, true).await?;
            for (file, layout) in data.layout_files(territory)? {
                for layer in &layout.layers {
                    let mut by_kind: std::collections::BTreeMap<u32, Vec<&crystal_data::Object>> = Default::default();
                    for o in &layer.objects {
                        by_kind.entry(o.kind).or_default().push(o);
                    }
                    let counts: Vec<String> = by_kind.iter().map(|(k, v)| format!("{k}×{}", v.len())).collect();
                    println!("{file} layer {} \"{}\": {}", layer.id, layer.name, counts.join(" "));
                    for (k, objects) in &by_kind {
                        let all = kind == Some(*k);
                        for o in objects.iter().take(if all { usize::MAX } else { 2 }) {
                            println!(
                                "    type {k} id {} \"{}\" at ({:.1}, {:.1}, {:.1})",
                                o.id, o.name, o.pos[0], o.pos[1], o.pos[2]
                            );
                            if all {
                                let bytes: String = o.data().iter().take(0x30).map(|b| format!("{b:02x}")).collect();
                                println!("      data {bytes}");
                            }
                        }
                    }
                }
            }
        }
        Command::File { paths, out } => {
            let data = game_data(&config, None, true).await?;
            std::fs::create_dir_all(&out)?;
            for path in &paths {
                match data.pack().read(path) {
                    Ok(bytes) => {
                        let name = path.rsplit('/').next().unwrap_or(path);
                        std::fs::write(out.join(name), &bytes)?;
                        println!("{path}: {} bytes", bytes.len());
                    }
                    Err(e) => println!("{path}: {e}"),
                }
            }
        }
        Command::Bots { count, territory, at, radius, look, chat_every, rate } => {
            let db = Databases::connect(&config.database).await.context("databases")?;
            db.migrate().await?;
            let actors = bot_characters(&db, count, look.as_deref(), territory, at).await?;
            db.close().await;
            let addr = config.zone.bind;
            info!(count, %addr, territory, "starting bots");
            let stats = Arc::new(Stats::default());
            let started = Instant::now();
            for (i, actor) in actors.into_iter().enumerate() {
                let stats = stats.clone();
                let r = radius * (0.3 + 0.7 * ((i * 37 % 100) as f32 / 100.0));
                tokio::spawn(async move {
                    // a little apart, so a thousand do not arrive at once
                    tokio::time::sleep(Duration::from_millis(20 * i as u64)).await;
                    if let Err(e) = run_bot(addr, actor, i, at, r, rate, chat_every, &stats).await {
                        warn!(bot = i + 1, "{e:#}");
                        stats.errors.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
            let mut report = tokio::time::interval(Duration::from_secs(5));
            report.tick().await;
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    _ = report.tick() => print_stats(&stats, started),
                }
            }
            print_stats(&stats, started);
        }
    }
    Ok(())
}

fn print_stats(s: &Stats, started: Instant) {
    let secs = started.elapsed().as_secs_f64().max(1.0);
    let n = s.rtt_n.load(Ordering::Relaxed).max(1);
    info!(
        online = s.online.load(Ordering::Relaxed),
        errors = s.errors.load(Ordering::Relaxed),
        sent_per_s = format_args!("{:.0}", s.sent.load(Ordering::Relaxed) as f64 / secs),
        moves_per_s = format_args!("{:.0}", s.moves.load(Ordering::Relaxed) as f64 / secs),
        spawns = s.spawns.load(Ordering::Relaxed),
        despawns = s.despawns.load(Ordering::Relaxed),
        chats = s.chats.load(Ordering::Relaxed),
        rtt_ms = format_args!("{:.2}", s.rtt_us.load(Ordering::Relaxed) as f64 / n as f64 / 1000.0),
        rtt_max_ms = format_args!("{:.2}", s.rtt_max_us.load(Ordering::Relaxed) as f64 / 1000.0),
        "bots"
    );
}

/// The bots' actor ids: their characters made if missing, and all placed at `at`.
async fn bot_characters(
    db: &Databases,
    count: u32,
    look: Option<&str>,
    territory: u16,
    at: [f32; 3],
) -> anyhow::Result<Vec<u32>> {
    let account = db.login.ensure_account(BOT_ACCOUNT, now()).await?;
    let source = match look {
        Some(name) => {
            db.characters.character_by_name(name).await?.with_context(|| format!("no character named {name}"))?
        }
        None => db
            .characters
            .character_by_id(1)
            .await?
            .context("no stored character to copy a look from: make one in the client, or give --look")?,
    };
    let mut next_content = db.characters.max_content_id().await? + 1;
    let mut actors = Vec::new();
    for i in 1..=count {
        let name = format!("Bot {i:03}");
        let chara = match db.characters.character_by_name(&name).await? {
            Some(c) => c,
            None => {
                db.characters
                    .create_character(account, next_content, source.world_id, &name, &source.appearance, now())
                    .await?;
                next_content += 1;
                db.characters.character_by_name(&name).await?.context("the bot just made")?
            }
        };
        let place =
            Position { zone_id: territory as u32, x: at[0] as f64, y: at[1] as f64, z: at[2] as f64, rotation: 0.0 };
        db.characters.save_position(chara.content_id, place).await?;
        actors.push(PLAYER_ACTOR_BASE + chara.id as u32);
    }
    Ok(actors)
}

#[allow(clippy::too_many_arguments)]
async fn run_bot(
    addr: std::net::SocketAddr,
    actor: u32,
    index: usize,
    at: [f32; 3],
    radius: f32,
    rate: f32,
    chat_every: u64,
    stats: &Stats,
) -> anyhow::Result<()> {
    let mut bot = Bot::connect(addr, actor).await?;
    bot.enter().await?;
    // counted online from here until this returns, however it returns
    struct Online<'a>(&'a AtomicU64);
    impl Drop for Online<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }
    stats.online.fetch_add(1, Ordering::Relaxed);
    let _online = Online(&stats.online);
    let started = Instant::now();
    let mut step = tokio::time::interval(Duration::from_secs_f32(1.0 / rate.max(0.1)));
    let mut ping = tokio::time::interval(Duration::from_secs(5));
    let chat_period = Duration::from_secs(chat_every.max(1));
    let mut talk = tokio::time::interval_at(tokio::time::Instant::now() + chat_period, chat_period);
    let phase = index as f32 * 2.399; // the golden angle spreads them round the circle
    let speed = 6.0 / radius.max(1.0); // about running speed, in radians per second
    loop {
        tokio::select! {
            _ = step.tick() => {
                let a = phase + started.elapsed().as_secs_f32() * speed;
                let pos = [at[0] + radius * a.cos(), at[1], at[2] + radius * a.sin()];
                // facing along the circle: atan2 of the heading's x over its z
                let rotation = (-a.sin()).atan2(a.cos());
                bot.move_to(pos, rotation).await?;
                stats.sent.fetch_add(1, Ordering::Relaxed);
            }
            _ = ping.tick() => {
                bot.ping(started.elapsed().as_millis() as u32).await?;
                stats.sent.fetch_add(1, Ordering::Relaxed);
            }
            _ = talk.tick(), if chat_every > 0 => {
                bot.chat(chat::SAY, &format!("Bot {:03} walking by", index + 1)).await?;
                stats.sent.fetch_add(1, Ordering::Relaxed);
            }
            down = bot.recv() => match down? {
                Down::Spawn { .. } => { stats.spawns.fetch_add(1, Ordering::Relaxed); }
                Down::Despawn { .. } => { stats.despawns.fetch_add(1, Ordering::Relaxed); }
                Down::Move { .. } => { stats.moves.fetch_add(1, Ordering::Relaxed); }
                Down::Chat(_) => { stats.chats.fetch_add(1, Ordering::Relaxed); }
                Down::Pong(sent_ms) => {
                    let rtt = (started.elapsed().as_micros() as u64).saturating_sub(sent_ms as u64 * 1000);
                    stats.rtt_us.fetch_add(rtt, Ordering::Relaxed);
                    stats.rtt_n.fetch_add(1, Ordering::Relaxed);
                    stats.rtt_max_us.fetch_max(rtt, Ordering::Relaxed);
                }
                _ => {}
            },
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
