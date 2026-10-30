//! Where gathering points stand (`data/<build>/gathering.json`).
//!
//! The β places no gathering point in its layouts: every territory's `planlive.lgb` has the
//! gathering layers (`CRF_LOGGING_LV05`, `CRF_MINING_LV35`, ...) with no objects in them, and
//! the server spawns each point as an object (down 0x19d, kind 6, its `GatheringPoint` row). So
//! the positions are server data, from the best source there is, each tagged with it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    /// `GatheringPoint` row (30000 + n).
    pub point: u32,
    /// Its `GatheringPointBase` (col 1).
    pub base: u32,
    pub territory: u16,
    pub pos: [f32; 3],
    #[serde(default)]
    pub rotation: f32,
    /// Where the position came from.
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Placements {
    /// How the file was made, for whoever replaces a guess with a measurement.
    #[serde(default)]
    pub notes: Vec<String>,
    pub points: Vec<Placement>,
}

impl Placements {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}
