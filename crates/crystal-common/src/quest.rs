//! Quest definitions: what the server does when a quest's events happen, as data
//! (`data/<build>/quests/<script>.json`), so no quest is code.
//!
//! The β client carries each quest's scenes (its compiled Lua) and its `Quest` row (listeners,
//! sequences, to-dos, rewards); which talk plays which scene, and what a scene's result does, was
//! in Square's server scripts, which never shipped. A definition holds that part, from where it
//! came: `sapphire` (imported from Sapphire's quest scripts, 4.0 era) or `derived` (from the β's
//! own `Quest` row and script: the offer and turn-in chains). Statements a source had that this
//! format cannot express are kept in `review`, so nothing is silently dropped.
//!
//! The quest's state is what the client keeps per quest (its slot, @0xa50040): the sequence
//! (`GetQuestSequence`, slot +6) and six bytes `UI8A`-`UI8F` (slot +8 - +0xd; `..H`/`..L` their
//! high and low nibbles; `BitFlag8` bit n is byte F's 0x80 >> (n - 1)).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestDef {
    /// `Quest` row (0x10000 + n).
    pub quest: u32,
    /// The client's script (`Quest` col 1).
    pub script: String,
    pub source: Source,
    /// Which scene talking to an actor plays: the first rule whose actor and conditions match.
    pub talks: Vec<Talk>,
    /// What each scene's end does: the first rule whose conditions match; no rule (or no
    /// matching one) ends the event.
    pub scenes: BTreeMap<u16, Vec<Rule>>,
    /// What the source did that this format cannot say yet, for a person to look at.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Sapphire,
    Derived,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Talk {
    /// The ENpc (or EObj) talked to.
    pub actor: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<Cond>,
    pub scene: u16,
    /// An event action that runs first (its cast bar; 1 is the scripts' `EVENT_ACTION_SEARCH`),
    /// the scene playing when it completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<u32>,
    /// What the talk does before its scene (attune an aetheryte, unlock something).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub then: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<Cond>,
    pub then: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "if")]
pub enum Cond {
    /// The quest's sequence (0: not accepted).
    Seq { op: Op, value: u8 },
    /// A value the scene returned (index 0 first).
    Result { index: u8, op: Op, value: u32 },
    /// One of the quest's variables.
    Var { var: Var, op: Op, value: u8 },
    /// A `BitFlag8` bit (1-based).
    Flag { index: u8, set: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    pub fn test(self, a: u32, b: u32) -> bool {
        match self {
            Op::Eq => a == b,
            Op::Ne => a != b,
            Op::Lt => a < b,
            Op::Le => a <= b,
            Op::Gt => a > b,
            Op::Ge => a >= b,
        }
    }
}

/// A quest variable: a whole byte (`UI8A`) or one of its nibbles (`UI8AH`, `UI8AL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Var {
    /// 0-5: A-F.
    pub byte: u8,
    pub part: Part,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    Byte,
    High,
    Low,
}

impl Var {
    /// `UI8A`, `UI8AH`, `UI8AL` ... `UI8FL`.
    pub fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix("UI8")?;
        let mut c = rest.chars();
        let byte = match c.next()? {
            l @ 'A'..='F' => l as u8 - b'A',
            _ => return None,
        };
        let part = match c.next() {
            None => Part::Byte,
            Some('H') => Part::High,
            Some('L') => Part::Low,
            _ => return None,
        };
        c.next().is_none().then_some(Var { byte, part })
    }

    pub fn get(self, vars: &[u8; 6]) -> u8 {
        let b = vars[self.byte as usize];
        match self.part {
            Part::Byte => b,
            Part::High => b >> 4,
            Part::Low => b & 0xf,
        }
    }

    pub fn set(self, vars: &mut [u8; 6], value: u8) {
        let b = &mut vars[self.byte as usize];
        *b = match self.part {
            Part::Byte => value,
            Part::High => (*b & 0x0f) | (value << 4),
            Part::Low => (*b & 0xf0) | (value & 0x0f),
        };
    }
}

/// `BitFlag8` bit `index` (1-based): byte F, 0x80 >> (index - 1) (@0x729d30).
pub fn flag(vars: &[u8; 6], index: u8) -> bool {
    index >= 1 && vars[5] & (0x80 >> ((index - 1) & 7)) != 0
}

pub fn set_flag(vars: &mut [u8; 6], index: u8, set: bool) {
    if index >= 1 {
        let bit = 0x80 >> ((index - 1) & 7);
        vars[5] = if set { vars[5] | bit } else { vars[5] & !bit };
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "do")]
pub enum Action {
    /// Play another scene of this quest.
    Play { scene: u16 },
    /// Set the sequence; on a quest not yet the character's, accepts it.
    SetSeq { seq: u8 },
    SetVar { var: Var, value: u8 },
    /// Add to a variable (a counter: `quest.setUI8AL( quest.getUI8AL() + 1 )`).
    AddVar { var: Var, delta: i8 },
    SetFlag { index: u8, set: bool },
    /// The client's quest progress notice (Sapphire's `sendEventNotice`: its index and values);
    /// sent once its β packet is known.
    Notice { index: u8, values: Vec<u32> },
    /// Complete the quest: rewards from its `Quest` row; the optional reward chosen is the
    /// scene's result `choice`.
    Complete { choice: Option<u8> },
    /// Move the character's town opening to this sequence (`OPENING_SEQ_*`).
    OpeningSeq { seq: u8 },
    /// Play a scene of the character's town opening, nested in this event (kind 7).
    OpeningScene { scene: u16 },
    /// End the event now.
    Finish,
    /// Move the character (after the event ends).
    Warp { territory: u16, pos: [f32; 3], rotation: f32 },
    /// Move the character to another territory where it stands (after the event ends): from a
    /// town's opening copy to the town (183 `f1t1_opening` → 132 `f1t1`, the same map), as
    /// Sapphire's `requestMoveTerritoryType` without a position keeps the player's.
    MoveTerritory { territory: u16 },
    /// Attune the character to an aetheryte (`Aetheryte` row).
    Attune { aetheryte: u32 },
    /// Unlock something for the character (1 Return, 4 Teleport).
    Unlock { id: u32 },
}

/// Every definition in `dir` (`*.json`), by quest id; a file that does not read is an error
/// naming it.
pub fn load_dir(dir: &std::path::Path) -> Result<std::collections::HashMap<u32, QuestDef>, String> {
    let mut defs = std::collections::HashMap::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let def: QuestDef = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        defs.insert(def.quest, def);
    }
    Ok(defs)
}

/// The talk rule for `actor` that holds, given the quest's sequence and variables.
pub fn talk_for<'a>(def: &'a QuestDef, actor: u32, seq: u8, vars: &[u8; 6]) -> Option<&'a Talk> {
    def.talks.iter().find(|t| t.actor == actor && t.when.iter().all(|c| c.holds(seq, vars, &[])))
}

/// The scene rule for `scene` that holds, given the quest's state and the scene's result.
pub fn rule_for<'a>(def: &'a QuestDef, scene: u16, seq: u8, vars: &[u8; 6], result: &[u32]) -> Option<&'a Rule> {
    def.scenes.get(&scene)?.iter().find(|r| r.when.iter().all(|c| c.holds(seq, vars, result)))
}

impl Cond {
    pub fn holds(&self, seq: u8, vars: &[u8; 6], result: &[u32]) -> bool {
        match *self {
            Cond::Seq { op, value } => op.test(seq as u32, value as u32),
            Cond::Result { index, op, value } => {
                result.get(index as usize).is_some_and(|&r| op.test(r, value))
            }
            Cond::Var { var, op, value } => op.test(var.get(vars) as u32, value as u32),
            Cond::Flag { index, set } => flag(vars, index) == set,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_as_the_client_keeps_them() {
        let mut v = [0u8; 6];
        Var::parse("UI8AL").unwrap().set(&mut v, 1);
        Var::parse("UI8AH").unwrap().set(&mut v, 3);
        assert_eq!(v[0], 0x31);
        assert_eq!(Var::parse("UI8AH").unwrap().get(&v), 3);
        assert_eq!(Var::parse("UI8A").unwrap().get(&v), 0x31);
        set_flag(&mut v, 1, true);
        assert_eq!(v[5], 0x80);
        assert!(flag(&v, 1) && !flag(&v, 2));
        assert_eq!(Var::parse("UI8G"), None);
    }

    #[test]
    fn shipped_definitions_read_and_run() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/672261/quests");
        let defs = load_dir(&dir).unwrap();
        assert!(defs.len() > 400, "{}", defs.len());
        // Coming to Gridania, as played: offered by Bertennant, accepted, turned in to Miounne
        let def = &defs[&0x10027];
        let none = [0; 6];
        assert_eq!(talk_for(def, 1001148, 0, &none).unwrap().scene, 0);
        assert_eq!(rule_for(def, 0, 0, &none, &[1, 0]).unwrap().then[1], Action::Play { scene: 1 });
        assert!(rule_for(def, 0, 0, &none, &[0]).is_none());
        assert_eq!(rule_for(def, 2, 0, &none, &[1]).unwrap().then[0], Action::SetSeq { seq: 255 });
        assert_eq!(talk_for(def, 1001140, 255, &none).unwrap().scene, 4);
        assert_eq!(rule_for(def, 5, 255, &none, &[1, 0]).unwrap().then, vec![Action::Complete { choice: Some(1) }]);
    }

    #[test]
    fn definitions_round_trip() {
        let def = QuestDef {
            quest: 0x10027,
            script: "ManFst001_00039".into(),
            source: Source::Sapphire,
            talks: vec![Talk { actor: 1001148, when: vec![Cond::Seq { op: Op::Eq, value: 0 }], scene: 0, action: None, then: vec![] }],
            scenes: [(0, vec![Rule {
                when: vec![Cond::Result { index: 0, op: Op::Eq, value: 1 }],
                then: vec![Action::Play { scene: 1 }],
            }])]
            .into(),
            review: vec![],
        };
        let json = serde_json::to_string(&def).unwrap();
        assert_eq!(serde_json::from_str::<QuestDef>(&json).unwrap(), def);
    }
}
