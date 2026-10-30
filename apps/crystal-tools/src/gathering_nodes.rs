//! `crystal-tools import gathering-nodes <nodes.json>`: places the β's gathering points from
//! Teamcraft's node data (today's client: per `GatheringPointBase`, its map, map coordinates and
//! radius), as `data/672261/gathering.json`.
//!
//! What is fact and what is not, per point:
//! - which points exist, their base and (when set) territory: the β's `GatheringPoint` sheet;
//! - the base's area: Teamcraft's map coordinates, turned into world ones with the β's `Map` row
//!   (col 6 its size factor in hundreds, col 12 its territory; no offsets in the β's sheet):
//!   `world = ((map - 1) * c / 41 * 2048 - 1024) / c`. Checked on Central Shroud's aetheryte:
//!   layout (13.1, 35.9) is map (21.8, 22.2), the 2.x maps' (22, 21);
//! - each point's place in that area: **a guess**, the base's points spread evenly on a circle of
//!   the base's radius around its centre;
//! - height: **a guess**, the nearest battle NPC spawn's (the world database's).
//!
//! Every placement says so in its `source`; play corrects them.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Context;
use crystal_common::gathering::{Placement, Placements};
use crystal_data::WorldData;
use crystal_db::{BETA_PATCH, Databases};

/// What the importer reads of a Teamcraft node.
struct Node {
    x: Option<f32>,
    y: Option<f32>,
    map: Option<u32>,
    radius: Option<f32>,
}

pub async fn run(data: &WorldData, db: &Databases, nodes: &Path, out: &Path) -> anyhow::Result<usize> {
    let text = std::fs::read_to_string(nodes).with_context(|| nodes.display().to_string())?;
    let json: HashMap<String, serde_json::Value> = serde_json::from_str(&text).context("Teamcraft nodes.json")?;
    let nodes: HashMap<String, Node> = json
        .into_iter()
        .map(|(k, v)| {
            let f = |key: &str| v.get(key).and_then(|x| x.as_f64()).map(|x| x as f32);
            let node = Node { x: f("x"), y: f("y"), map: f("map").map(|m| m as u32), radius: f("radius") };
            (k, node)
        })
        .collect();
    let points = data.sheet("GatheringPoint")?;
    let maps = data.sheet("Map")?;
    let mut heights: HashMap<u16, Vec<[f32; 3]>> = HashMap::new();
    for s in db.world.bnpc_spawns(BETA_PATCH, None).await? {
        heights.entry(s.territory).or_default().push(s.pos);
    }

    // the β's points by base
    let mut by_base: BTreeMap<u32, Vec<(u32, u64)>> = BTreeMap::new();
    for (id, row) in points.rows.iter().map(|(id, row)| (*id, row)) {
        let base = row.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let territory = row.get(7).and_then(|v| v.as_u64()).unwrap_or(0);
        if base != 0 {
            by_base.entry(base).or_default().push((id, territory));
        }
    }

    let (mut placed, mut no_node, mut mismatch) = (Vec::new(), 0, 0);
    for (base, pts) in &by_base {
        let Some(n) = nodes.get(&base.to_string()) else {
            no_node += pts.len();
            continue;
        };
        // map 0: Teamcraft has no location for the base
        let (Some(mx), Some(my), Some(map @ 1..)) = (n.x, n.y, n.map) else {
            no_node += pts.len();
            continue;
        };
        let Some(map_row) = maps.row(map) else {
            no_node += pts.len();
            continue;
        };
        let c = map_row.get(6).and_then(|v| v.as_u64()).unwrap_or(1).max(1) as f32;
        let territory = map_row.get(12).and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        let to_world = |m: f32| ((m - 1.0) * c / 41.0 * 2048.0 - 1024.0) / c;
        let centre = [to_world(mx), to_world(my)];
        let radius = n.radius.unwrap_or(0.0).clamp(0.0, 60.0);
        for (k, &(point, point_territory)) in pts.iter().enumerate() {
            // the β's own territory for the point, when it has one (1 is unset), must agree
            if point_territory > 1 && point_territory != territory as u64 {
                mismatch += 1;
                continue;
            }
            let angle = std::f32::consts::TAU * k as f32 / pts.len() as f32;
            let (x, z) = (centre[0] + radius * angle.cos(), centre[1] + radius * angle.sin());
            let y = heights
                .get(&territory)
                .and_then(|spawns| {
                    spawns.iter().min_by(|a, b| {
                        let d = |p: &&[f32; 3]| (p[0] - x).powi(2) + (p[2] - z).powi(2);
                        d(a).total_cmp(&d(b))
                    })
                })
                .map_or(0.0, |p| p[1]);
            placed.push(Placement {
                point,
                base: *base,
                territory,
                pos: [x, y, z],
                rotation: 0.0,
                source: "teamcraft-area+guess".into(),
            });
        }
    }
    placed.sort_by_key(|p| p.point);
    let file = Placements {
        notes: vec![
            "Made by `crystal-tools import gathering-nodes` from Teamcraft's nodes.json (today's client).".into(),
            "Fact: the points and their bases (the β's GatheringPoint); the base's area (Teamcraft's map x/y and map, through the β's Map row: size factor col 6, territory col 12).".into(),
            "Guess: each point on a circle of the base's radius around the area's centre; its height from the nearest battle NPC spawn.".into(),
        ],
        points: placed,
    };
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, serde_json::to_string_pretty(&file)? + "\n")?;
    println!(
        "{} points placed ({} without a Teamcraft area, {} whose β territory disagrees) -> {}",
        file.points.len(),
        no_node,
        mismatch,
        out.display()
    );
    Ok(file.points.len())
}
