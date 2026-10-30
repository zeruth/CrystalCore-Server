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

/// `NpcEquip`'s ten equipment models (u32 at 0x10-0x37) and their dyes (u8 at 0x3a-0x43), by
/// column.
const NPC_EQUIP_MODELS: [usize; 10] = [4, 7, 9, 11, 13, 15, 17, 19, 21, 23];
const NPC_EQUIP_DYES: [usize; 10] = [5, 8, 10, 12, 14, 16, 18, 20, 22, 24];

/// The main attributes' base by level (1-50; index 0 unused): Sapphire's `levelTable` MAIN
/// column, ARR's numbers, until the β's own are found.
pub const MAIN_BASE: [i64; 51] = [
    1, 20, 21, 22, 24, 26, 27, 29, 31, 33, 35, 36, 38, 41, 44, 46, 49, 52, 54, 57, 60, 63, 67, 71, 74, 78, 81, 85, 89,
    92, 97, 101, 106, 110, 115, 119, 124, 128, 134, 139, 144, 150, 155, 161, 166, 171, 177, 183, 189, 196, 202,
];

/// A character's base attributes and maximum HP/MP (before gear).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub strength: u32,
    pub dexterity: u32,
    pub vitality: u32,
    pub intelligence: u32,
    pub mind: u32,
    pub piety: u32,
    pub hp: u32,
    pub mp: u32,
}

/// What the server reads of an `Action` row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActionRow {
    pub name: String,
    /// `ActionCategory`: 1 auto-attack, 2 spell, 3 weaponskill, 4 ability, ...
    pub category: u8,
    pub class: i8,
    pub level: u8,
    /// Yalms; -1 (or 0) for melee.
    pub range: i8,
    pub hostile: bool,
    pub cost_type: u8,
    pub cost: u16,
    pub cast_100ms: u16,
    pub recast_100ms: u16,
    /// From the description; 0 when it names none.
    pub potency: u32,
}

/// The number after "potency of" in an action's description, or after "Potency:" (a spell's
/// "Cure Potency: 400", the colour markup between: `Potency:\x02\x13\x02..\x03 400`).
pub fn potency_in(description: &str) -> u32 {
    let after = ["potency of ", "Potency:"]
        .iter()
        .find_map(|marker| description.find(marker).map(|at| &description[at + marker.len()..]));
    let Some(after) = after else { return 0 };
    // (past the markup, spaces and punctuation to the number; a word first means it names none)
    let digits: String = after
        .chars()
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().unwrap_or(0)
}

/// What the server reads of an `Item` row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemRow {
    pub name: String,
    pub stack_size: u32,
    /// `EquipSlotCategory` row (0: not equippable).
    pub slot_category: u8,
    pub model_main: u64,
    pub model_sub: u64,
}

/// How an NPC looks, as a spawn carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NpcLook {
    /// `ModelChara` row.
    pub model: u16,
    pub customize: [u8; 26],
    pub main_weapon: u64,
    pub sub_weapon: u64,
    pub models: [u32; 10],
}

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

    /// How a battle NPC looks: `BNpcBase` gives its `ModelChara` (col 5), `BNpcCustomize` (6)
    /// and `NpcEquip` (7) rows; the client builds the body from what the spawn carries, taking
    /// only the scale from the base row (`ReadSpawn` @0xaac200, case 2).
    pub fn bnpc_look(&self, base_id: u32) -> Option<NpcLook> {
        let base = self.sheet("BNpcBase").ok()?;
        let row = base.row(base_id)?;
        let col = |i: usize| row.get(i).and_then(|v| v.as_u64()).unwrap_or(0);
        let mut look = NpcLook { model: col(5) as u16, ..Default::default() };
        if col(6) != 0
            && let Some(c) = self.sheet("BNpcCustomize").ok().and_then(|s| s.row(col(6) as u32).map(<[_]>::to_vec))
        {
            for (slot, v) in look.customize.iter_mut().zip(&c) {
                *slot = v.as_u64().unwrap_or(0) as u8;
            }
        }
        if col(7) != 0
            && let Some(e) = self.sheet("NpcEquip").ok().and_then(|s| s.row(col(7) as u32).map(<[_]>::to_vec))
        {
            let u = |i: usize| e.get(i).and_then(|v| v.as_u64()).unwrap_or(0);
            look.main_weapon = u(0);
            look.sub_weapon = u(2);
            // ten (model, dye) pairs: the dye goes in the model's top byte, as gear models carry it
            for (i, (m, d)) in NPC_EQUIP_MODELS.iter().zip(NPC_EQUIP_DYES).enumerate() {
                look.models[i] = (u(*m) as u32 & 0x00ff_ffff) | ((u(d) as u32 & 0xff) << 24);
            }
        }
        Some(look)
    }

    /// A class's starting gear (`CharaMakeClassEquip`: armour models in cols 0-4, weapons in 5
    /// and 6, the `ClassJob` in 7): main weapon, off hand, the ten equipment models.
    pub fn starting_gear(&self, class: u8) -> Option<(u64, u64, [u32; 10])> {
        let sheet = self.sheet("CharaMakeClassEquip").ok()?;
        let (_, row) = sheet.rows.iter().find(|(_, r)| r.get(7).and_then(|v| v.as_u64()) == Some(class as u64))?;
        let u = |i: usize| row.get(i).and_then(|v| v.as_u64()).unwrap_or(0);
        let mut models = [0u32; 10];
        for (i, m) in models.iter_mut().take(5).enumerate() {
            *m = u(i) as u32;
        }
        Some((u(5), u(6), models))
    }

    /// An `Action` row as the server needs it (the β's columns: category 3, class 9, level 10,
    /// range 11, hostile 15, cost type 27 and cost 28, cast 33 and recast 34 in tenths of a
    /// second; SaintCoinach 2.5 has the last four one higher). The potency is read from the
    /// description ("... with a potency of 150").
    pub fn action(&self, id: u32) -> Option<ActionRow> {
        let sheet = self.sheet("Action").ok()?;
        let r = sheet.row(id)?;
        let u = |i: usize| r.get(i).and_then(|v| v.as_u64()).unwrap_or(0);
        let description = r.get(1).and_then(|v| v.as_str()).unwrap_or_default();
        Some(ActionRow {
            name: r.first().and_then(|v| v.as_str()).unwrap_or_default().to_owned(),
            category: u(3) as u8,
            class: r.get(9).and_then(|v| v.as_i64()).unwrap_or(0) as i8,
            level: u(10) as u8,
            range: r.get(11).and_then(|v| v.as_i64()).unwrap_or(0) as i8,
            hostile: r.get(15).and_then(|v| v.as_u64()) == Some(1),
            cost_type: u(27) as u8,
            cost: u(28) as u16,
            cast_100ms: u(33) as u16,
            recast_100ms: u(34) as u16,
            potency: potency_in(description),
        })
    }

    /// An `Item` row as the server needs it (the β's columns: `StackSize` 14, `EquipSlotCategory`
    /// 28, the main and sub models 29 and 30; SaintCoinach 2.5 has them one lower from 27 on).
    pub fn item(&self, item_id: u32) -> Option<ItemRow> {
        let sheet = self.sheet("Item").ok()?;
        let row = sheet.row(item_id)?;
        let u = |i: usize| row.get(i).and_then(|v| v.as_u64()).unwrap_or(0);
        Some(ItemRow {
            name: row.get(9).and_then(|v| v.as_str()).unwrap_or_default().to_owned(),
            stack_size: u(14).max(1) as u32,
            slot_category: u(28) as u8,
            model_main: u(29),
            model_sub: u(30),
        })
    }

    /// The first item with `model` for `slot_category` (an `EquipSlotCategory` row).
    pub fn item_by_model(&self, model: u64, slot_category: u8) -> Option<u32> {
        let sheet = self.sheet("Item").ok()?;
        sheet
            .rows
            .iter()
            .find(|(_, r)| {
                r.get(29).and_then(|v| v.as_u64()) == Some(model)
                    && r.get(28).and_then(|v| v.as_u64()) == Some(slot_category as u64)
            })
            .map(|(id, _)| *id)
    }

    /// What a new character wears, as (equipped slot, item): its class's starting weapon
    /// (`ClassJob` col 22: the weathered shortsword for a gladiator, the weathered shortbow an
    /// archer was given) and its race's starting body, hands, legs and feet (`Race` cols 2-5 for
    /// a male, 6-9 for a female: a Hyur's tunic, gloves, breeches, thighboots or chemisette,
    /// halfgloves, skirt, longboots), nothing else, as Sapphire makes a character (its
    /// `PlayerMinimal::saveAsNew`: `ClassJob.InitWeapon`, `Race.Body/Hand/Leg/Foot[gender]`).
    /// `gender` is the CharaMake appearance's second value (0 male).
    pub fn starting_items(&self, class: u8, race: u8, gender: u8) -> Vec<(u16, u32)> {
        let mut items = Vec::new();
        if let Some(weapon) = self.sheet("ClassJob").ok().and_then(|s| s.row(class as u32)?.get(22)?.as_u64())
            && weapon != 0
        {
            items.push((0, weapon as u32));
        }
        let races = self.sheet("Race").ok();
        let first = if gender == 0 { 2 } else { 6 };
        for (i, slot) in [3u16, 4, 6, 7].into_iter().enumerate() {
            let item = races.as_ref().and_then(|s| s.row(race as u32)?.get(first + i)?.as_u64()).unwrap_or(0);
            if item != 0 {
                items.push((slot, item as u32));
            }
        }
        items
    }

    /// How equipped items look on a character: (main weapon, off hand, the ten gear models in
    /// the spawn's order: head, body, hands, legs, feet, ears, neck, wrists, rings).
    pub fn equipment_look(&self, equipped: &[(u16, u32)]) -> (u64, u64, [u32; 10]) {
        let (mut main, mut sub, mut models) = (0, 0, [0u32; 10]);
        for &(slot, item_id) in equipped {
            let Some(item) = self.item(item_id) else { continue };
            // equipped slot (Sapphire's GearSetSlot) → the spawn's model slot
            let model_slot = match slot {
                0 => {
                    (main, sub) = (item.model_main, if sub == 0 { item.model_sub } else { sub });
                    continue;
                }
                1 => {
                    sub = item.model_main;
                    continue;
                }
                2 => 0,
                3 => 1,
                4 => 2,
                6 => 3,
                7 => 4,
                8 => 5,
                9 => 6,
                10 => 7,
                11 => 8,
                12 => 9,
                _ => continue,
            };
            models[model_slot] = item.model_main as u32;
        }
        (main, sub, models)
    }

    /// A character's base attributes and maximum HP/MP before gear: the level's main base
    /// ([`MAIN_BASE`], Sapphire's table: the β's `ParamGrow` does not carry it) times the class's
    /// modifier (`ClassJob` cols 5-12: HP, MP, STR, VIT, DEX, INT, MND, PIE) over 100, plus the
    /// tribe's bonus (`Tribe` cols 4-9: STR, DEX, VIT, INT, MND, PIE); HP and MP from
    /// `ParamGrow`'s bases by the class's modifiers. MP's base is col 5 ([`Self::mp_base`]): 2.x's
    /// level-50 healers and casters (WHM `7.90 PIE + 1596`, SCH `7.57 PIE + 1530`, BLM
    /// `8.23 PIE + 1662`, measured in June 2014) are all `(1330 + 6.58 PIE) * MP modifier / 100`,
    /// that is `2660 * (202 + PIE) / (2 x 202)`: col 5's 2660 and the main base 202. HP's (col 5
    /// too, as yet) is unverified.
    pub fn base_stats(&self, class: u8, tribe: u8, level: u8) -> Option<Stats> {
        let cj = self.sheet("ClassJob").ok()?;
        let class_row = cj.row(class as u32)?;
        let m = |i: usize| class_row.get(i).and_then(|v| v.as_i64()).unwrap_or(100);
        let tribes = self.sheet("Tribe").ok()?;
        let t = |i: usize| tribes.row(tribe as u32).and_then(|r| r.get(i)).and_then(|v| v.as_i64()).unwrap_or(0);
        let grow = self.sheet("ParamGrow").ok()?;
        let g = |i: usize| grow.row(level as u32).and_then(|r| r.get(i)).and_then(|v| v.as_i64()).unwrap_or(0);
        let main = MAIN_BASE[(level as usize).min(MAIN_BASE.len() - 1)];
        let attr = |modifier: i64, bonus: i64| (main * modifier / 100 + bonus).max(1) as u32;
        let piety = attr(m(12), t(9));
        // (MP from the MP base and piety: `base * (main + PIE) / (2 main)`, each piety point above
        // the main base worth base / (2 main), 6.58 at level 50 as 2014's measured lines have it;
        // the other levels by the same rule, unmeasured)
        let mp = g(5) * (main + piety as i64) / (2 * main) * m(6) / 100;
        Some(Stats {
            strength: attr(m(7), t(4)),
            dexterity: attr(m(9), t(5)),
            vitality: attr(m(8), t(6)),
            intelligence: attr(m(10), t(7)),
            mind: attr(m(11), t(8)),
            piety,
            hp: (g(5) * m(5) / 100).max(1) as u32,
            mp: mp.max(0) as u32,
        })
    }

    /// The level's MP base (`ParamGrow` col 5: 104 at level 1, 2660 at 50), which max MP and an
    /// action's MP cost scale from: a cost of N (`Action` col 28, cost type 3) is N% of it
    /// (Sapphire's `calculateMpCost`), Cure's 5 133 MP at level 50, about 4% of a white mage's MP.
    pub fn mp_base(&self, level: u8) -> Option<u32> {
        let v = self.sheet("ParamGrow").ok()?.row(level as u32)?.get(5)?.as_i64()?;
        (v > 0).then_some(v as u32)
    }

    /// A quest's script (`game_script/quest/NNN/<Quest col 1>.luab`, NNN its index / 100), read for
    /// its scene functions; `None` when the quest names no script or the install lacks it.
    pub fn quest_script(&self, quest_id: u32) -> Option<crate::luac::Script> {
        let script = self.sheet("Quest").ok()?.row(quest_id)?.get(1)?.as_str()?.to_owned();
        if script.is_empty() {
            return None;
        }
        let path = format!("game_script/quest/{:03}/{script}.luab", (quest_id & 0xffff) / 100);
        crate::luac::Script::parse(&self.pack.read(&path).ok()?).ok()
    }

    /// A variable of an opening's script, from its `Opening` row (0x130001-0x130003): the names
    /// are cols 2-41, each value 40 columns on (`ERANGE_SEQ_1_CLOSED_1`, `POS_START`, ...).
    pub fn opening_value(&self, event_id: u32, name: &str) -> Option<u32> {
        let sheet = self.sheet("Opening").ok()?;
        let row = sheet.row(event_id)?;
        let at = (2..42).find(|&i| row.get(i).and_then(|v| v.as_str()) == Some(name))?;
        row.get(at + 40)?.as_u64().map(|v| v as u32)
    }

    /// The town a class starts in (`ClassJob` col 25, a `Town` row: 1 Limsa Lominsa, 2 Gridania,
    /// 3 Ul'dah); `None` for a class one cannot start as (0 there). The β's rows: gladiator,
    /// pugilist, thaumaturge 3; marauder, arcanist 1; lancer, archer, conjurer 2; the client's
    /// character creation showed Gridania for an archer.
    pub fn starting_town(&self, class: u8) -> Option<u8> {
        let v = self.sheet("ClassJob").ok()?.row(class as u32)?.get(25)?.as_i64()?;
        (v > 0).then_some(v as u8)
    }

    /// The experience a level needs to reach the next (`ParamGrow` col 0); `None` past the table.
    pub fn exp_to_next(&self, level: u8) -> Option<u32> {
        let v = self.sheet("ParamGrow").ok()?.row(level as u32)?.first()?.as_i64()?;
        (v > 0).then_some(v as u32)
    }

    /// Which of the 19 level slots (`PlayerStatus` @0x66) a class's level is kept in
    /// (`ClassJob` col 4: gladiator and paladin share 1, ...).
    pub fn class_level_slot(&self, class: u8) -> Option<usize> {
        let slot = self.sheet("ClassJob").ok()?.row(class as u32)?.get(4)?.as_i64()?;
        (0..19).contains(&slot).then_some(slot as usize)
    }

    /// Where a warp event (handler type 2, a `Warp` row) leads: its `Level` row (col 0) gives the
    /// position (cols 0-3) and the territory (col 9).
    pub fn warp(&self, event_id: u32) -> Option<Arrival> {
        let level_id = self.sheet("Warp").ok()?.row(event_id)?.first()?.as_u64()? as u32;
        let level = self.sheet("Level").ok()?;
        let row = level.row(level_id)?;
        let f = |i: usize| row.get(i).and_then(|v| v.as_f32()).unwrap_or(0.0);
        let territory = row.get(9)?.as_u64()? as u16;
        Some(Arrival { territory, pos: [f(0), f(1), f(2)], rotation: f(3) })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn potencies_as_weaponskills_and_spells_write_them() {
        assert_eq!(potency_in("Delivers an attack with a potency of 150."), 150);
        // Cure (Action 120) as the β's sheet has it, the colour markup decoded lossily
        let cure = "Restores target's HP.  \u{2}\u{13}\u{5}\u{fffd}\u{fffd}\u{fffd}\"\u{3}Cure Potency:\u{2}\u{13}\u{2}\u{fffd}\u{3} 400\u{2}";
        assert_eq!(potency_in(cure), 400);
        assert_eq!(potency_in("Increases your defense. Duration: 20s"), 0);
        // a number far past the marker is not its potency
        assert_eq!(potency_in("Potency: none here, but 30 yalms away"), 0);
    }
}
