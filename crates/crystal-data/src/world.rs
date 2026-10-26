//! What the zone service asks of the game data: where a territory's layout is, and where a zone
//! exit leads.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use tracing::debug;

use crate::lgb::Layout;
use crate::sqpack::SqPack;
use crate::{DataError, Result};

/// The layout files of a territory that hold the objects the server uses.
const LAYOUT_FILES: [&str; 3] = ["planmap.lgb", "planevent.lgb", "planner.lgb"];

/// Where a zone exit puts the player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arrival {
    pub territory: u16,
    pub pos: [f32; 3],
    /// Radians.
    pub rotation: f32,
}

/// An event NPC placed in a territory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NpcPlacement {
    /// `ENpcBase` / `ENpcResident` row: the client builds the NPC's look from it.
    pub enpc: u32,
    pub pos: [f32; 3],
    /// Radians.
    pub rotation: f32,
}

/// `Level` columns: x, y, z, yaw @0-3, type @5, object @6, territory @9. Type 8 is an event NPC.
const LEVEL_EVENT_NPC: u64 = 8;

pub struct WorldData {
    pack: SqPack,
    /// `TerritoryType` row → its layout folder (`bg/<column 1 without its last part>`).
    territories: HashMap<u32, String>,
    layouts: Mutex<HashMap<u16, Arc<Layout>>>,
    /// Event NPCs by territory, from the `Level` sheet; read on first use.
    npcs: Mutex<Option<HashMap<u16, Vec<NpcPlacement>>>>,
}

impl WorldData {
    /// `game` is the client's `game` folder.
    pub fn open(game: &Path) -> Result<Self> {
        let pack = SqPack::open(game)?;
        let sheet = pack.sheet("TerritoryType")?;
        let territories = sheet
            .rows
            .iter()
            .filter_map(|(id, v)| {
                // "ffxiv/sea_s1/twn/s1t2/level/s1t2" → "bg/ffxiv/sea_s1/twn/s1t2/level"
                let bg = v.get(1)?.as_str()?;
                let (folder, _) = bg.rsplit_once('/')?;
                (!folder.is_empty()).then(|| (*id, format!("bg/{folder}")))
            })
            .collect();
        Ok(Self { pack, territories, layouts: Mutex::new(HashMap::new()), npcs: Mutex::new(None) })
    }

    /// The event NPCs the `Level` sheet places in `territory`.
    pub fn npcs(&self, territory: u16) -> Result<Vec<NpcPlacement>> {
        let mut npcs = self.npcs.lock();
        if npcs.is_none() {
            let level = self.pack.sheet("Level")?;
            let mut by_territory: HashMap<u16, Vec<NpcPlacement>> = HashMap::new();
            for (_, v) in &level.rows {
                let f = |i: usize| v.get(i).and_then(|x| x.as_f32()).unwrap_or(0.0);
                let u = |i: usize| v.get(i).and_then(|x| x.as_u64()).unwrap_or(0);
                if u(5) == LEVEL_EVENT_NPC && u(6) != 0 {
                    by_territory.entry(u(9) as u16).or_default().push(NpcPlacement {
                        enpc: u(6) as u32,
                        pos: [f(0), f(1), f(2)],
                        rotation: f(3),
                    });
                }
            }
            *npcs = Some(by_territory);
        }
        Ok(npcs.as_ref().unwrap().get(&territory).cloned().unwrap_or_default())
    }

    pub fn pack(&self) -> &SqPack {
        &self.pack
    }

    /// A territory's layout objects (its planmap, planevent and planner files together).
    pub fn layout(&self, territory: u16) -> Result<Arc<Layout>> {
        if let Some(l) = self.layouts.lock().get(&territory) {
            return Ok(l.clone());
        }
        let folder = self
            .territories
            .get(&(territory as u32))
            .ok_or_else(|| DataError::NotFound(format!("TerritoryType {territory}")))?;
        let mut layout = Layout::default();
        for file in LAYOUT_FILES {
            let path = format!("{folder}/{file}");
            match self.pack.read(&path) {
                Ok(d) => layout.layers.extend(Layout::parse(&path, &d)?.layers),
                Err(DataError::NotFound(_)) => debug!(%path, "no such layout file"),
                Err(e) => return Err(e),
            }
        }
        let layout = Arc::new(layout);
        self.layouts.lock().insert(territory, layout.clone());
        Ok(layout)
    }

    /// Where exit `exit` (an exit range's instance id) in `territory` leads: the destination
    /// territory and the pop range there it names.
    pub fn zone_line(&self, territory: u16, exit: u32) -> Result<Arrival> {
        let here = self.layout(territory)?;
        let range = here
            .find(exit)
            .and_then(|o| o.exit_range())
            .ok_or_else(|| DataError::NotFound(format!("exit range {exit} in territory {territory}")))?;
        let there = self.layout(range.territory)?;
        let pop = there.find(range.destination).ok_or_else(|| {
            DataError::NotFound(format!("pop range {} in territory {}", range.destination, range.territory))
        })?;
        Ok(Arrival { territory: range.territory, pos: pop.pos, rotation: pop.rot[1] })
    }
}
