//! `crystal-tools import derive-quests`: quest definitions from the β's own data, for the quests no
//! other source covers (an existing definition is never replaced). Tagged `derived`: built by
//! rules from facts, unverified until played.
//!
//! The facts: the `Quest` row (start ENpc col 27, end ENpc col 29, listeners and the sequence each
//! is active at, cols 132/260) and the quest's script, whose scene functions show what each does
//! (`crystal_data::luac`): the one calling `QuestOffer` returns 1 when the player accepts, the one
//! calling `QuestAccepted` closes the offer, the one calling `QuestReward` returns (accepted, the
//! reward chosen) (ManFst001_00039, ManSea001_00107: so in the β's own scripts).
//!
//! The rules (each matches Sapphire's hand-written flow for Coming to Gridania and Coming to Limsa
//! Lominsa):
//! - offer: the start ENpc at sequence 0 plays the `QuestOffer` scene; accepted, the scenes after it
//!   up to the `QuestAccepted` one in order; then the quest moves to its first listener sequence
//!   (255 when every listener waits for the end);
//! - turn-in: the end ENpc at 255 plays the `QuestReward` scene, the scene before it first when that
//!   one plays a cutscene or talks; accepted, the quest completes with the reward chosen;
//! - anything a listener does at another sequence is left in `review`: which scene it plays the data
//!   cannot tell.

use std::collections::BTreeMap;
use std::path::Path;

use crystal_common::quest::{Action, Cond, Op, QuestDef, Rule, Source, Talk};
use crystal_data::WorldData;

use crate::quest::listeners;

pub fn derive(data: &WorldData, quest_id: u32) -> Option<QuestDef> {
    let sheet = data.sheet("Quest").ok()?;
    let row = sheet.row(quest_id)?;
    let script_name = row.get(1)?.as_str()?.to_owned();
    let start = row.get(27)?.as_u64()? as u32;
    let end = row.get(29)?.as_u64()? as u32;
    let script = data.quest_script(quest_id)?;
    let scenes = script.scenes();
    let calls = |n: u16, what: &str| scenes.get(&n).is_some_and(|f| f.uses.iter().any(|u| u == what));
    let first = |what: &str| scenes.keys().copied().find(|&n| calls(n, what));
    let l = listeners(data, quest_id)?;
    let mut review = Vec::new();
    let mut talks = Vec::new();
    let mut rules: BTreeMap<u16, Vec<Rule>> = BTreeMap::new();
    let accepted_1 = Cond::Result { index: 0, op: Op::Eq, value: 1 };

    // the offer
    let next_seq = l.entries.iter().map(|e| e.1).filter(|&s| s != 255).min().unwrap_or(255);
    match (first("QuestOffer"), first("QuestAccepted")) {
        // (one scene may both offer and accept: ClsAlc050_00578's scene 0)
        (Some(offer), Some(accepted)) if offer <= accepted && start != 0 => {
            talks.push(Talk { actor: start, when: vec![Cond::Seq { op: Op::Eq, value: 0 }], scene: offer, action: None });
            let chain: Vec<u16> = (offer..=accepted).filter(|n| scenes.get(n).is_some_and(|f| !f.uses.is_empty())).collect();
            for pair in chain.windows(2) {
                let when = if pair[0] == offer { vec![accepted_1.clone()] } else { vec![] };
                rules.insert(pair[0], vec![Rule { when, then: vec![Action::Play { scene: pair[1] }] }]);
            }
            let when = if accepted == offer { vec![accepted_1.clone()] } else { vec![] };
            rules.insert(accepted, vec![Rule { when, then: vec![Action::SetSeq { seq: next_seq }] }]);
        }
        _ => review.push("no QuestOffer/QuestAccepted scene pair: the offer not derived".to_owned()),
    }

    // the turn-in
    match first("QuestReward") {
        Some(reward) if end != 0 => {
            let lead = reward.checked_sub(1).filter(|&p| {
                (calls(p, "PlayCutScene") || calls(p, "Talk")) && !calls(p, "QuestOffer") && !calls(p, "QuestAccepted")
            });
            let first_scene = lead.unwrap_or(reward);
            talks.push(Talk { actor: end, when: vec![Cond::Seq { op: Op::Eq, value: 255 }], scene: first_scene, action: None });
            if let Some(p) = lead {
                rules.insert(p, vec![Rule { when: vec![], then: vec![Action::Play { scene: reward }] }]);
            }
            rules.insert(reward, vec![Rule { when: vec![accepted_1], then: vec![Action::Complete { choice: Some(1) }] }]);
        }
        _ => review.push("no QuestReward scene: the turn-in not derived".to_owned()),
    }

    // what the data cannot say
    for (who, seq) in &l.entries {
        if *seq != 255 && *seq != 0 {
            review.push(format!("listener {who} at sequence {seq}: which scene it plays is not in the data"));
        }
    }
    for (who, seq) in &l.entries {
        if *seq == 255 && *who != end {
            review.push(format!("listener {who} at sequence 255 (not the end ENpc): its scene is not in the data"));
        }
    }
    Some(QuestDef { quest: quest_id, script: script_name, source: Source::Derived, talks, scenes: rules, review })
}

/// A derived definition for every β quest with a script and no definition in `out`; the quests
/// written.
pub fn run(data: &WorldData, out: &Path) -> anyhow::Result<Vec<u32>> {
    std::fs::create_dir_all(out)?;
    let sheet = data.sheet("Quest")?;
    let mut written = Vec::new();
    for (id, r) in &sheet.rows {
        let Some(script) = r.get(1).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else { continue };
        let path = out.join(format!("{script}.json"));
        if path.exists() {
            continue;
        }
        if let Some(def) = derive(data, *id) {
            std::fs::write(&path, serde_json::to_string_pretty(&def)? + "\n")?;
            written.push(*id);
        }
    }
    Ok(written)
}
