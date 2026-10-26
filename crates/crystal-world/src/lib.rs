//! The world model. Each territory is one task ([`zone`]) that owns its actors, the players in it
//! and its event NPCs, as plain structs; everything else talks to it through messages
//! ([`ZoneHandle`]), so game logic takes no locks. A player's connection enters a territory, sends
//! it the player's moves and chat, and receives [`Event`]s: who appears and goes away around the
//! player (by range, within the client's 99 free character slots), how they move, what they say.
//!
//! Nothing here knows the wire format: the zone service turns events into the client build's
//! packets.

pub mod inventory;
pub mod zone;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::zone::{Request, ZoneState};

pub type ActorId = u32;

/// Event NPC actor ids: this, the territory << 12, and the NPC's index in the territory (from 1).
pub const NPC_ACTOR_BASE: u32 = 0x4000_0000;
/// Battle NPC actor ids: this, the territory << 14, and the spawn's index in the territory (from 1).
pub const BNPC_ACTOR_BASE: u32 = 0x5000_0000;
/// Characters are spawned for a player within this distance (yalms)...
pub const SPAWN_RANGE: f32 = 60.0;
/// ...and despawned beyond this one, so walking along the edge does not flicker.
pub const DESPAWN_RANGE: f32 = 70.0;
/// How far "say" carries; shout and yell reach the whole territory.
pub const SAY_RANGE: f32 = 20.0;
/// The client keeps 100 characters (@0xa64860); slot 0 is the player.
pub const SLOTS: usize = 100;
/// How often each territory recomputes who sees whom.
pub const VISIBILITY_INTERVAL: Duration = Duration::from_millis(250);
/// Events queued for one player before the territory gives up on it (its connection is too slow)
/// and drops it.
pub const OUTBOX: usize = 1024;
/// Requests queued for one territory; moves and chat beyond it are dropped.
pub const INBOX: usize = 8192;

/// What other players see of a player.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerLook {
    pub name: String,
    pub content_id: u64,
    pub class: u8,
    pub level: u8,
    pub customize: [u8; 26],
    pub main_weapon: u64,
    pub sub_weapon: u64,
    pub models: [u32; 10],
}

/// An event NPC the game data places in a territory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NpcPlacement {
    /// `ENpcBase` row: the client builds the NPC's look from it.
    pub enpc: u32,
    pub pos: [f32; 3],
    /// Radians.
    pub rotation: f32,
}

/// A battle NPC's spawn point (the world database's `bnpc_spawn`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BnpcPlacement {
    /// `BNpcBase` row: what it looks like.
    pub base_id: u32,
    /// `BNpcName` row.
    pub name_id: u32,
    pub level: u16,
    pub pos: [f32; 3],
    /// Radians.
    pub rotation: f32,
}

/// What the game data and the world database put in a territory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerritoryContent {
    pub enpcs: Vec<NpcPlacement>,
    pub bnpcs: Vec<BnpcPlacement>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActorKind {
    Player(Arc<PlayerLook>),
    EventNpc { enpc: u32 },
    BattleNpc { base_id: u32, name_id: u32, level: u16 },
}

/// An actor as a spawn shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ActorView {
    pub id: ActorId,
    pub kind: ActorKind,
    pub pos: [f32; 3],
    /// Radians.
    pub rotation: f32,
}

/// How a player moves, as its client reports it, passed on to those who see it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Motion {
    pub rotation_before_slip: f32,
    pub flags: u8,
    pub collision: u8,
}

/// Who hears a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRange {
    /// Players within [`SAY_RANGE`].
    Say,
    /// Everyone in the territory.
    Zone,
}

/// What a player's connection is told.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `actor` comes into view, in the client's character slot `slot` (1..100).
    Spawn {
        slot: u8,
        actor: ActorView,
    },
    /// The actor in `slot` goes away.
    Despawn {
        slot: u8,
        actor: ActorId,
    },
    Move {
        actor: ActorId,
        pos: [f32; 3],
        rotation: f32,
        motion: Motion,
    },
    Chat {
        actor: ActorId,
        content_id: u64,
        name: Arc<str>,
        kind: u16,
        text: Arc<str>,
    },
}

/// Who is online, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Presence {
    pub actor: ActorId,
    pub name: String,
    pub territory: u16,
}

/// What stands in a territory before any player comes: its event NPCs (the game data) and battle
/// NPCs (the world database).
pub type NpcSource = Box<dyn Fn(u16) -> TerritoryContent + Send + Sync>;

/// The territories of one zone service. Each is started on first use and runs until shutdown.
pub struct World {
    zones: Mutex<HashMap<u16, ZoneHandle>>,
    npcs: NpcSource,
    presence: Mutex<HashMap<ActorId, (u64, Presence)>>,
    entries: AtomicU64,
    shutdown: CancellationToken,
    tasks: TaskTracker,
}

/// A player in a territory, as [`World::enter`] put it there. Dropping it does not leave: call
/// [`World::leave`].
pub struct Entry {
    pub zone: ZoneHandle,
    pub id: ActorId,
    /// Tells this entry from a later one of the same character (a second login replaces the first).
    token: u64,
    /// What the territory tells the player. Closed when the territory dropped it (too slow, or
    /// replaced by a later login, or shutdown).
    pub events: mpsc::Receiver<Event>,
}

impl World {
    pub fn new(npcs: NpcSource, shutdown: CancellationToken) -> Self {
        Self {
            zones: Mutex::new(HashMap::new()),
            npcs,
            presence: Mutex::new(HashMap::new()),
            entries: AtomicU64::new(1),
            shutdown,
            tasks: TaskTracker::new(),
        }
    }

    /// The territory's task, started if it is not running. Must be called within a Tokio runtime.
    pub fn zone(&self, territory: u16) -> ZoneHandle {
        let mut zones = self.zones.lock();
        if let Some(z) = zones.get(&territory)
            && !z.tx.is_closed()
        {
            return z.clone();
        }
        let (tx, rx) = mpsc::channel(INBOX);
        let content = (self.npcs)(territory);
        let state = ZoneState::new(territory, &content.enpcs, &content.bnpcs);
        self.tasks.spawn(zone::run(state, rx, self.shutdown.clone()));
        let handle = ZoneHandle { territory, tx };
        zones.insert(territory, handle.clone());
        handle
    }

    /// Puts a player into `territory`. `None` when the territory is not running (shutdown).
    pub async fn enter(
        &self,
        territory: u16,
        id: ActorId,
        look: Arc<PlayerLook>,
        pos: [f32; 3],
        rotation: f32,
    ) -> Option<Entry> {
        let zone = self.zone(territory);
        let token = self.entries.fetch_add(1, Ordering::Relaxed);
        let (outbox, events) = mpsc::channel(OUTBOX);
        let name = look.name.clone();
        zone.tx.send(Request::Enter { id, token, look, pos, rotation, outbox }).await.ok()?;
        self.presence.lock().insert(id, (token, Presence { actor: id, name, territory }));
        Some(Entry { zone, id, token, events })
    }

    /// Takes the player out of its territory; the others there see it go.
    pub async fn leave(&self, entry: Entry) {
        let Entry { zone, id, token, .. } = entry;
        {
            let mut presence = self.presence.lock();
            if presence.get(&id).is_some_and(|(t, _)| *t == token) {
                presence.remove(&id);
            }
        }
        let _ = zone.tx.send(Request::Leave { id, token }).await;
    }

    /// Everyone online, by name.
    pub fn online(&self) -> Vec<Presence> {
        let mut all: Vec<_> = self.presence.lock().values().map(|(_, p)| p.clone()).collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }

    /// An online player by name, ignoring case.
    pub fn find(&self, name: &str) -> Option<Presence> {
        self.presence.lock().values().map(|(_, p)| p).find(|p| p.name.eq_ignore_ascii_case(name)).cloned()
    }

    /// Where an online player stands: its territory, position and rotation.
    pub async fn locate(&self, actor: ActorId) -> Option<(u16, [f32; 3], f32)> {
        let territory = self.presence.lock().get(&actor)?.1.territory;
        let (pos, rotation) = self.zone(territory).locate(actor).await?;
        Some((territory, pos, rotation))
    }

    /// Stops every territory (players' event channels close) and waits for them.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

/// Sends to one territory's task.
#[derive(Debug, Clone)]
pub struct ZoneHandle {
    pub territory: u16,
    tx: mpsc::Sender<Request>,
}

impl ZoneHandle {
    /// The player moved. Dropped when the territory is overloaded (the next move corrects it).
    pub fn moved(&self, id: ActorId, pos: [f32; 3], rotation: f32, motion: Motion) {
        let _ = self.tx.try_send(Request::Move { id, pos, rotation, motion });
    }

    /// The player said something. `kind` is passed on as is.
    pub fn chat(&self, id: ActorId, range: ChatRange, kind: u16, text: &str) {
        if let Err(e) = self.tx.try_send(Request::Chat { id, range, kind, text: text.into() }) {
            tracing::warn!(territory = self.territory, "chat dropped: {e}");
        }
    }

    /// Places a battle NPC in the territory now; its actor id (`None`: the territory is down).
    pub async fn add_bnpc(&self, bnpc: BnpcPlacement) -> Option<ActorId> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(Request::AddBnpc { bnpc, reply }).await.ok()?;
        answer.await.ok()
    }

    pub async fn locate(&self, id: ActorId) -> Option<([f32; 3], f32)> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(Request::Locate { id, reply }).await.ok()?;
        answer.await.ok().flatten()
    }
}
