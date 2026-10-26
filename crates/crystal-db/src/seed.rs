//! The world's starting content, shipped in the repository (`data/<build>/`) so a new server
//! has a populated world without importing anything: CSV files, one per table, written by
//! `crystal-tools export` and loaded when the server starts with the table empty.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::world::BnpcSpawn;
use crate::{Database, DbError, Result};

/// The battle NPC spawns' file in a build's seed folder.
pub const BNPC_SPAWNS: &str = "bnpc_spawn.csv";

/// A `bnpc_spawn` row as the seed file has it (flat, for CSV).
#[derive(Debug, Serialize, Deserialize)]
struct BnpcRow {
    territory: u16,
    instance_id: Option<u32>,
    layer: String,
    base_id: u32,
    name_id: u32,
    level: u16,
    x: f32,
    y: f32,
    z: f32,
    rotation: f32,
    wander_range: f32,
    respawn_secs: u32,
    pop_weather: u32,
    pop_time_start: u32,
    pop_time_end: u32,
    loot_table: u32,
    link_group: u32,
    link_family: u32,
    link_range: u32,
    normal_ai: u32,
    /// Seeds from before it was kept are passive.
    #[serde(default)]
    aggressive: bool,
    source: String,
}

impl From<&BnpcSpawn> for BnpcRow {
    fn from(s: &BnpcSpawn) -> Self {
        Self {
            territory: s.territory,
            instance_id: s.instance_id,
            layer: s.layer.clone(),
            base_id: s.base_id,
            name_id: s.name_id,
            level: s.level,
            x: s.pos[0],
            y: s.pos[1],
            z: s.pos[2],
            rotation: s.rotation,
            wander_range: s.wander_range,
            respawn_secs: s.respawn_secs,
            pop_weather: s.pop_weather,
            pop_time_start: s.pop_time_start,
            pop_time_end: s.pop_time_end,
            loot_table: s.loot_table,
            link_group: s.link_group,
            link_family: s.link_family,
            link_range: s.link_range,
            normal_ai: s.normal_ai,
            aggressive: s.aggressive,
            source: s.source.clone(),
        }
    }
}

impl From<BnpcRow> for BnpcSpawn {
    fn from(r: BnpcRow) -> Self {
        Self {
            territory: r.territory,
            instance_id: r.instance_id,
            layer: r.layer,
            base_id: r.base_id,
            name_id: r.name_id,
            level: r.level,
            pos: [r.x, r.y, r.z],
            rotation: r.rotation,
            wander_range: r.wander_range,
            respawn_secs: r.respawn_secs,
            pop_weather: r.pop_weather,
            pop_time_start: r.pop_time_start,
            pop_time_end: r.pop_time_end,
            loot_table: r.loot_table,
            link_group: r.link_group,
            link_family: r.link_family,
            link_range: r.link_range,
            normal_ai: r.normal_ai,
            aggressive: r.aggressive,
            source: r.source,
        }
    }
}

fn seed_error(path: &Path, what: impl std::fmt::Display) -> DbError {
    DbError::Seed { path: path.display().to_string(), what: what.to_string() }
}

pub fn write_bnpcs(path: &Path, spawns: &[BnpcSpawn]) -> Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| seed_error(path, e))?;
    }
    let mut w = csv::Writer::from_path(path).map_err(|e| seed_error(path, e))?;
    for s in spawns {
        w.serialize(BnpcRow::from(s)).map_err(|e| seed_error(path, e))?;
    }
    w.flush().map_err(|e| seed_error(path, e))
}

pub fn read_bnpcs(path: &Path) -> Result<Vec<BnpcSpawn>> {
    let mut r = csv::Reader::from_path(path).map_err(|e| seed_error(path, e))?;
    r.deserialize::<BnpcRow>().map(|row| row.map(BnpcSpawn::from).map_err(|e| seed_error(path, e))).collect()
}

/// What [`seed_world`] loaded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Seeded {
    pub bnpc_spawns: usize,
}

/// Loads the seed files of `dir` (a build's folder, `data/672261`) into the tables of `world`
/// that are empty for `patch`. Missing files are skipped.
pub async fn seed_world(world: &Database, dir: &Path, patch: &str) -> Result<Seeded> {
    let mut seeded = Seeded::default();
    let path: PathBuf = dir.join(BNPC_SPAWNS);
    if path.exists() && world.bnpc_spawn_counts(patch).await?.is_empty() {
        let spawns = read_bnpcs(&path)?;
        // one source per call: group them (the file is ordered, so few groups)
        let mut sources: Vec<&str> = spawns.iter().map(|s| s.source.as_str()).collect();
        sources.sort();
        sources.dedup();
        for source in sources {
            let of: Vec<BnpcSpawn> = spawns.iter().filter(|s| s.source == source).cloned().collect();
            world.replace_bnpc_spawns(patch, source, &of).await?;
        }
        info!(path = %path.display(), spawns = spawns.len(), "seeded battle NPC spawns");
        seeded.bnpc_spawns = spawns.len();
    }
    Ok(seeded)
}
