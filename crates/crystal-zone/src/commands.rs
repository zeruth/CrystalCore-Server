//! Chat commands, in AzerothCore's style: what a player says starting with [`PREFIX`] runs a
//! command instead of being heard. Modules (`modules/mod-gm`) register them; the zone service runs
//! them with a [`Context`] that can answer the player and move it.
//!
//! Who may run what is the account's GM level (`login.account.gm_level`, AzerothCore's security
//! level): each command needs at least its [`Command::level`]. Players (level 0) have none, so what
//! they say is always chat. A GM also has GM mode, on when it logs in and switched with `.gm on`/
//! `.gm off`; with it off only [`ALWAYS`] commands run.

use std::sync::Arc;

use crystal_data::WorldData;
use crystal_world::{ActorId, World};
use futures_util::future::BoxFuture;

/// Text said with this first is a command.
pub const PREFIX: char = '.';

/// Commands that run with GM mode off (switching it back on).
pub const ALWAYS: &[&str] = &["gm"];

pub trait Command: Send + Sync {
    /// What follows the prefix, lowercase.
    fn name(&self) -> &'static str;
    /// The arguments, for help: `<territory> [x y z]`.
    fn usage(&self) -> &'static str {
        ""
    }
    fn help(&self) -> &'static str;
    /// The GM level the account needs.
    fn level(&self) -> u8 {
        1
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, args: &'a str) -> BoxFuture<'a, ()>;
}

/// The commands a zone service runs.
#[derive(Default, Clone)]
pub struct Commands {
    list: Vec<Arc<dyn Command>>,
}

impl Commands {
    pub fn register(&mut self, command: impl Command + 'static) {
        self.list.push(Arc::new(command));
    }

    pub fn list(&self) -> &[Arc<dyn Command>] {
        &self.list
    }

    pub fn find(&self, name: &str) -> Option<&Arc<dyn Command>> {
        self.list.iter().find(|c| c.name().eq_ignore_ascii_case(name))
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// The player running a command.
#[derive(Debug, Clone)]
pub struct Player {
    pub actor: ActorId,
    pub name: String,
    pub territory: u16,
    pub pos: [f32; 3],
    pub rotation: f32,
    /// The account's GM level.
    pub gm_level: u8,
    /// Whether GM mode is on.
    pub gm_mode: bool,
}

/// Where a command sends the player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Teleport {
    pub territory: u16,
    pub pos: [f32; 3],
    pub rotation: f32,
}

pub struct Context<'a> {
    pub player: Player,
    pub world: &'a World,
    /// The client's game data, when configured.
    pub data: Option<&'a WorldData>,
    pub commands: &'a Commands,
    pub(crate) replies: Vec<String>,
    pub(crate) teleport: Option<Teleport>,
    pub(crate) gm_mode: Option<bool>,
}

impl<'a> Context<'a> {
    pub fn new(player: Player, world: &'a World, data: Option<&'a WorldData>, commands: &'a Commands) -> Self {
        Self { player, world, data, commands, replies: vec![], teleport: None, gm_mode: None }
    }

    /// A line in the player's chat log.
    pub fn reply(&mut self, text: impl Into<String>) {
        self.replies.push(text.into());
    }

    /// Sends the player to `territory` (it loads it, even when it is the one it is in).
    pub fn teleport(&mut self, territory: u16, pos: [f32; 3], rotation: f32) {
        self.teleport = Some(Teleport { territory, pos, rotation });
    }

    /// Switches GM mode for the rest of the session.
    pub fn set_gm_mode(&mut self, on: bool) {
        self.gm_mode = Some(on);
    }

    pub fn replies(&self) -> &[String] {
        &self.replies
    }

    pub fn teleport_to(&self) -> Option<Teleport> {
        self.teleport
    }
}

/// Splits `.name args` into the name and the rest; `None` when `text` is not a command.
pub fn parse(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix(PREFIX)?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric())).then_some((name, args.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse(".tele 129 1 2 3"), Some(("tele", "129 1 2 3")));
        assert_eq!(parse(".who"), Some(("who", "")));
        assert_eq!(parse("hello"), None);
        assert_eq!(parse("..."), None);
        assert_eq!(parse(". hi"), None);
    }
}
