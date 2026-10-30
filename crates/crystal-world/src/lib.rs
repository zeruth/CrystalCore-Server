//! The world model. Each territory is one task ([`zone`]) that owns its actors, the players in it
//! and its event NPCs, as plain structs; everything else talks to it through messages
//! ([`ZoneHandle`]), so game logic takes no locks. A player's connection enters a territory, sends
//! it the player's moves and chat, and receives [`Event`]s: who appears and goes away around the
//! player (by range, within the client's 99 free character slots), how they move, what they say.
//!
//! Nothing here knows the wire format: the zone service turns events into the client build's
//! packets.

pub mod combat;
pub mod inventory;
pub mod nav;
pub mod progress;
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
    pub hp_max: u32,
    pub mp_max: u32,
    /// Base attributes: strength, dexterity, vitality, intelligence, mind, piety.
    pub attributes: [u32; 6],
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
    /// Attacks players who come near.
    pub aggressive: bool,
    /// How far from its spawn point it wanders while idle, yalms (0: it stays).
    pub wander_range: f32,
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

impl Motion {
    /// [`Self::flags`]: walking (the client's PositionUpdate flag 2; a move with it plays at
    /// walking speed).
    pub const WALKING: u8 = 2;
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
    /// `source` hit `target` for `amount`; its HP now.
    Damage {
        source: ActorId,
        target: ActorId,
        /// The Action row (7: the auto-attack) and the player's request it answers (0: none).
        action: u16,
        sequence: u32,
        amount: u32,
        hp: u32,
        hp_max: u32,
        /// The target's level and class (for the client's HP update; 0 for battle NPCs).
        level: u16,
        class: u8,
        /// The target's statuses (the client's HP update replaces them all).
        statuses: Statuses,
        /// Its MP and TP (the client's HP update carries them).
        mana: Mana,
    },
    /// The actor's HP or statuses changed (healing, regeneration, a revive, a status on or off):
    /// its level and class too, for the client's HP update.
    Hp {
        actor: ActorId,
        hp: u32,
        hp_max: u32,
        level: u16,
        class: u8,
        statuses: Statuses,
        /// Its MP and TP (the client's HP update carries them).
        mana: Mana,
    },
    /// `actor` starts casting `action` on `target`, landing in `time`.
    Cast {
        actor: ActorId,
        action: u16,
        target: ActorId,
        time: std::time::Duration,
    },
    /// `actor`'s cast of `action` ended before it landed: cancelled by its own doing (it moved),
    /// or interrupted.
    CastEnd {
        actor: ActorId,
        action: u16,
        interrupted: bool,
    },
    /// `source` restored `amount` of `target`'s HP with `action`; its HP now.
    Heal {
        source: ActorId,
        target: ActorId,
        action: u16,
        sequence: u32,
        amount: u32,
        hp: u32,
        hp_max: u32,
        level: u16,
        class: u8,
        statuses: Statuses,
        /// Its MP and TP (the client's HP update carries them).
        mana: Mana,
    },
    /// The actor died.
    Death {
        actor: ActorId,
        /// Who dealt the last blow.
        killer: Option<ActorId>,
        /// The dead actor's level (for the killer's experience).
        level: u16,
    },
    /// The actor's gear changed (players; the actor's own client is told too).
    Look {
        actor: ActorId,
        look: Arc<PlayerLook>,
    },
    Chat {
        actor: ActorId,
        content_id: u64,
        name: Arc<str>,
        kind: u16,
        text: Arc<str>,
    },
}

/// The most statuses an actor has (the client's 30 slots).
pub const STATUS_SLOTS: usize = 30;

/// A status on an actor, as the client shows it: the Status row, its parameter (stacks, or
/// what the status reads), the seconds left, and who put it there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatusSlot {
    pub id: u16,
    pub param: u16,
    pub remaining: f32,
    pub source: ActorId,
}

/// An actor's statuses, in its slots' order (at most [`STATUS_SLOTS`]).
pub type Statuses = Arc<[StatusSlot]>;

/// An action a player uses, as the zone service read it from the Action sheet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActionUse {
    pub action: u16,
    /// The client's request, echoed in the result.
    pub sequence: u32,
    pub potency: u32,
    /// Yalms (melee: [`combat::MELEE_RANGE`]).
    pub range: f32,
    pub recast: std::time::Duration,
    /// On the global cooldown (weaponskills and spells) rather than its own.
    pub gcd: bool,
    /// How long it casts before it lands (zero: at once).
    pub cast: std::time::Duration,
    /// It restores HP (on the user or another player) rather than hurting a battle NPC.
    pub heal: bool,
    /// What it costs, paid when it lands (checked when it starts too).
    pub mp_cost: u32,
    pub tp_cost: u32,
}

/// A character's MP and TP, as its HP update carries them (zero for battle NPCs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mana {
    pub mp: u32,
    pub mp_max: u32,
    pub tp: u32,
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

/// Where a territory's battle NPCs can walk (built from its collision; `None`: nowhere known).
/// Called on a blocking thread when the territory starts.
pub type NavSource = std::sync::Arc<dyn Fn(u16) -> Option<nav::NavGrid> + Send + Sync>;

/// The territories of one zone service. Each is started on first use and runs until shutdown.
pub struct World {
    zones: Mutex<HashMap<u16, ZoneHandle>>,
    npcs: NpcSource,
    nav: Option<NavSource>,
    /// Grids built so far, kept for a territory that starts again.
    grids: std::sync::Arc<Mutex<HashMap<u16, std::sync::Arc<nav::NavGrid>>>>,
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
            nav: None,
            grids: Default::default(),
            presence: Mutex::new(HashMap::new()),
            entries: AtomicU64::new(1),
            shutdown,
            tasks: TaskTracker::new(),
        }
    }

    /// Where territories' battle NPCs can walk, built when each starts.
    pub fn with_nav(mut self, source: NavSource) -> Self {
        self.nav = Some(source);
        self
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
        // its walkable ground, built aside and handed over when ready (until then its battle
        // NPCs move in straight lines)
        if let Some(source) = self.nav.clone() {
            let (grids, tx) = (self.grids.clone(), tx.clone());
            let cached = grids.lock().get(&territory).cloned();
            self.tasks.spawn(async move {
                let grid = match cached {
                    Some(g) => Some(g),
                    None => {
                        let started = std::time::Instant::now();
                        let built = tokio::task::spawn_blocking(move || source(territory)).await.ok().flatten();
                        let built = built.map(std::sync::Arc::new);
                        if let Some(g) = &built {
                            tracing::info!(territory, took = ?started.elapsed(), "walkable ground built");
                            grids.lock().insert(territory, g.clone());
                        }
                        built
                    }
                };
                if let Some(grid) = grid {
                    let _ = tx.send(Request::Nav { grid }).await;
                }
            });
        }
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
    /// The player targets `target` (`None`: nothing).
    pub fn set_target(&self, id: ActorId, target: Option<ActorId>) {
        let _ = self.tx.try_send(Request::Target { id, target });
    }

    /// A dead player gets up (whole, where it fell).
    pub fn revive(&self, id: ActorId) {
        let _ = self.tx.try_send(Request::Revive { id });
    }

    /// Puts status `id` on `target` for `duration` (refreshed when `source`'s is already there),
    /// or takes it off when `duration` is `None`.
    pub fn set_status(&self, target: ActorId, id: u16, param: u16, duration: Option<std::time::Duration>, source: ActorId) {
        let _ = self.tx.try_send(Request::Status { target, id, param, duration, source });
    }

    /// The player sprints (all its TP for a while of status 50).
    pub fn sprint(&self, id: ActorId) {
        let _ = self.tx.try_send(Request::Sprint { id });
    }

    /// Takes every status off `target`.
    pub fn clear_statuses(&self, target: ActorId) {
        let _ = self.tx.try_send(Request::ClearStatuses { target });
    }

    /// The player uses an action on `target`.
    pub fn use_action(&self, id: ActorId, target: ActorId, action: ActionUse) {
        let _ = self.tx.try_send(Request::UseAction { id, target, action });
    }

    /// The player starts or stops auto-attacking its target.
    pub fn set_auto_attack(&self, id: ActorId, on: bool) {
        let _ = self.tx.try_send(Request::AutoAttack { id, on });
    }

    /// The player's gear changed.
    pub fn set_look(&self, id: ActorId, look: Arc<PlayerLook>) {
        let _ = self.tx.try_send(Request::Look { id, look });
    }

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
