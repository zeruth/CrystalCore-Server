//! `crystal-tools quest`: a quest as the β has it (its `Quest` row's listeners, to-dos and
//! rewards, its script's scenes and what each calls), or with `--stats` what every quest script
//! calls, for the quest engine's definitions.

use std::collections::BTreeMap;

use crystal_data::WorldData;

/// The `Quest` row's per-listener arrays (64 entries each; the β's offsets, cols 132-1331).
pub struct Listeners {
    /// (listener: an ENpc, EObj or other id; the sequence it is active at; @132 and @260)
    pub entries: Vec<(u32, u8)>,
    /// To-dos: (the sequence that completes it, its count, its marker's `Level` row), @1092,
    /// @1116, @1140.
    pub todos: Vec<(u8, u8, u32)>,
}

pub fn listeners(data: &WorldData, quest_id: u32) -> Option<Listeners> {
    let sheet = data.sheet("Quest").ok()?;
    let r = sheet.row(quest_id)?;
    let u = |i: usize| r.get(i).and_then(|v| v.as_u64()).unwrap_or(0);
    let entries = (0..64).filter(|i| u(260 + i) != 0).map(|i| (u(260 + i) as u32, u(132 + i) as u8)).collect();
    let todos = (0..24).filter(|i| u(1092 + i) != 0).map(|i| (u(1092 + i) as u8, u(1116 + i) as u8, u(1140 + i * 8) as u32)).collect();
    Some(Listeners { entries, todos })
}

pub fn run(data: &WorldData, quest: Option<u32>, stats: bool) -> anyhow::Result<()> {
    if stats {
        let sheet = data.sheet("Quest")?;
        let mut uses: BTreeMap<String, usize> = BTreeMap::new();
        let (mut quests, mut with_script) = (0, 0);
        for (id, r) in &sheet.rows {
            if r.first().and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                continue;
            }
            quests += 1;
            let Some(script) = data.quest_script(*id) else { continue };
            with_script += 1;
            let mut seen: Vec<&str> = Vec::new();
            for f in script.scenes().values() {
                for u in &f.uses {
                    if u.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                        && !u.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
                        && !seen.contains(&u.as_str())
                    {
                        seen.push(u);
                    }
                }
            }
            for u in seen {
                *uses.entry(u.to_owned()).or_insert(0) += 1;
            }
        }
        println!("{quests} quests, {with_script} with a script; methods their scenes call, by quests using them:");
        let mut v: Vec<_> = uses.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (name, n) in v {
            println!("{n:5} {name}");
        }
        return Ok(());
    }
    let id = quest.ok_or_else(|| anyhow::anyhow!("a quest id, or --stats"))?;
    let sheet = data.sheet("Quest")?;
    let r = sheet.row(id).ok_or_else(|| anyhow::anyhow!("no Quest row {id:#x}"))?;
    let s = |i: usize| r.get(i).and_then(|v| v.as_str()).unwrap_or("").to_owned();
    let u = |i: usize| r.get(i).and_then(|v| v.as_u64()).unwrap_or(0);
    println!("{id:#x} {} ({}): start ENpc {}, end ENpc {}, {} gil, exp factor {}", s(0), s(1), u(27), u(29), u(1334), u(1333));
    let vars: Vec<String> = (32..82).filter(|&i| !s(i).is_empty()).map(|i| format!("{}={}", s(i), u(i + 50))).collect();
    println!("variables: {}", vars.join(" "));
    if let Some(l) = listeners(data, id) {
        for (who, seq) in &l.entries {
            println!("listener {who} at sequence {seq}");
        }
        for (seq, n, level) in &l.todos {
            println!("to-do: done at sequence {seq}, count {n}, marker Level {level}");
        }
    }
    match data.quest_script(id) {
        Some(script) => {
            for (n, f) in script.scenes() {
                let calls: Vec<_> = f.uses.iter().filter(|u| !u.starts_with("TEXT_")).cloned().collect();
                println!("scene {n:3}: returns {:?} · {}", f.returns, calls.join(" "));
            }
        }
        None => println!("no script in the install"),
    }
    Ok(())
}
