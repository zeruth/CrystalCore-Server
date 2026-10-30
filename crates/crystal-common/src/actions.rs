//! What player actions do beyond the `Action` sheet's columns (`data/<build>/actions.json`, made
//! by `tools/sapphire_actions.py`): combos, and the statuses they put on. From the β's own
//! descriptions first (2.0's potencies and durations), Sapphire's action table for the status ids
//! and modifiers the descriptions only name.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActionExtra {
    #[serde(default)]
    pub name: String,
    /// The action it follows in a combo, and its potency when it does.
    #[serde(default)]
    pub combo_from: Option<u16>,
    #[serde(default)]
    pub combo_potency: u32,
    #[serde(default)]
    pub statuses: Vec<StatusEffect>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusEffect {
    /// "target" or "self".
    pub on: String,
    /// `Status` row.
    pub id: u16,
    pub duration_ms: u32,
    /// "damage" or "heal" over time, with its potency each tick.
    #[serde(default)]
    pub tick: Option<String>,
    #[serde(default)]
    pub potency: u32,
    /// Sapphire's ParamModifier names and values (DamageDealtPercent, ...).
    #[serde(default)]
    pub modifiers: HashMap<String, i32>,
}

/// The file's actions by id.
pub fn load(path: &std::path::Path) -> Result<HashMap<u32, ActionExtra>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw: HashMap<String, ActionExtra> = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    raw.into_iter()
        .map(|(k, v)| k.parse::<u32>().map(|id| (id, v)).map_err(|e| format!("{}: action {k}: {e}", path.display())))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_repositorys_file_loads() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/672261/actions.json");
        let actions = super::load(&path).unwrap();
        // Savage Blade follows Fast Blade; Aero's damage over time
        assert_eq!(actions[&11].combo_from, Some(9));
        assert_eq!(actions[&11].combo_potency, 200);
        let aero = &actions[&121].statuses[0];
        assert_eq!((aero.id, aero.tick.as_deref(), aero.potency, aero.duration_ms), (143, Some("damage"), 25, 18_000));
    }
}
