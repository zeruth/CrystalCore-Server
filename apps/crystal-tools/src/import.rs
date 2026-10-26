//! `crystal-tools import`: server-only content into the world database.
//!
//! `sapphire-bnpcs` reads Sapphire's `battlenpc` dump (`sql/migrations/*BattleNpc*.sql`: MySQL
//! `INSERT INTO battlenpc (columns) VALUES (...), ...`), made from later clients' layouts. Each
//! row is checked against this build: its territory (by name, `TerritoryType` column 0, so ids
//! that moved between patches still land right), its `BNpcBase` and `BNpcName` rows. What does
//! not fit is counted and left out.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, bail};
use crystal_data::WorldData;
use crystal_db::{BETA_PATCH, BnpcSpawn, Databases};

pub const SAPPHIRE: &str = "sapphire";

/// One SQL value of a dump.
#[derive(Debug, Clone, PartialEq)]
enum Sql {
    Null,
    Text(String),
}

impl Sql {
    fn text(&self) -> &str {
        match self {
            Sql::Null => "",
            Sql::Text(t) => t.trim(),
        }
    }
}

/// The value tuples of one `VALUES (...), (...);` run, from `s` (just after `VALUES`). Returns them
/// and where the statement ended.
fn tuples(s: &str) -> anyhow::Result<(Vec<Vec<Sql>>, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            bail!("the dump ends inside an INSERT");
        }
        match b[i] {
            b';' => return Ok((out, i + 1)),
            b',' => {
                i += 1;
                continue;
            }
            b'(' => i += 1,
            c => bail!("unexpected {:?} between tuples", c as char),
        }
        let mut row = Vec::new();
        loop {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= b.len() {
                bail!("the dump ends inside a tuple");
            }
            if b[i] == b'\'' {
                i += 1;
                let mut text = Vec::new();
                while i < b.len() && b[i] != b'\'' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                    }
                    text.push(b[i]);
                    i += 1;
                    // '' is a quote inside a string
                    if i + 1 < b.len() && b[i] == b'\'' && b[i + 1] == b'\'' {
                        text.push(b'\'');
                        i += 2;
                    }
                }
                i += 1;
                row.push(Sql::Text(String::from_utf8_lossy(&text).into_owned()));
            } else {
                let start = i;
                while i < b.len() && b[i] != b',' && b[i] != b')' {
                    i += 1;
                }
                let word = s[start..i].trim();
                row.push(if word.eq_ignore_ascii_case("NULL") { Sql::Null } else { Sql::Text(word.to_owned()) });
            }
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            match b.get(i) {
                Some(b',') => i += 1,
                Some(b')') => {
                    i += 1;
                    break;
                }
                _ => bail!("unexpected end of a tuple"),
            }
        }
        out.push(row);
    }
}

/// Every `INSERT INTO <table> (cols) VALUES ...` of `table` in a dump: its rows, by column name.
fn inserts(dump: &str, table: &str) -> anyhow::Result<Vec<HashMap<String, Sql>>> {
    let mut rows = Vec::new();
    let head = format!("INSERT INTO `{table}`");
    let mut rest = dump;
    while let Some(at) = rest.find(&head) {
        let stmt = &rest[at + head.len()..];
        let open = stmt.find('(').context("INSERT without columns")?;
        let close = stmt[open..].find(')').context("INSERT without columns")? + open;
        let cols: Vec<String> =
            stmt[open + 1..close].split(',').map(|c| c.trim().trim_matches('`').to_owned()).collect();
        let values = stmt[close..].find("VALUES").context("INSERT without VALUES")? + close + "VALUES".len();
        let (tuples, used) = tuples(&stmt[values..])?;
        for t in tuples {
            if t.len() != cols.len() {
                bail!("a {table} row has {} values for {} columns", t.len(), cols.len());
            }
            rows.push(cols.iter().cloned().zip(t).collect());
        }
        rest = &stmt[values + used..];
    }
    Ok(rows)
}

#[derive(Debug, Default)]
pub struct Report {
    pub read: usize,
    pub imported: usize,
    pub unknown_territory: BTreeMap<String, usize>,
    pub unknown_base: usize,
    pub unknown_name: usize,
    pub per_territory: BTreeMap<u16, usize>,
}

/// Sapphire's battle NPCs, as this build's spawns.
pub fn sapphire_bnpcs(data: &WorldData, dump: &str) -> anyhow::Result<(Vec<BnpcSpawn>, Report)> {
    let rows = inserts(dump, "battlenpc")?;
    let bases: HashSet<u32> = data.sheet("BNpcBase")?.rows.iter().map(|r| r.0).collect();
    let names: HashSet<u32> = data.sheet("BNpcName")?.rows.iter().map(|r| r.0).collect();
    let mut territories: HashMap<String, Option<u16>> = HashMap::new();
    let mut report = Report { read: rows.len(), ..Default::default() };
    let mut spawns = Vec::new();
    for r in rows {
        let get = |c: &str| r.get(c).map(Sql::text).unwrap_or_default();
        let num = |c: &str| get(c).parse::<f64>().unwrap_or(0.0);
        let uint = |c: &str| num(c).max(0.0) as u32;
        let territory = match r.get("TerritoryName") {
            Some(name) => {
                let name = name.text().to_owned();
                *territories.entry(name.clone()).or_insert_with(|| data.territory_by_name(&name))
            }
            // the first dump has the id instead
            None => Some(uint("TerritoryType") as u16).filter(|t| data.has_territory(*t)),
        };
        let Some(territory) = territory else {
            let key = r.get("TerritoryName").map_or_else(|| get("TerritoryType").to_owned(), |n| n.text().to_owned());
            *report.unknown_territory.entry(key).or_default() += 1;
            continue;
        };
        let (base_id, name_id) = (uint("BaseId"), uint("NameId"));
        if !bases.contains(&base_id) {
            report.unknown_base += 1;
            continue;
        }
        if !names.contains(&name_id) {
            report.unknown_name += 1;
            continue;
        }
        spawns.push(BnpcSpawn {
            territory,
            instance_id: Some(uint("instanceId")),
            layer: get("GroupName").to_owned(),
            base_id,
            name_id,
            level: uint("Level") as u16,
            pos: [num("x") as f32, num("y") as f32, num("z") as f32],
            rotation: num("Rotation") as f32,
            wander_range: num("WanderingRange") as f32,
            respawn_secs: 0,
            pop_weather: uint("PopWeather"),
            pop_time_start: uint("PopTimeStart"),
            pop_time_end: uint("PopTimeEnd"),
            loot_table: uint("DropItem"),
            link_group: uint("LinkGroup"),
            link_family: uint("LinkFamily"),
            link_range: uint("LinkRange"),
            normal_ai: uint("NormalAI"),
            source: SAPPHIRE.into(),
        });
        *report.per_territory.entry(territory).or_default() += 1;
    }
    // a dump can list one instance twice (several migrations in one file); the last one wins
    let mut seen = HashSet::new();
    spawns.reverse();
    spawns.retain(|s| seen.insert((s.territory, s.instance_id)));
    spawns.reverse();
    report.imported = spawns.len();
    Ok((spawns, report))
}

pub async fn run_sapphire_bnpcs(db: &Databases, data: &WorldData, file: &Path) -> anyhow::Result<()> {
    let dump = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let (spawns, report) = sapphire_bnpcs(data, &dump)?;
    let written = db.world.replace_bnpc_spawns(BETA_PATCH, SAPPHIRE, &spawns).await?;
    println!("{} rows read, {written} spawns written (patch {BETA_PATCH}, source {SAPPHIRE})", report.read);
    println!(
        "left out: {} in territories this build lacks, {} with no BNpcBase row, {} with no BNpcName row",
        report.unknown_territory.values().sum::<usize>(),
        report.unknown_base,
        report.unknown_name
    );
    if !report.unknown_territory.is_empty() {
        let mut missing: Vec<_> = report.unknown_territory.iter().collect();
        missing.sort_by(|a, b| b.1.cmp(a.1));
        let list: Vec<String> = missing.iter().take(15).map(|(t, n)| format!("{t} ({n})")).collect();
        println!(
            "  territories not in this build: {}{}",
            list.join(", "),
            if missing.len() > 15 { ", ..." } else { "" }
        );
    }
    for (t, n) in &report.per_territory {
        let name = data
            .sheet("TerritoryType")
            .ok()
            .and_then(|s| s.row(*t as u32).and_then(|r| r.first()).and_then(|v| v.as_str().map(str::to_owned)))
            .unwrap_or_default();
        println!("  {t:5} {name:8} {n}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mysql_dumps() {
        let dump = "-- comment\nINSERT INTO `battlenpc` (`GroupId`, `TerritoryName`, `x`, `name`) VALUES\n\
                    (1, ' f1f1', '42.705971', ' bnpc1'),\n(2, 'it''s', '-1.5', NULL);\n\
                    INSERT INTO `other` (`a`) VALUES (1);\n\
                    INSERT INTO `battlenpc` (`GroupId`, `TerritoryName`, `x`, `name`) VALUES (3, 'a\\'b', '0', 'c');";
        let rows = inserts(dump, "battlenpc").unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["TerritoryName"].text(), "f1f1");
        assert_eq!(rows[0]["x"].text(), "42.705971");
        assert_eq!(rows[1]["TerritoryName"].text(), "it's");
        assert_eq!(rows[1]["name"], Sql::Null);
        assert_eq!(rows[2]["TerritoryName"].text(), "a'b");
        assert!(inserts("INSERT INTO `battlenpc` (`a`, `b`) VALUES (1);", "battlenpc").is_err());
    }
}
