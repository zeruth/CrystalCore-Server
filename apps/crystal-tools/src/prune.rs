//! `crystal-tools prune`: the world database's content checked against this client build, the
//! same way imports check it: battle NPC spawns (territory, `BNpcBase`, `BNpcName`, layout
//! layer) and EXD overrides (sheet, column, value type). Without `--apply` it only reports.

use std::collections::BTreeMap;

use crystal_data::WorldData;
use crystal_db::{BETA_PATCH, BnpcSpawn, Databases};

pub async fn run(db: &Databases, data: &WorldData, apply: bool) -> anyhow::Result<()> {
    // battle NPC spawns
    let spawns = db.world.bnpc_spawns(BETA_PATCH, None).await?;
    let mut keep: BTreeMap<String, Vec<BnpcSpawn>> = BTreeMap::new();
    let mut misfits: BTreeMap<String, usize> = BTreeMap::new();
    let mut dropped = 0;
    for s in spawns.iter() {
        keep.entry(s.source.clone()).or_default();
        match data.bnpc_misfit(s.territory, s.base_id, s.name_id, &s.layer)? {
            None => keep.get_mut(&s.source).unwrap().push(s.clone()),
            Some(why) => {
                *misfits.entry(why.to_string()).or_default() += 1;
                dropped += 1;
            }
        }
    }
    println!("battle NPC spawns: {} checked, {dropped} do not belong in this build", spawns.len());
    for (why, n) in &misfits {
        println!("  {n:6} {why}");
    }

    // EXD overrides
    let names = data.pack().sheet_names()?;
    let overrides = db.world.exd_overrides(BETA_PATCH).await?;
    let mut bad = Vec::new();
    for o in &overrides {
        let why = match names.iter().find(|n| **n == o.sheet) {
            None => Some("no such sheet"),
            Some(_) => {
                let mut sheet = (*data.sheet(&o.sheet)?).clone();
                (!sheet.set(o.row_id, o.col as usize, &o.value)).then_some("the column does not take the value")
            }
        };
        if let Some(why) = why {
            println!("  override {} {} {} = {:?}: {why}", o.sheet, o.row_id, o.col, o.value);
            bad.push(o);
        }
    }
    println!("EXD overrides: {} checked, {} do not fit this build", overrides.len(), bad.len());

    if !apply {
        if dropped + bad.len() > 0 {
            println!("nothing removed: run again with --apply to remove them");
        }
        return Ok(());
    }
    if dropped > 0 {
        for (source, kept) in &keep {
            db.world.replace_bnpc_spawns(BETA_PATCH, source, kept).await?;
        }
    }
    for o in bad {
        db.world.remove_exd_override(BETA_PATCH, &o.sheet, o.row_id, o.col).await?;
    }
    println!("removed");
    Ok(())
}
