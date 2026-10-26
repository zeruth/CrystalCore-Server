//! The client's game data, read from its install (`game/sqpack`), as the server needs it: EXD
//! sheets (`TerritoryType`, `Level`, ...) and LGB layouts (zone exits, pop ranges, NPCs).
//!
//! The formats are the ones every ARR client uses; this build's specifics are noted where they
//! matter. Nothing here is copied into the server's databases: the install is the source of truth.

pub mod exd;
pub mod lgb;
pub mod sqpack;
pub mod world;

pub use exd::{Column, ColumnKind, Header, Language};
pub use exd::{Sheet, Value};
pub use lgb::{ExitRange, Layer, Layout, Object};
pub use sqpack::SqPack;
pub use world::{Arrival, Misfit, NpcPlacement, Overrides, WorldData};

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("{path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("{0}: not in the game data")]
    NotFound(String),
    #[error("{path}: {what}")]
    Format { path: String, what: String },
}

pub type Result<T> = std::result::Result<T, DataError>;

pub(crate) fn format(path: &str, what: impl Into<String>) -> DataError {
    DataError::Format { path: path.to_owned(), what: what.into() }
}
