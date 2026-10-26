//! What the zone service asks of the game data: where a territory's layout is, and where a zone
//! exit leads.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use tracing::{debug, warn};

use crate::exd::Sheet;
use crate::lgb::Layout;
use crate::sqpack::SqPack;
use crate::{DataError, Result};

/// The layout files of a territory that hold the objects the server uses.
const LAYOUT_FILES: [&str; 3] = ["planmap.lgb", "planevent.lgb", "planner.lgb"];
/// Every layout file a territory's level folder has, for its layer names.
const ALL_LAYOUT_FILES: [&str; 7] =
    ["planmap.lgb", "planevent.lgb", "planner.lgb", "planlive.lgb", "bg.lgb", "sound.lgb", "vfx.lgb"];

/// Why content does not belong in this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Misfit {
    Territory,
    BNpcBase,
    BNpcName,
    /// Its layout layer is not in the territory's layouts: added by a later patch.
    Layer,
}

impl std::fmt::Display for Misfit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Misfit::Territory => "territory not in this build",
            Misfit::BNpcBase => "no BNpcBase row",
            Misfit::BNpcName => "no BNpcName row",
            Misfit::Layer => "layout layer not in this build",
        })
    }
}

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

/// Changes to EXD rows the server makes over the client's data (`world.exd_override`): column
/// values as text, read as each column's type. A row the sheet does not have is added, its other
/// columns zero.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    by_sheet: HashMap<String, Vec<(u32, usize, String)>>,
}

impl Overrides {
    pub fn add(&mut self, sheet: &str, row: u32, column: usize, value: impl Into<String>) {
        self.by_sheet.entry(sheet.to_owned()).or_default().push((row, column, value.into()));
    }

    pub fn len(&self) -> usize {
        self.by_sheet.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Applies the overrides of `sheet.name`; the ones that do not fit (no such column, a value
    /// not of its type) are returned.
    pub fn apply(&self, sheet: &mut Sheet) -> Vec<(u32, usize, String)> {
        let mut rejected = Vec::new();
        for (row, col, value) in self.by_sheet.get(&sheet.name).into_iter().flatten() {
            if !sheet.set(*row, *col, value) {
                rejected.push((*row, *col, value.clone()));
            }
        }
        rejected
    }
}

pub struct WorldData {
    pack: SqPack,
    overrides: Overrides,
    /// Sheets read so far, overrides applied.
    sheets: Mutex<HashMap<String, Arc<Sheet>>>,
    /// `TerritoryType` row → its layout folder (`bg/<column 1 without its last part>`).
    territories: HashMap<u32, String>,
    layouts: Mutex<HashMap<u16, Arc<Layout>>>,
    /// Event NPCs by territory, from the `Level` sheet; read on first use.
    npcs: Mutex<Option<HashMap<u16, Vec<NpcPlacement>>>>,
    /// Every layer name of a territory's layouts, by territory; read on first use.
    layer_names: Mutex<HashMap<u16, Arc<HashSet<String>>>>,
}

impl WorldData {
    /// `game` is the client's `game` folder.
    pub fn open(game: &Path) -> Result<Self> {
        Self::open_with(game, Overrides::default())
    }

    /// As [`Self::open`], with the server's overrides of the client's rows.
    pub fn open_with(game: &Path, overrides: Overrides) -> Result<Self> {
        let pack = SqPack::open(game)?;
        let mut data = Self {
            pack,
            overrides,
            sheets: Mutex::new(HashMap::new()),
            territories: HashMap::new(),
            layouts: Mutex::new(HashMap::new()),
            npcs: Mutex::new(None),
            layer_names: Mutex::new(HashMap::new()),
        };
        let sheet = data.sheet("TerritoryType")?;
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
        data.territories = territories;
        Ok(data)
    }

    /// A sheet (English where it has languages), with the overrides applied. Read once.
    pub fn sheet(&self, name: &str) -> Result<Arc<Sheet>> {
        if let Some(s) = self.sheets.lock().get(name) {
            return Ok(s.clone());
        }
        let mut sheet = self.pack.sheet(name)?;
        for (row, col, value) in self.overrides.apply(&mut sheet) {
            warn!(sheet = name, row, col, value, "override does not fit the sheet: ignored");
        }
        let sheet = Arc::new(sheet);
        self.sheets.lock().insert(name.to_owned(), sheet.clone());
        Ok(sheet)
    }

    /// The `TerritoryType` row whose name (column 0, `s1f1`) is `name`.
    pub fn territory_by_name(&self, name: &str) -> Option<u16> {
        let sheet = self.sheet("TerritoryType").ok()?;
        sheet.rows.iter().find(|(_, v)| v.first().and_then(|n| n.as_str()) == Some(name)).map(|(id, _)| *id as u16)
    }

    /// The event NPCs the `Level` sheet places in `territory`.
    pub fn npcs(&self, territory: u16) -> Result<Vec<NpcPlacement>> {
        let mut npcs = self.npcs.lock();
        if npcs.is_none() {
            let level = self.sheet("Level")?;
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
                Ok(d) => match Layout::parse(&path, &d) {
                    Ok(l) => layout.layers.extend(l.layers),
                    // a few dungeons have files in an older layout format: left out
                    Err(e) => warn!("{e}"),
                },
                Err(DataError::NotFound(_)) => debug!(%path, "no such layout file"),
                Err(e) => return Err(e),
            }
        }
        let layout = Arc::new(layout);
        self.layouts.lock().insert(territory, layout.clone());
        Ok(layout)
    }

    /// Whether `territory` is a `TerritoryType` row with a layout folder.
    /// The names of every layer in `territory`'s layout files (empty layers too: later patches
    /// fill layers the β already has, and add new ones).
    pub fn layer_names(&self, territory: u16) -> Result<Arc<HashSet<String>>> {
        if let Some(n) = self.layer_names.lock().get(&territory) {
            return Ok(n.clone());
        }
        let folder = self
            .territories
            .get(&(territory as u32))
            .ok_or_else(|| DataError::NotFound(format!("TerritoryType {territory}")))?;
        let mut names = HashSet::new();
        for file in ALL_LAYOUT_FILES {
            let path = format!("{folder}/{file}");
            match self.pack.read(&path) {
                Ok(d) => match Layout::parse(&path, &d) {
                    Ok(l) => names.extend(l.layers.into_iter().map(|l| l.name)),
                    Err(e) => debug!("{e}"),
                },
                Err(DataError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let names = Arc::new(names);
        self.layer_names.lock().insert(territory, names.clone());
        Ok(names)
    }

    /// Whether a battle NPC spawn belongs in this build: its territory, `BNpcBase` and `BNpcName`
    /// rows exist, and so does its layout layer (`layer` empty: placed by hand, not checked).
    pub fn bnpc_misfit(&self, territory: u16, base_id: u32, name_id: u32, layer: &str) -> Result<Option<Misfit>> {
        if !self.has_territory(territory) {
            return Ok(Some(Misfit::Territory));
        }
        if self.sheet("BNpcBase")?.row(base_id).is_none() {
            return Ok(Some(Misfit::BNpcBase));
        }
        if self.sheet("BNpcName")?.row(name_id).is_none() {
            return Ok(Some(Misfit::BNpcName));
        }
        if !layer.is_empty() && !self.layer_names(territory)?.contains(layer) {
            return Ok(Some(Misfit::Layer));
        }
        Ok(None)
    }

    pub fn has_territory(&self, territory: u16) -> bool {
        self.territories.contains_key(&(territory as u32))
    }

    /// A place to stand in `territory`: the pop range its first exit names for players arriving
    /// through it (so on walkable ground), else its first pop range.
    pub fn arrival(&self, territory: u16) -> Result<Arrival> {
        let layout = self.layout(territory)?;
        let is_pop = |o: &&crate::lgb::Object| o.kind == crate::lgb::kind::POP_RANGE;
        let pop = layout
            .objects()
            .filter_map(|o| o.exit_range())
            .filter_map(|e| layout.find(e.return_point))
            .find(is_pop)
            .or_else(|| layout.objects().find(is_pop))
            .ok_or_else(|| DataError::NotFound(format!("a pop range in territory {territory}")))?;
        Ok(Arrival { territory, pos: pop.pos, rotation: pop.rot[1] })
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
