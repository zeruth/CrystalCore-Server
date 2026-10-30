//! `crystal-tools coverage`: how much of the β (build 672261) the server serves, domain by domain,
//! as a self-contained HTML page (like the client's fidelity report).
//!
//! Every domain counts what the truth has (the client's own sheets and scripts first; the server's
//! world database for what the client never carried) and gives each entry a status:
//!
//! - verified: the server does it and it was seen working in the β client;
//! - implemented: the server does it, from evidence, not yet seen in the client;
//! - partial: the server does some of it;
//! - pending: the β has it (data or script), the server does not;
//! - no source: the server needs data the β client does not carry and no β-era source is in yet;
//! - absent: not in this build (added in a later patch).
//!
//! What counts as implemented or verified is stated here, next to the evidence for it, so the
//! report and the code it describes are reviewed together.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use crystal_data::WorldData;
use crystal_db::{BETA_PATCH, Databases};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Status {
    Verified,
    Implemented,
    Partial,
    Pending,
    NoSource,
    Absent,
}

impl Status {
    const ALL: [Status; 6] =
        [Status::Verified, Status::Implemented, Status::Partial, Status::Pending, Status::NoSource, Status::Absent];

    fn label(self) -> &'static str {
        match self {
            Status::Verified => "verified",
            Status::Implemented => "implemented",
            Status::Partial => "partial",
            Status::Pending => "pending",
            Status::NoSource => "no source",
            Status::Absent => "absent",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Status::Verified => "s-ver",
            Status::Implemented => "s-imp",
            Status::Partial => "s-par",
            Status::Pending => "s-pen",
            Status::NoSource => "s-nos",
            Status::Absent => "s-abs",
        }
    }

    /// How much an entry counts towards fidelity.
    fn weight(self) -> f64 {
        match self {
            Status::Verified => 1.0,
            Status::Implemented => 0.75,
            Status::Partial => 0.35,
            _ => 0.0,
        }
    }
}

struct Entry {
    id: String,
    name: String,
    status: Status,
    detail: String,
}

struct Domain {
    group: &'static str,
    name: &'static str,
    /// Where the truth for this domain comes from.
    source: String,
    entries: Vec<Entry>,
    /// What stands between this domain and 100%, in order.
    plan: Vec<&'static str>,
}

impl Domain {
    fn new(group: &'static str, name: &'static str, source: impl Into<String>) -> Self {
        Self { group, name, source: source.into(), entries: Vec::new(), plan: Vec::new() }
    }

    fn add(&mut self, id: impl ToString, name: impl Into<String>, status: Status, detail: impl Into<String>) {
        self.entries.push(Entry { id: id.to_string(), name: name.into(), status, detail: detail.into() });
    }

    fn counts(&self) -> BTreeMap<Status, usize> {
        let mut c = BTreeMap::new();
        for e in &self.entries {
            *c.entry(e.status).or_insert(0) += 1;
        }
        c
    }

    /// Fidelity over what this build has (absent entries left out).
    fn fidelity(&self) -> f64 {
        let present: Vec<_> = self.entries.iter().filter(|e| e.status != Status::Absent).collect();
        if present.is_empty() {
            return 0.0;
        }
        present.iter().map(|e| e.status.weight()).sum::<f64>() / present.len() as f64
    }
}

fn str_at(row: &[crystal_data::Value], i: usize) -> &str {
    row.get(i).and_then(|v| v.as_str()).unwrap_or("")
}

fn u64_at(row: &[crystal_data::Value], i: usize) -> u64 {
    row.get(i).and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|x| x.max(0) as u64))).unwrap_or(0)
}

/// Rows of a sheet with a name in column `name_col` (or all rows when `None`).
fn named_rows(data: &WorldData, sheet: &str, name_col: Option<usize>) -> Vec<(u32, String, Vec<crystal_data::Value>)> {
    let Ok(s) = data.sheet(sheet) else { return vec![] };
    s.rows
        .iter()
        .filter_map(|(id, r)| {
            let name = name_col.map(|c| str_at(r, c).to_owned()).unwrap_or_default();
            (name_col.is_none() || !name.is_empty()).then(|| (*id, name, r.clone()))
        })
        .collect()
}

/// The quest definitions, from the repository's root.
const QUEST_DEFS: &str = "data/672261/quests";

pub async fn run(data: &WorldData, db: &Databases, out: &Path) -> anyhow::Result<()> {
    let mut domains = Vec::new();
    let items: HashMap<u32, String> =
        named_rows(data, "Item", Some(0)).into_iter().map(|(id, name, _)| (id, name)).collect();
    let item_name = |id: u64| items.get(&(id as u32)).cloned().unwrap_or_else(|| format!("item {id}"));

    // ---- content ---------------------------------------------------------------------------
    let mut quests = Domain::new(
        "Content",
        "Quests",
        "β Quest sheet (col 0 name, col 1 script, col 27 start ENpc, col 29 end ENpc, col 1333 exp factor, col 1334 gil) and its game_script/quest Lua",
    );
    // the definitions the zone runs (crystal-tools runs from the repository's root)
    let defs = crystal_common::quest::load_dir(Path::new(QUEST_DEFS)).unwrap_or_default();
    for (id, name, r) in named_rows(data, "Quest", Some(0)) {
        let script = str_at(&r, 1).to_owned();
        let n = id.saturating_sub(0x10000);
        let path = format!("game_script/quest/{:03}/{script}.luab", n / 100);
        let has_script = !script.is_empty() && data.pack().read(&path).is_ok();
        let def = defs.get(&id);
        let status = match (id, def) {
            // offered, accepted (scenes 0-2), kept, the opening's walls opened, turned in at Mother
            // Miounne (scenes 4-5: returned [1, 0]), 103 gil paid, completed (seen 2026-10-30);
            // its experience (col 1333 is a factor; the β's formula not yet known) not paid
            (0x10027, _) => Status::Partial,
            // run by the engine from a definition with nothing left to review
            (_, Some(d)) if has_script && d.review.is_empty() => Status::Implemented,
            // a definition with steps it cannot express yet
            (_, Some(_)) if has_script => Status::Partial,
            _ if has_script => Status::Pending,
            _ => Status::NoSource,
        };
        let defined = match def {
            Some(d) if d.review.is_empty() => format!(" · {:?} definition", d.source).to_lowercase(),
            Some(d) => format!(" · {:?} definition, {} to review", d.source, d.review.len()).to_lowercase(),
            None => " · no definition".to_owned(),
        };
        let detail = format!(
            "{script}{}{defined} · start ENpc {} · end ENpc {} · {} gil · exp factor {}",
            if has_script { "" } else { " (no script in the install)" },
            u64_at(&r, 27),
            u64_at(&r, 29),
            u64_at(&r, 1334),
            u64_at(&r, 1333)
        );
        quests.add(format!("{id:#x}"), name, status, detail);
    }
    quests.plan = vec![
        "Quest definitions (data/672261/quests, run by crystal-zone's engine): resolve each one's review notes (battle, item and event-action steps the format cannot say yet), then derive the quests that still have none",
        "Rewards: pin the item-reward columns near cols 1335-1389 against known 2.0 rewards (Coming to Gridania: 103 gil, exp factor 50 already match), then pay gil, exp and items on QuestReward",
        "Quest state packets: Quest (0x1e1) per change, QuestCompleteFlags (0x1e2) kept per character, the tracker; LegacyQuest flags (0x1f0)",
        "Battle and gathering steps: quest-owned BNpcs (IsBattleNpcTriggerOwner), EObjects (EOBJECT*), QuestBattle duties",
        "Verify each quest line in the client; record it here as verified",
    ];
    domains.push(quests);

    let mut openings = Domain::new("Content", "Openings", "β Opening sheet and game_script/opening Lua");
    openings.add("0x130002", "OpeningGridania", Status::Verified, "scenes 0, 1, 40, 20 and the wall ranges played in the client");
    openings.add("0x130001", "OpeningLimsaLominsa", Status::Implemented, "scene 0 played in the client; the rest as Gridania's, unseen");
    openings.add("0x130003", "OpeningUldah", Status::Implemented, "as Gridania's, unseen");
    openings.plan = vec![
        "Play the Limsa and Ul'dah openings through in the client",
        "Scene 0's return (3 for the Gridania one): Sapphire gives a starting ring by it (ITEM_INITIAL_RING_A-D, 4423-4426, in OpeningSystemDefine); confirm from the cutscene what the value means before giving anything",
    ];
    domains.push(openings);

    let mut leves = Domain::new("Content", "Levequests", "β Leve sheet (+ BattleLeve, CraftLeve, GatheringLeve, CompanyLeve, GuildleveAssignment)");
    for (id, name, _) in named_rows(data, "Leve", Some(0)) {
        leves.add(id, name, Status::Pending, "");
    }
    leves.plan = vec![
        "Guildleve assignment NPCs (GuildleveAssignment handlers), allowances (2.0: researched rate, dated source required)",
        "Battle leves: BattleLeve targets and their spawns (positions: Level/layout, else captures/Sapphire)",
        "Craft and gathering leves: turn-ins and GatheringLeve routes",
    ];
    domains.push(leves);

    let mut fates = Domain::new("Content", "FATEs", "β Fate sheet; positions and spawns are server-only");
    // (the β's Fate rows carry no English names)
    for (id, _, r) in named_rows(data, "Fate", None) {
        if r.iter().take(18).all(|v| v.as_u64().unwrap_or(0) == 0) {
            continue;
        }
        fates.add(id, format!("Fate {id}"), Status::NoSource, "where and when it runs, its enemies: not in the client");
    }
    fates.plan = vec![
        "Find a β-era source for FATE positions, levels and enemies (Level/layout entries, captures, dated databases)",
        "FATE director packets from the client's dispatcher; participation and rewards (2.0 rules, dated sources)",
    ];
    domains.push(fates);

    let mut duties = Domain::new("Content", "Instanced duties", "β InstanceContent sheet; their territories' layouts");
    for (id, name, _) in named_rows(data, "InstanceContent", Some(0)) {
        duties.add(id, name, Status::Pending, "");
    }
    for (id, name, _) in named_rows(data, "QuestBattle", None) {
        duties.add(format!("qb {id}"), if name.is_empty() { format!("QuestBattle {id}") } else { name }, Status::Pending, "quest battle");
    }
    duties.plan = vec![
        "Instances as their own zone loops; the Director (down 0x226) and its scripts",
        "Duty finder (party, roles, queue) after parties",
        "Each duty's enemies and scripts (Sapphire's instance scripts as a cross-check only)",
    ];
    domains.push(duties);

    let mut guildhests = Domain::new("Content", "Guildhests", "β GuildOrderGuide / GuildOrderOfficer");
    for (id, _, _) in named_rows(data, "GuildOrderGuide", None) {
        guildhests.add(id, format!("GuildOrderGuide {id}"), Status::Pending, "");
    }
    domains.push(guildhests);

    let mut treasure = Domain::new("Content", "Treasure coffers", "β Treasure sheet; contents are server-only");
    for (id, _, _) in named_rows(data, "Treasure", None) {
        treasure.add(id, format!("Treasure {id}"), Status::NoSource, "contents not in the client");
    }
    domains.push(treasure);

    // ---- items and economy -------------------------------------------------------------------
    let mut sources: HashMap<u32, Vec<&'static str>> = HashMap::new();
    let mut note = |id: u64, how: &'static str| {
        if id != 0 {
            let v = sources.entry(id as u32).or_default();
            if !v.contains(&how) {
                v.push(how);
            }
        }
    };
    for (_, _, r) in named_rows(data, "ShopItem", None) {
        note(u64_at(&r, 0), "gil shop");
    }
    for (_, _, r) in named_rows(data, "Recipe", None) {
        note(u64_at(&r, 2), "craft");
    }
    for (_, _, r) in named_rows(data, "GatheringItem", None) {
        note(u64_at(&r, 0), "gathering");
    }
    let mut starting = HashSet::new();
    for (_, _, r) in named_rows(data, "ClassJob", None) {
        let w = u64_at(&r, 22);
        if w != 0 {
            note(w, "starting gear");
            starting.insert(w as u32);
        }
    }
    for (_, _, r) in named_rows(data, "Race", None) {
        for c in 2..10 {
            let i = u64_at(&r, c);
            note(i, "starting gear");
            starting.insert(i as u32);
        }
    }
    let mut item_domain =
        Domain::new("Items & economy", "Obtainable items", "β Item sheet; sources: ShopItem col 0, Recipe col 2, GatheringItem col 0, ClassJob col 22, Race cols 2-9 (each read against named rows)");
    let mut ids: Vec<_> = items.keys().copied().collect();
    ids.sort();
    for id in ids {
        let how = sources.get(&id).cloned().unwrap_or_default();
        let status = if starting.contains(&id) {
            Status::Implemented
        } else if how.is_empty() {
            Status::NoSource
        } else {
            Status::Pending
        };
        let detail = if how.is_empty() {
            "no source in the client's data (drops, quest rewards, special shops: not yet mapped)".to_owned()
        } else {
            how.join(", ")
        };
        item_domain.add(id, items[&id].clone(), status, detail);
    }
    item_domain.plan = vec![
        "Gil shops (Shop/ShopItem, ENpcBase handlers 0x40000+n): the shop window's packets from the client, prices from Item × ShopItem rates",
        "Map the remaining item sources in the client's data: quest rewards (Quest cols ~1335+), SpecialShop, GCShop/GrandCompanySealShopItem, LeveRewardItem, Treasure",
        "Drops: the server-only loot tables (bnpc_spawn.loot_table) from a β-era source; until then mark each drop's source",
        "Crafting and gathering systems (below) make their items obtainable",
    ];
    domains.push(item_domain);

    let mut shops = Domain::new("Items & economy", "Gil shops", "β Shop sheet (rows 0x40000+n), ShopItem");
    for (id, name, r) in named_rows(data, "Shop", Some(42)) {
        let n = (2..42).filter(|&c| u64_at(&r, c) != 0).count();
        shops.add(format!("{id:#x}"), name, Status::Pending, format!("{n} items"));
    }
    shops.plan = vec!["Shop event (handler type 4): open the window, buy, sell, buyback; from the client's shop agent and its packets"];
    domains.push(shops);

    for (sheet, name) in [("SpecialShop", "Special shops"), ("GCShop", "Grand Company shops")] {
        let mut d = Domain::new("Items & economy", name, format!("β {sheet} sheet"));
        for (id, n, _) in named_rows(data, sheet, None) {
            d.add(format!("{id:#x}"), if n.is_empty() { format!("{sheet} {id}") } else { n }, Status::Pending, "");
        }
        domains.push(d);
    }

    let mut recipes = Domain::new("Items & economy", "Recipes (crafting)", "β Recipe sheet (col 2 result), CraftAction, RecipeLevelTable");
    for (id, _, r) in named_rows(data, "Recipe", None) {
        let result = u64_at(&r, 2);
        if result != 0 {
            recipes.add(id, item_name(result), Status::Pending, "");
        }
    }
    recipes.plan = vec![
        "Crafting from the client's synthesis packets; progress/quality formulas from dated 2.0-2.x sources (Japanese 2013 theorycraft first)",
    ];
    domains.push(recipes);

    let mut gathering =
        Domain::new("Items & economy", "Gathering points", "β GatheringPoint/GatheringPointBase sheets; positions in layouts (empty in the β's gathering layers)");
    for (id, _, _) in named_rows(data, "GatheringPoint", None) {
        gathering.add(id, format!("GatheringPoint {id}"), Status::NoSource, "no position in the β's layouts");
    }
    for (id, _, _) in named_rows(data, "FishingSpot", None) {
        gathering.add(format!("fish {id}"), format!("FishingSpot {id}"), Status::Pending, "");
    }
    gathering.plan = vec![
        "Gathering node positions from a β-era source (2.0 captures or dated databases); node respawn by Eorzea time",
        "Gathering and fishing actions and their packets",
    ];
    domains.push(gathering);

    let mut market = Domain::new("Items & economy", "Market board, retainers", "protocol (down 0x106-0x109 in the client's dispatcher)");
    market.add("market", "Market board", Status::Pending, "");
    market.add("retainers", "Retainers", Status::Pending, "");
    domains.push(market);

    // ---- world --------------------------------------------------------------------------------
    let mut handlers = Domain::new(
        "World",
        "Event NPC handlers",
        "β ENpcBase cols 2-33 (each event's handler type, its id's top 16 bits, named by the sheet whose rows start at that page)",
    );
    let mut type_sheet: HashMap<u32, &str> = HashMap::new();
    for sheet in [
        "Quest", "Warp", "Shop", "Opening", "CustomTalk", "DefaultTalk", "GCShop", "SpecialShop", "GuildleveAssignment",
        "ChocoboTaxiStand", "GuildOrderGuide", "GuildOrderOfficer", "Story", "GimmickYesNo", "GimmickBill", "GimmickRect",
        "CraftLeve", "QuestBattle", "ArrayEventHandler", "Aetheryte",
    ] {
        if let Ok(s) = data.sheet(sheet)
            && let Some((first, _)) = s.rows.first()
            && *first >= 0x10000
        {
            type_sheet.insert(first >> 16, sheet);
        }
    }
    let mut per_type: BTreeMap<u32, (usize, HashSet<u32>)> = BTreeMap::new();
    for (_, _, r) in named_rows(data, "ENpcBase", None) {
        for c in 2..34 {
            let e = u64_at(&r, c) as u32;
            if e != 0 {
                let t = per_type.entry(e >> 16).or_default();
                t.0 += 1;
                t.1.insert(e);
            }
        }
    }
    for (t, (uses, events)) in per_type {
        let sheet = type_sheet.get(&t).copied().unwrap_or("?");
        let status = match sheet {
            "Opening" => Status::Verified,
            "Warp" => Status::Implemented,
            "Quest" => Status::Partial,
            _ => Status::Pending,
        };
        handlers.add(format!("{t:#x}"), sheet, status, format!("{} events, on {uses} NPC slots", events.len()));
    }
    handlers.plan = vec![
        "Name the unnamed handler types from the client's event handler classes (RTTI: Client::Game::Event::*EventHandler)",
        "CustomTalk and DefaultTalk: their scripts (game_script/custom, talk) driven like the openings'",
        "Aetheryte attunement and teleport, chocobo porters (ChocoboTaxiStand)",
    ];
    domains.push(handlers);

    let mut bnpcs = Domain::new("World", "Battle NPC spawns", "world database bnpc_spawn (server-only data), checked against the β's BNpcBase/BNpcName/layouts");
    let spawns = db.world.bnpc_spawns(BETA_PATCH, None).await?;
    let mut per_territory: BTreeMap<u16, (usize, HashMap<String, usize>, usize)> = BTreeMap::new();
    for s in &spawns {
        let t = per_territory.entry(s.territory).or_default();
        t.0 += 1;
        *t.1.entry(s.source.clone()).or_insert(0) += 1;
        if s.loot_table != 0 {
            t.2 += 1;
        }
    }
    let tt = data.sheet("TerritoryType").ok();
    for (t, (n, by_source, with_loot)) in per_territory {
        let name = tt.as_ref().and_then(|s| s.row(t as u32)).map(|r| str_at(r, 0).to_owned()).unwrap_or_default();
        let src: Vec<_> = by_source.iter().map(|(s, n)| format!("{n} {s}")).collect();
        bnpcs.add(t, name, Status::Partial, format!("{n} spawns ({}); {with_loot} with loot tables; placeholder stats and AI", src.join(", ")));
    }
    bnpcs.plan = vec![
        "Replace Sapphire-sourced spawns (4.0 era) with β-era evidence where it differs; record each row's source",
        "Stats from BNpcBase/ParamGrow and dated 2.0 measurements instead of placeholders",
        "Aggro, linking, leash and respawn rules from dated 2.0 sources; navmesh chasing",
    ];
    domains.push(bnpcs);

    let mut territories = Domain::new("World", "Territories", "β TerritoryType (with a level path)");
    for (id, name, r) in named_rows(data, "TerritoryType", Some(0)) {
        if str_at(&r, 1).is_empty() {
            continue;
        }
        // entered in the client: Limsa Lominsa Lower Decks, Middle La Noscea, the Limsa and
        // Gridania openings; every other loads with its exits but no one has walked it
        let (status, how) = if matches!(id, 129 | 134 | 181 | 183) {
            (Status::Verified, "entered in the client")
        } else {
            (Status::Partial, "loads, exits from its layout; not visited")
        };
        territories.add(id, name, status, format!("{} · {how}", str_at(&r, 1)));
    }
    territories.plan = vec!["Aetherytes and teleport; instances; weather by region (Weather sheet, Eorzea time)"];
    domains.push(territories);

    let mut aetherytes = Domain::new("World", "Aetherytes", "β Aetheryte sheet");
    for (id, _, _) in named_rows(data, "Aetheryte", None) {
        aetherytes.add(id, format!("Aetheryte {id}"), Status::Pending, "attunement, teleport, return, home point");
    }
    domains.push(aetherytes);

    // ---- combat ---------------------------------------------------------------------------------
    let mut actions = Domain::new("Combat", "Player actions", "β Action sheet (col 9 class, potency from the description)");
    for (id, name, r) in named_rows(data, "Action", Some(0)) {
        if u64_at(&r, 9) == 0 {
            continue;
        }
        let potency = data.action(id).map_or(0, |a| a.potency);
        let (status, detail) = if potency > 0 {
            (Status::Partial, format!("potency {potency} (placeholder formula, no statuses)"))
        } else {
            (Status::Pending, "no effect yet".to_owned())
        };
        actions.add(id, name, status, detail);
    }
    actions.plan = vec![
        "Damage and healing formulas from dated 2.x derivations (combat research note), replacing combat.rs placeholders",
        "Statuses each action applies, from its description in the β and 2.0 patch notes; DoT/HoT ticks",
        "Traits, cross-class actions, the GCD and recasts as the client reports them",
    ];
    domains.push(actions);

    let mut statuses = Domain::new("Combat", "Statuses", "β Status sheet");
    for (id, name, _) in named_rows(data, "Status", Some(0)) {
        statuses.add(id, name, Status::Partial, "can be put on (.status), worn off on the tick; no effects");
    }
    domains.push(statuses);

    let mut traits = Domain::new("Combat", "Traits", "β Trait sheet");
    for (id, name, _) in named_rows(data, "Trait", Some(0)) {
        traits.add(id, name, Status::Pending, "");
    }
    domains.push(traits);

    // ---- character --------------------------------------------------------------------------
    let mut classes = Domain::new("Character", "Classes and jobs", "β ClassJob, ParamGrow");
    for (id, name, _) in named_rows(data, "ClassJob", Some(0)) {
        classes.add(id, name, Status::Partial, "level, exp, starting gear; class change and job unlock not yet");
    }
    domains.push(classes);

    for (sheet, group, name, col) in [
        ("Achievement", "Character", "Achievements", Some(1)),
        ("Title", "Character", "Titles", Some(0)),
        ("Emote", "Character", "Emotes", Some(14)),
        ("Mount", "Character", "Mounts", None),
        ("Companion", "Character", "Minions", Some(0)),
        ("MonsterNote", "Character", "Hunting log", None),
        ("GrandCompanyRank", "Character", "Grand Company ranks", None),
        ("Buddy", "Character", "Chocobo companion", None),
        ("Materia", "Character", "Materia", None),
        ("CompleteJournal", "Character", "Journal (completed)", None),
    ] {
        let mut d = Domain::new(group, name, format!("β {sheet} sheet"));
        for (id, n, _) in named_rows(data, sheet, col) {
            d.add(id, if n.is_empty() { format!("{sheet} {id}") } else { n }, Status::Pending, "");
        }
        domains.push(d);
    }
    let mut dyes = Domain::new("Character", "Dyes", "β Stain sheet (col 2 the dye's item)");
    for (id, _, r) in named_rows(data, "Stain", None) {
        let item = u64_at(&r, 2);
        if item != 0 {
            dyes.add(id, item_name(item), Status::Pending, "");
        }
    }
    domains.push(dyes);

    // ---- social -----------------------------------------------------------------------------------
    let mut social = Domain::new("Social", "Social systems", "protocol (the client's dispatcher and InfoProxy classes); server-side state");
    social.add("say", "Say, shout, yell", Status::Implemented, "relayed (say 20 yalms, shout/yell zone-wide; 2.0 ranges not yet researched)");
    social.add("tell", "Tells", Status::Pending, "");
    social.add("party", "Party", Status::Pending, "list requests (up 0xcb kind 2) unanswered");
    social.add("friends", "Friend list", Status::Pending, "list requests (up 0xcb kind 0x0b) unanswered");
    social.add("blacklist", "Blacklist", Status::Implemented, "an empty last page (down 0xe3)");
    social.add("linkshells", "Linkshells", Status::Partial, "an empty list (down 0xf6); creating, joining, chat not yet");
    social.add("fc", "Free companies", Status::Pending, "FC* sheets present");
    social.add("search", "Player search", Status::Pending, "");
    social.add("trade", "Trade", Status::Pending, "");
    social.add("mail", "Letters (moogle mail)", Status::Pending, "");
    social.add("pvp", "PvP (Wolves' Den)", Status::Absent, "no PvP sheets in this build (patch 2.1)");
    social.add("housing", "Housing", Status::Absent, "no housing sheets in this build (patch 2.1)");
    social.plan = vec![
        "Chat services: tells, party, linkshell and FC channels over the chat connection; 2.0 ranges from dated sources",
        "InfoProxy replies (party, friends, linkshell members) from the client's reply layouts",
        "Linkshells and free companies: create, invite, ranks, chat (FC* sheets)",
    ];
    domains.push(social);

    // ---- protocol ---------------------------------------------------------------------------------
    let mut proto = Domain::new("Protocol", "Zone down packets", "protocol/672261/zone_down.tsv (the client's PacketDispatcher)");
    let sent: HashSet<u16> = [
        0x67, 0xe3, 0xf6, 0x140, 0x142, 0x146, 0x148, 0x190, 0x191, 0x192, 0x196, 0x19a, 0x19f, 0x1a0, 0x1a1, 0x1a3,
        0x1a5, 0x1af, 0x1b0, 0x1b2, 0x1b3, 0x1b6, 0x1c3, 0x1cc, 0x1cd, 0x1e0, 0x1e1, 0x1e2, 0x2d6,
    ]
    .into();
    if let Ok(tsv) = std::fs::read_to_string("protocol/672261/zone_down.tsv") {
        for line in tsv.lines().filter(|l| l.starts_with("0x")) {
            let f: Vec<_> = line.split('\t').collect();
            let op = u16::from_str_radix(f[0].trim_start_matches("0x"), 16).unwrap_or(0);
            let status = if sent.contains(&op) { Status::Implemented } else { Status::Pending };
            proto.add(f[0], f.get(3).copied().unwrap_or(""), status, format!("case {} → {}", f.get(1).unwrap_or(&""), f.get(2).unwrap_or(&"")));
        }
    }
    proto.plan = vec!["Each pending opcode: its layout from the handler, a server sender when a system needs it, a replay test"];
    domains.push(proto);

    std::fs::create_dir_all(out.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(out, render(&domains))?;
    for d in &domains {
        let c: Vec<_> = d.counts().iter().map(|(s, n)| format!("{n} {}", s.label())).collect();
        println!("{:<26} {:>6} {:>5.1}%  {}", d.name, d.entries.len(), d.fidelity() * 100.0, c.join(", "));
    }
    let total: usize = domains.iter().map(|d| d.entries.len()).sum();
    println!("{} domains, {total} entries → {}", domains.len(), out.display());
    Ok(())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn render(domains: &[Domain]) -> String {
    let mut h = String::new();
    let all: Vec<&Entry> = domains.iter().flat_map(|d| &d.entries).collect();
    let present = all.iter().filter(|e| e.status != Status::Absent).count().max(1);
    let overall = all.iter().map(|e| e.status.weight()).sum::<f64>() / present as f64;
    let count = |s: Status| all.iter().filter(|e| e.status == s).count();
    h.push_str(HEAD);
    let _ = write!(
        h,
        "<main><h1>CrystalCore-Server coverage</h1><div class=sub>The β client (2013.08.09, build 672261) as the server serves it. \
         Every entry comes from the client's own data or scripts, or from the server's world database for what the client never \
         carried; generated by <code>crystal-tools coverage</code>.</div><div class=tiles>\
         <div class=tile><b>{:.1}%</b><span>fidelity (verified 1, implemented .75, partial .35)</span></div>\
         <div class=tile><b>{}</b><span>entries</span></div>",
        overall * 100.0,
        all.len()
    );
    for s in Status::ALL {
        let _ = write!(h, "<div class=tile><b>{}</b><span><i class='dot {}'></i>{}</span></div>", count(s), s.class(), s.label());
    }
    h.push_str("</div><section><div class=sh><h2>Domains</h2><span class=muted>fidelity over what this build has</span></div>");
    let mut group = "";
    for d in domains {
        if d.group != group {
            group = d.group;
            let _ = write!(h, "<div class=group>{}</div>", esc(group));
        }
        let _ = write!(
            h,
            "<a class=row href='#{}'><span class=lbl>{}</span>{}<span class=v>{:.0}%</span><span class=v>{}</span></a>",
            anchor(d.name),
            esc(d.name),
            bar(d),
            d.fidelity() * 100.0,
            d.entries.len()
        );
    }
    h.push_str("</section>");
    h.push_str("<section><div class=sh><h2>Plan to 100%</h2><span class=muted>per domain, in order</span></div><ol class=plan>");
    for d in domains.iter().filter(|d| !d.plan.is_empty()) {
        let _ = write!(h, "<li><b>{}</b> ({:.0}%)<ul>", esc(d.name), d.fidelity() * 100.0);
        for step in &d.plan {
            let _ = write!(h, "<li>{}</li>", esc(step));
        }
        h.push_str("</ul></li>");
    }
    h.push_str("</ol></section>");
    for d in domains {
        let c = d.counts();
        let _ = write!(
            h,
            "<section id='{}'><div class=sh><h2>{}</h2><span class=muted>{} · {} entries · {:.0}%</span></div>{}<div class=legend>",
            anchor(d.name),
            esc(d.name),
            esc(&d.source),
            d.entries.len(),
            d.fidelity() * 100.0,
            bar(d)
        );
        for (s, n) in &c {
            let _ = write!(h, "<span><i class='dot {}'></i>{} {}</span>", s.class(), n, s.label());
        }
        let _ = write!(
            h,
            "</div><details><summary>{} entries</summary><input class=q placeholder='filter'><div class=wrap><table><thead><tr><th>id</th><th>name</th><th>status</th><th>detail</th></tr></thead><tbody>",
            d.entries.len()
        );
        for e in &d.entries {
            let _ = write!(
                h,
                "<tr><td>{}</td><td>{}</td><td><i class='dot {}'></i>{}</td><td>{}</td></tr>",
                esc(&e.id),
                esc(&e.name),
                e.status.class(),
                e.status.label(),
                esc(&e.detail)
            );
        }
        h.push_str("</tbody></table></div></details></section>");
    }
    h.push_str(TAIL);
    h
}

fn anchor(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect()
}

fn bar(d: &Domain) -> String {
    let n = d.entries.len().max(1) as f64;
    let mut s = String::from("<span class=stack>");
    for (st, c) in d.counts() {
        let _ = write!(s, "<span class='seg {}' style='width:{:.3}%' title='{} {}'></span>", st.class(), c as f64 * 100.0 / n, c, st.label());
    }
    s.push_str("</span>");
    s
}

const HEAD: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>CrystalCore-Server coverage</title>
<style>
:root { --bg:#f7f7f5; --card:#fff; --fg:#1b1b19; --muted:#6d6c66; --line:#e6e5e0; --track:#efeee9;
  --ver:#17834a; --imp:#79c28e; --par:#e3b02c; --pen:#d8613a; --nos:#8b5a9e; --abs:#a9a8a2; }
@media (prefers-color-scheme: dark) { :root { --bg:#141416; --card:#1c1c20; --fg:#ecebe7; --muted:#9b9a94;
  --line:#2b2b31; --track:#26262c; --abs:#55545c; } }
* { box-sizing:border-box; }
body { background:var(--bg); color:var(--fg); font:14px/1.45 system-ui,-apple-system,"Segoe UI",sans-serif; margin:0; padding:28px 16px; }
main { max-width:1180px; margin:0 auto; }
h1 { font-size:22px; margin:0 0 2px; } h2 { font-size:16px; margin:0; }
.sub, .muted { color:var(--muted); } .sub { margin-bottom:20px; font-size:13px; }
.tiles { display:grid; grid-template-columns:repeat(auto-fill,minmax(150px,1fr)); gap:10px; margin-bottom:18px; }
.tile { background:var(--card); border:1px solid var(--line); border-radius:10px; padding:12px 14px; }
.tile b { display:block; font-size:22px; font-variant-numeric:tabular-nums; } .tile span { color:var(--muted); font-size:12px; }
section { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:16px 18px; margin-bottom:16px; }
.sh { display:flex; gap:10px; align-items:baseline; flex-wrap:wrap; margin-bottom:12px; } .sh .muted { font-size:12px; }
.group { font-size:11px; text-transform:uppercase; letter-spacing:.06em; color:var(--muted); margin:12px 0 4px; }
.row { display:grid; grid-template-columns:190px 1fr 56px 64px; gap:10px; align-items:center; padding:3px 0; color:inherit; text-decoration:none; }
.lbl { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.stack { display:flex; height:12px; border-radius:6px; overflow:hidden; background:var(--track); }
section > .stack { height:16px; margin-bottom:10px; }
.seg { height:100%; } .v { text-align:right; font-variant-numeric:tabular-nums; font-size:13px; }
.dot { width:10px; height:10px; border-radius:3px; display:inline-block; margin-right:6px; vertical-align:-1px; }
.s-ver { background:var(--ver); } .s-imp { background:var(--imp); } .s-par { background:var(--par); }
.s-pen { background:var(--pen); } .s-nos { background:var(--nos); } .s-abs { background:var(--abs); }
.legend { display:flex; gap:14px; flex-wrap:wrap; font-size:12px; color:var(--muted); margin-bottom:8px; }
.plan li { margin:6px 0; } .plan ul { margin:4px 0 8px; color:var(--muted); }
summary { cursor:pointer; color:var(--muted); font-size:13px; }
input { font:inherit; color:inherit; background:var(--bg); border:1px solid var(--line); border-radius:8px; padding:6px 10px; margin:8px 0; width:100%; }
.wrap { overflow-x:auto; max-height:520px; overflow-y:auto; } table { border-collapse:collapse; width:100%; font-size:13px; }
th, td { text-align:left; padding:4px 8px; border-bottom:1px solid var(--line); vertical-align:top; }
th { position:sticky; top:0; background:var(--card); font-weight:600; } code { font-size:12px; }
</style></head><body>"#;

const TAIL: &str = r#"<script>
for (const q of document.querySelectorAll('input.q')) {
  q.addEventListener('input', () => {
    const t = q.value.toLowerCase();
    for (const tr of q.parentElement.querySelectorAll('tbody tr')) tr.style.display = tr.textContent.toLowerCase().includes(t) ? '' : 'none';
  });
}
</script></main></body></html>"#;
