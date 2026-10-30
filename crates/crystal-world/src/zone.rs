//! One territory: its actors, who sees whom, and the loop that owns them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    ActionUse, ActorId, ActorKind, ActorView, BNPC_ACTOR_BASE, BnpcPlacement, ChatRange, DESPAWN_RANGE, Event, Motion,
    Mana, NPC_ACTOR_BASE, NpcPlacement, PlayerLook, SAY_RANGE, SLOTS, SPAWN_RANGE, STATUS_SLOTS, StatusSlot, Statuses,
    VISIBILITY_INTERVAL, combat,
};

/// The most event NPCs one territory takes (the id space under its territory number).
pub const MAX_NPCS: usize = 0xfff;
/// The most battle NPCs one territory takes.
pub const MAX_BNPCS: usize = 0x3fff;

/// A visibility pass slower than this is logged.
const SLOW_PASS: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub(crate) enum Request {
    Enter { id: ActorId, token: u64, look: Arc<PlayerLook>, pos: [f32; 3], rotation: f32, outbox: mpsc::Sender<Event> },
    Leave { id: ActorId, token: u64 },
    Move { id: ActorId, pos: [f32; 3], rotation: f32, motion: Motion },
    Chat { id: ActorId, range: ChatRange, kind: u16, text: Arc<str> },
    Locate { id: ActorId, reply: oneshot::Sender<Option<([f32; 3], f32)>> },
    AddBnpc { bnpc: BnpcPlacement, reply: oneshot::Sender<ActorId> },
    Look { id: ActorId, look: Arc<PlayerLook> },
    Target { id: ActorId, target: Option<ActorId> },
    AutoAttack { id: ActorId, on: bool },
    Revive { id: ActorId },
    UseAction { id: ActorId, target: ActorId, action: ActionUse },
    Status { target: ActorId, id: u16, param: u16, duration: Option<Duration>, source: ActorId },
    ClearStatuses { target: ActorId },
    Sprint { id: ActorId },
    /// The territory's walkable ground, built after it started.
    Nav { grid: Arc<crate::nav::NavGrid> },
}

/// A battle NPC's way to where it is going: the goal it was planned for, the points left,
/// whether they end at the goal, and when it was planned.
#[derive(Debug, Clone)]
struct Route {
    goal: [f32; 3],
    points: std::collections::VecDeque<[f32; 3]>,
    reaches: bool,
    planned: Instant,
}

/// Where a battle NPC's step left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Moved {
    There,
    Going,
    /// There is no walkable way there (it has gone as near as it can).
    Stuck,
}

/// A player's cast: what lands on whom, when, and where it stood when it began.
#[derive(Debug, Clone, Copy)]
struct Cast {
    action: ActionUse,
    target: ActorId,
    lands: Instant,
    from: [f32; 3],
}

/// How far a caster may move before its cast is interrupted, yalms.
const CAST_SLIDE: f32 = 0.5;

/// A status on an actor, and when it wears off.
#[derive(Debug, Clone, Copy)]
struct Status {
    id: u16,
    param: u16,
    until: Instant,
    source: ActorId,
    /// What it does each server tick: damage, or healing when `tick_heal` (its potency through the
    /// source's damage or healing formula when it went on).
    tick: u32,
    tick_heal: bool,
    /// Its bearer's damage bonus, percent (Sapphire's DamageDealtPercent: Raging Strikes' 20).
    damage_percent: i16,
}

/// A battle NPC in a fight: where it came from, who it is after (the top of its hate list),
/// when it next swings, and its hate list (Sapphire's `BNpc::m_hateList`: who, how much).
#[derive(Debug, Clone)]
struct Mob {
    home: [f32; 3],
    target: ActorId,
    next_swing: Option<Instant>,
    hate: Vec<(ActorId, u32)>,
}

/// The action a hit came from, and the player's request it answers (0: none).
#[derive(Debug, Clone, Copy)]
struct ActionResultKey {
    action: u16,
    sequence: u32,
}

impl ActionResultKey {
    /// The Î²'s `Action` row 7, Attack: a melee auto-attack.
    const AUTO_ATTACK: Self = Self { action: 7, sequence: 0 };
    /// Row 8, Shot ("Delivers a ranged attack"): a ranged class's auto-attack, with its own
    /// animation (col 7: 1876 against Attack's 1875).
    const SHOT: Self = Self { action: 8, sequence: 0 };

    /// The auto-attack of a player of `class`.
    fn auto_attack(class: u8) -> Self {
        if combat::is_ranged_class(class) { Self::SHOT } else { Self::AUTO_ATTACK }
    }
}

/// A player's fighting state.
#[derive(Debug, Clone, Copy)]
struct Fighter {
    target: Option<ActorId>,
    auto_attack: bool,
    /// When its next auto-attack may land (`None`: now).
    next_swing: Option<Instant>,
}

/// A player's view of the territory: which actor is in which of its client's slots.
struct Viewer {
    token: u64,
    outbox: mpsc::Sender<Event>,
    slots: [Option<ActorId>; SLOTS],
    visible: HashMap<ActorId, u8>,
}

impl Viewer {
    fn new(token: u64, outbox: mpsc::Sender<Event>) -> Self {
        Self { token, outbox, slots: [None; SLOTS], visible: HashMap::new() }
    }

    fn free_slot(&self) -> Option<u8> {
        // slot 0 is the player itself
        (1..SLOTS).find(|&s| self.slots[s].is_none()).map(|s| s as u8)
    }
}

/// A territory's actors and players. Synchronous: the loop in [`run`] drives it, tests call it
/// directly.
pub struct ZoneState {
    pub territory: u16,
    actors: HashMap<ActorId, ActorView>,
    viewers: HashMap<ActorId, Viewer>,
    /// Players whose outbox was full or closed, to drop after the current request.
    failed: Vec<ActorId>,
    /// Battle NPCs placed so far (the next one's index).
    bnpcs: u32,
    /// Current and maximum HP of whatever can be hurt (battle NPCs, players).
    health: HashMap<ActorId, (u32, u32)>,
    fighters: HashMap<ActorId, Fighter>,
    /// Dead battle NPCs: when their bodies go, and when they come back as they were.
    corpses: Vec<(Instant, ActorId)>,
    respawns: Vec<(Instant, ActorView)>,
    /// Battle NPCs fighting someone.
    mobs: HashMap<ActorId, Mob>,
    last_tick: Option<Instant>,
    /// When the next server tick (regeneration) is.
    next_server_tick: Option<Instant>,
    /// Battle NPCs that attack players who come near.
    aggressive: std::collections::HashSet<ActorId>,
    /// When a player's actions are ready again: by action, or 0 for the global cooldown.
    recasts: HashMap<(ActorId, u16), Instant>,
    /// Actors' statuses, in their slots' order.
    statuses: HashMap<ActorId, Vec<Status>>,
    /// Players casting.
    casts: HashMap<ActorId, Cast>,
    /// Players' MP and TP.
    mana: HashMap<ActorId, Mana>,
    /// Battle NPCs' spawn points: where they stand, face, and wander around (yalms).
    spawn_points: HashMap<ActorId, ([f32; 3], f32, f32)>,
    /// Battle NPCs out of a fight wandering: where each walks to (`None`: resting) and when it
    /// next sets off.
    idle: HashMap<ActorId, (Option<[f32; 3]>, Instant)>,
    /// Battle NPCs that gave up a fight, running back to their spawn point.
    returning: std::collections::HashSet<ActorId>,
    /// The territory's own random numbers (wander points and pauses).
    rng: u64,
    /// Where battle NPCs can walk (none until built: then they move in straight lines).
    nav: Option<Arc<crate::nav::NavGrid>>,
    /// Battle NPCs' ways to where they are going.
    routes: HashMap<ActorId, Route>,
    /// Whether hits and heals roll for criticals and variance (off in tests, to check amounts).
    rolls: bool,
    /// Each player's last action that kept a combo going, and when it landed.
    combos: HashMap<ActorId, (u16, Instant)>,
}

/// One step of at most `step` yalms from `from` toward `to`, stopping `short` of it: the new
/// position and the facing; `None` when already there.
fn step_toward(from: [f32; 3], to: [f32; 3], step: f32, short: f32) -> Option<([f32; 3], f32)> {
    let d = distance2(from, to).sqrt();
    if d <= short + 0.05 {
        return None;
    }
    let step = step.min(d - short);
    let dir = [(to[0] - from[0]) / d, (to[1] - from[1]) / d, (to[2] - from[2]) / d];
    let pos = [from[0] + dir[0] * step, from[1] + dir[1] * step, from[2] + dir[2] * step];
    Some((pos, dir[0].atan2(dir[2])))
}

/// How soon a battle NPC plans its way again after its goal moved.
const REPLAN: Duration = Duration::from_millis(500);
/// How far a battle NPC's goal moves before its way is planned again.
const REPLAN_MOVE: f32 = 2.0;
/// How near a path's end must be to its goal to count as getting there (a cell and a half).
const REACHED: f32 = crate::nav::CELL * 1.5;

/// The distance over the ground (x and z).
fn flat(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn distance2(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn send(id: ActorId, viewer: &Viewer, event: Event, failed: &mut Vec<ActorId>) {
    if viewer.outbox.try_send(event).is_err() && !failed.contains(&id) {
        failed.push(id);
    }
}

impl ZoneState {
    pub fn new(territory: u16, npcs: &[NpcPlacement], bnpcs: &[BnpcPlacement]) -> Self {
        let bnpcs_in = bnpcs;
        if npcs.len() > MAX_NPCS {
            warn!(territory, count = npcs.len(), "more event NPCs than a territory takes; the rest are left out");
        }
        if bnpcs.len() > MAX_BNPCS {
            warn!(territory, count = bnpcs.len(), "more battle NPCs than a territory takes; the rest are left out");
        }
        let placed = bnpcs.len().min(MAX_BNPCS) as u32;
        let aggressive = bnpcs
            .iter()
            .take(MAX_BNPCS)
            .enumerate()
            .filter(|(_, b)| b.aggressive)
            .map(|(i, _)| BNPC_ACTOR_BASE + ((territory as u32) << 14) + i as u32 + 1)
            .collect();
        let enpcs = npcs.iter().take(MAX_NPCS).enumerate().map(|(i, n)| {
            let id = NPC_ACTOR_BASE + ((territory as u32) << 12) + i as u32 + 1;
            (id, ActorView { id, kind: ActorKind::EventNpc { enpc: n.enpc }, pos: n.pos, rotation: n.rotation })
        });
        let bnpcs = bnpcs.iter().take(MAX_BNPCS).enumerate().map(|(i, b)| {
            let id = BNPC_ACTOR_BASE + ((territory as u32) << 14) + i as u32 + 1;
            let kind =
                ActorKind::BattleNpc { base_id: b.base_id, name_id: b.name_id, level: b.level, aggressive: b.aggressive };
            (id, ActorView { id, kind, pos: b.pos, rotation: b.rotation })
        });
        let spawn_points = bnpcs_in
            .iter()
            .take(MAX_BNPCS)
            .enumerate()
            .map(|(i, b)| {
                let id = BNPC_ACTOR_BASE + ((territory as u32) << 14) + i as u32 + 1;
                (id, (b.pos, b.rotation, b.wander_range))
            })
            .collect();
        let actors: HashMap<ActorId, ActorView> = enpcs.chain(bnpcs).collect();
        let health = actors
            .values()
            .filter_map(|a| match a.kind {
                ActorKind::BattleNpc { level, .. } => Some((a.id, (combat::bnpc_hp(level), combat::bnpc_hp(level)))),
                _ => None,
            })
            .collect();
        Self {
            territory,
            actors,
            viewers: HashMap::new(),
            failed: Vec::new(),
            bnpcs: placed,
            health,
            fighters: HashMap::new(),
            corpses: Vec::new(),
            respawns: Vec::new(),
            mobs: HashMap::new(),
            last_tick: None,
            next_server_tick: None,
            aggressive,
            recasts: HashMap::new(),
            statuses: HashMap::new(),
            casts: HashMap::new(),
            mana: HashMap::new(),
            spawn_points,
            idle: HashMap::new(),
            returning: std::collections::HashSet::new(),
            rng: 0x9e37_79b9_7f4a_7c15 ^ territory as u64,
            nav: None,
            routes: HashMap::new(),
            rolls: !cfg!(test),
            combos: HashMap::new(),
        }
    }

    /// Gives the territory its walkable ground (as [`Request::Nav`] does).
    pub fn set_nav(&mut self, grid: Arc<crate::nav::NavGrid>) {
        self.nav = Some(grid);
        self.routes.clear();
    }

    /// Moves battle NPC `id` toward `goal` by `step` yalms, stopping `short` of it: on the ground
    /// along a walkable path when the territory's ground is known, in a straight line when not
    /// (or when it stands nowhere walkable). `walking` plays it at a walk.
    fn move_toward(&mut self, id: ActorId, goal: [f32; 3], step: f32, short: f32, walking: bool, now: Instant) -> Moved {
        let Some(here) = self.actors.get(&id).map(|a| a.pos) else { return Moved::Stuck };
        if flat(here, goal) <= short + 0.05 {
            self.routes.remove(&id);
            return Moved::There;
        }
        let straight = |zone: &mut Self| {
            if let Some((pos, rotation)) = step_toward(here, goal, step, short) {
                if walking { zone.place_walking(id, pos, rotation) } else { zone.place(id, pos, rotation) }
            }
            Moved::Going
        };
        let Some(nav) = self.nav.clone() else { return straight(self) };
        let replan = self.routes.get(&id).is_none_or(|r| {
            r.points.is_empty() || (flat(r.goal, goal) > REPLAN_MOVE && now.saturating_duration_since(r.planned) >= REPLAN)
        });
        if replan {
            let Some(points) = nav.path(here, goal) else {
                self.routes.remove(&id);
                return straight(self);
            };
            // a path that ends short of the goal (it could not get there) is walked to its end
            let reaches = points.last().is_some_and(|&p| flat(p, goal) <= REACHED);
            if points.is_empty() || (!reaches && points.last().is_some_and(|&p| flat(p, here) < 0.5)) {
                self.routes.remove(&id);
                return Moved::Stuck;
            }
            self.routes.insert(id, Route { goal, points: points.into(), reaches, planned: now });
        }
        let route = self.routes.get_mut(&id).expect("planned");
        let (mut pos, mut left, mut facing) = (here, step, self.actors[&id].rotation);
        while left > 1e-4 {
            let Some(&next) = route.points.front() else { break };
            // a path that gets there aims its last leg at where the goal is now
            let last = route.points.len() == 1 && route.reaches;
            let (target, stop) = if last { (goal, short) } else { (next, 0.0) };
            let d = flat(pos, target);
            if d > 1e-4 {
                facing = (target[0] - pos[0]).atan2(target[2] - pos[2]);
            }
            if d - stop <= left {
                let t = if d > 0.0 { (d - stop).max(0.0) / d } else { 1.0 };
                pos = [pos[0] + (target[0] - pos[0]) * t, target[1], pos[2] + (target[2] - pos[2]) * t];
                left -= (d - stop).max(0.0);
                route.points.pop_front();
                if last {
                    break;
                }
            } else {
                let t = left / d;
                pos = [pos[0] + (target[0] - pos[0]) * t, pos[1] + (target[1] - pos[1]) * t, pos[2] + (target[2] - pos[2]) * t];
                left = 0.0;
            }
        }
        let ended_short = route.points.is_empty() && !route.reaches;
        if let Some(y) = nav.ground(pos, 3.0) {
            pos[1] = y;
        }
        if pos != here {
            if walking { self.place_walking(id, pos, facing) } else { self.place(id, pos, facing) }
        }
        if flat(pos, goal) <= short + 0.05 {
            self.routes.remove(&id);
            Moved::There
        } else if ended_short {
            self.routes.remove(&id);
            Moved::Stuck
        } else {
            Moved::Going
        }
    }

    /// A hit's or heal's amount after its critical and variance rolls ([`combat::roll`]); as
    /// it is when rolls are off (tests).
    fn roll(&mut self, base: u32) -> u32 {
        if !self.rolls {
            return base;
        }
        let (crit, variance) = (self.random() * 100.0, self.random() * 100.0);
        combat::roll(base, crit, variance).0
    }

    /// The next of the territory's random numbers, 0..1 (xorshift64*).
    fn random(&mut self) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// The territory's clock: the last tick's time (statuses count down from it).
    fn now(&self) -> Instant {
        self.last_tick.unwrap_or_else(Instant::now)
    }

    /// An actor's statuses as the client shows them, the seconds left counted from `now`.
    pub fn statuses(&self, id: ActorId, now: Instant) -> Statuses {
        self.statuses
            .get(&id)
            .map(|list| {
                list.iter()
                    .map(|s| StatusSlot {
                        id: s.id,
                        param: s.param,
                        remaining: s.until.saturating_duration_since(now).as_secs_f32(),
                        source: s.source,
                    })
                    .collect()
            })
            .unwrap_or_else(|| Arc::from([]))
    }

    /// Puts status `id` on `target` until `duration` from `now`: the one `source` put there before
    /// is refreshed, and a full list takes no more. `None` takes `source`'s off (any source's, for
    /// [`Self::clear_statuses`]' GM uses: source 0). The target and whoever sees it are told.
    pub fn set_status(
        &mut self,
        target: ActorId,
        id: u16,
        param: u16,
        duration: Option<Duration>,
        source: ActorId,
        now: Instant,
    ) -> bool {
        if self.health.get(&target).is_none_or(|(hp, _)| *hp == 0) {
            return false;
        }
        let list = self.statuses.entry(target).or_default();
        let same = |s: &Status| s.id == id && (source == 0 || s.source == source);
        let changed = match duration {
            Some(d) => {
                let status =
                    Status { id, param, until: now + d, source, tick: 0, tick_heal: false, damage_percent: 0 };
                if let Some(s) = list.iter_mut().find(|s| same(s)) {
                    *s = status;
                    true
                } else if list.len() < STATUS_SLOTS {
                    list.push(status);
                    true
                } else {
                    debug!(target = format_args!("{target:#x}"), id, "status refused: no free slot");
                    false
                }
            }
            None => {
                let before = list.len();
                list.retain(|s| !same(s));
                list.len() != before
            }
        };
        if changed {
            self.tell_hp(target, now);
        }
        changed
    }

    /// Sprint (action 3): all the player's TP for status 50 ("Sprint", "Movement speed is
    /// increased." in the Î²'s Status sheet), param 30, a second for every 50 TP spent. The Î²'s
    /// Action row has no status for it (its cost type 6, cost 0): the status, param and duration
    /// are Sapphire's (`ActionSprint.cpp`). False when there is no TP to spend.
    pub fn sprint(&mut self, id: ActorId, now: Instant) -> bool {
        let Some(m) = self.mana.get_mut(&id) else { return false };
        let tp = std::mem::take(&mut m.tp);
        if tp < 50 {
            m.tp = tp;
            return false;
        }
        let duration = Duration::from_secs((tp / 50) as u64);
        self.set_status(id, combat::SPRINT_STATUS, combat::SPRINT_PARAM, Some(duration), id, now)
    }

    /// Takes every status off `target`.
    pub fn clear_statuses(&mut self, target: ActorId, now: Instant) {
        if self.statuses.remove(&target).is_some_and(|list| !list.is_empty()) {
            self.tell_hp(target, now);
        }
    }

    /// Takes off the statuses that wore off by `now`.
    /// Puts a status on a live `target` (an action's): its source's earlier one of the same id is
    /// refreshed, a full list takes no more; the target and whoever sees it are told.
    fn put_status(&mut self, target: ActorId, status: Status, now: Instant) {
        if self.health.get(&target).is_none_or(|(hp, _)| *hp == 0) {
            return;
        }
        let list = self.statuses.entry(target).or_default();
        if let Some(s) = list.iter_mut().find(|s| s.id == status.id && s.source == status.source) {
            *s = status;
        } else if list.len() < STATUS_SLOTS {
            list.push(status);
        } else {
            return;
        }
        self.tell_hp(target, now);
    }

    /// The server tick's damage and healing over time (Sapphire's `Chara::onTick`, each tick
    /// the amount its source's formula gave when it went on): damage on battle NPCs as their
    /// source's, healing on players.
    fn tick_statuses(&mut self, now: Instant) {
        let ticks: Vec<(ActorId, ActorId, u32, bool)> = self
            .statuses
            .iter()
            .flat_map(|(&target, list)| {
                list.iter().filter(|s| s.tick > 0 && s.until > now).map(move |s| (target, s.source, s.tick, s.tick_heal))
            })
            .collect();
        for (target, source, amount, heal) in ticks {
            if heal {
                if let Some(&(hp, hp_max)) = self.health.get(&target)
                    && hp > 0
                    && hp < hp_max
                {
                    self.health.insert(target, ((hp + amount).min(hp_max), hp_max));
                    self.tell_hp(target, now);
                }
            } else if self.health.get(&target).is_some_and(|(hp, _)| *hp > 0) {
                self.hurt(source, target, amount, None, now);
            }
        }
    }

    fn expire_statuses(&mut self, now: Instant) {
        let mut ended = Vec::new();
        for (&id, list) in &mut self.statuses {
            let before = list.len();
            list.retain(|s| s.until > now);
            if list.len() != before {
                ended.push(id);
            }
        }
        self.statuses.retain(|_, list| !list.is_empty());
        for id in ended {
            self.tell_hp(id, now);
        }
    }

    /// Tells the actor (a player) and whoever sees it its HP and statuses as they are.
    fn tell_hp(&mut self, id: ActorId, now: Instant) {
        let Some((hp, hp_max)) = self.health.get(&id).copied() else { return };
        let (level, class) = match self.actors.get(&id).map(|a| &a.kind) {
            Some(ActorKind::Player(look)) => (look.level as u16, look.class),
            Some(ActorKind::BattleNpc { level, .. }) => (*level, 0),
            _ => return,
        };
        let event = Event::Hp { actor: id, hp, hp_max, level, class, statuses: self.statuses(id, now), mana: self.mana_of(id) };
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == id || viewer.visible.contains_key(&id) {
                send(vid, viewer, event.clone(), failed);
            }
        }
    }

    /// Places a battle NPC now (players see it on the next visibility pass); its actor id.
    pub fn add_bnpc(&mut self, b: BnpcPlacement) -> ActorId {
        self.bnpcs += 1;
        let id = BNPC_ACTOR_BASE + ((self.territory as u32) << 14) + self.bnpcs;
        let kind =
            ActorKind::BattleNpc { base_id: b.base_id, name_id: b.name_id, level: b.level, aggressive: b.aggressive };
        self.actors.insert(id, ActorView { id, kind, pos: b.pos, rotation: b.rotation });
        self.spawn_points.insert(id, (b.pos, b.rotation, b.wander_range));
        self.health.insert(id, (combat::bnpc_hp(b.level), combat::bnpc_hp(b.level)));
        if b.aggressive {
            self.aggressive.insert(id);
        }
        id
    }

    /// An actor's current and maximum HP.
    pub fn health(&self, id: ActorId) -> Option<(u32, u32)> {
        self.health.get(&id).copied()
    }

    /// The player targets `target` (`None`: nothing); auto-attack stops when the target goes.
    pub fn set_target(&mut self, id: ActorId, target: Option<ActorId>) {
        let f = self.fighters.entry(id).or_insert(Fighter { target: None, auto_attack: false, next_swing: None });
        if f.target != target {
            f.auto_attack = false;
        }
        f.target = target;
    }

    /// The player starts (its first swing as soon as its last one allows) or stops auto-attacking.
    pub fn set_auto_attack(&mut self, id: ActorId, on: bool) {
        let f = self.fighters.entry(id).or_insert(Fighter { target: None, auto_attack: false, next_swing: None });
        f.auto_attack = on;
    }

    /// Lands the auto-attacks that are due at `now`, and takes away and brings back the dead.
    pub fn combat_tick(&mut self, now: Instant) {
        // the dead: bodies go, and later they are back as they were
        let (gone, kept): (Vec<_>, Vec<_>) = self.corpses.drain(..).partition(|(at, _)| *at <= now);
        self.corpses = kept;
        for (_, id) in gone {
            self.actors.remove(&id);
        }
        let (back, waiting): (Vec<_>, Vec<_>) = self.respawns.drain(..).partition(|(at, _)| *at <= now);
        self.respawns = waiting;
        for (_, actor) in back {
            if let ActorKind::BattleNpc { level, .. } = actor.kind {
                self.health.insert(actor.id, (combat::bnpc_hp(level), combat::bnpc_hp(level)));
            }
            self.actors.insert(actor.id, actor);
        }
        // the swings
        let due: Vec<(ActorId, ActorId)> = self
            .fighters
            .iter()
            .filter(|(_, f)| f.auto_attack && f.next_swing.is_none_or(|t| t <= now))
            .filter_map(|(id, f)| Some((*id, f.target?)))
            .collect();
        for (attacker, target) in due {
            self.swing(attacker, target, now);
        }
        let dt = self
            .last_tick
            .map_or(Duration::from_millis(250), |t| now.saturating_duration_since(t))
            .min(Duration::from_secs(1));
        self.last_tick = Some(now);
        self.finish_casts(now);
        self.expire_statuses(now);
        self.mob_tick(now, dt.as_secs_f32());
        if self.next_server_tick.is_none_or(|t| t <= now) {
            self.next_server_tick = Some(now + combat::SERVER_TICK);
            self.tick_statuses(now);
            self.regenerate();
            self.rest_returning(now);
        }
        self.drop_failed();
    }

    /// The server tick's HP: live players no battle NPC is after get some of theirs back.
    fn regenerate(&mut self) {
        let fighting: Vec<ActorId> = self.mobs.values().map(|m| m.target).collect();
        let live: Vec<ActorId> =
            self.viewers.keys().copied().filter(|id| self.health.get(id).is_some_and(|(hp, _)| *hp > 0)).collect();
        for id in live {
            let in_fight = fighting.contains(&id);
            let mut changed = false;
            // HP out of a fight only
            if let Some((hp, max)) = self.health.get(&id).copied()
                && !in_fight
                && hp < max
            {
                self.health.insert(id, ((hp + (max * combat::REGEN_PERCENT / 100).max(1)).min(max), max));
                changed = true;
            }
            // MP and TP always, MP more slowly in a fight
            if let Some(m) = self.mana.get_mut(&id) {
                let percent = if in_fight { combat::MP_REGEN_FIGHTING_PERCENT } else { combat::MP_REGEN_PERCENT };
                let before = *m;
                m.mp = (m.mp + m.mp_max * percent / 100).min(m.mp_max);
                m.tp = (m.tp + combat::TP_REGEN).min(combat::TP_MAX);
                changed |= *m != before;
            }
            if changed {
                self.tell_hp(id, self.now());
            }
        }
    }

    /// An actor's MP and TP (zero for what has none).
    fn mana_of(&self, id: ActorId) -> Mana {
        self.mana.get(&id).copied().unwrap_or_default()
    }

    /// Whether the player has what `action` costs.
    fn can_afford(&self, id: ActorId, action: &ActionUse) -> bool {
        let m = self.mana_of(id);
        m.mp >= action.mp_cost && m.tp >= action.tp_cost
    }

    /// Pays what `action` costs; false (and nothing paid) when the player has not got it.
    fn pay(&mut self, id: ActorId, action: &ActionUse) -> bool {
        if action.mp_cost == 0 && action.tp_cost == 0 {
            return true;
        }
        let Some(m) = self.mana.get_mut(&id) else { return false };
        if m.mp < action.mp_cost || m.tp < action.tp_cost {
            return false;
        }
        m.mp -= action.mp_cost;
        m.tp -= action.tp_cost;
        true
    }

    /// A dead player gets up whole, where it fell.
    pub fn revive(&mut self, id: ActorId) {
        if let Some((0, max)) = self.health.get(&id).copied() {
            self.set_hp(id, max, max);
        }
    }

    /// Sets a player's HP and tells it and whoever sees it.
    fn set_hp(&mut self, id: ActorId, hp: u32, hp_max: u32) {
        self.health.insert(id, (hp, hp_max));
        self.tell_hp(id, self.now());
    }

    /// Battle NPCs in a fight (Sapphire's `StateCombat`): hate on the dead, the gone and those
    /// past [`combat::DEAGGRO_RANGE`] is dropped; with none left, or past the leash from its
    /// spawn point, it goes home; otherwise it goes for the top of its hate list, swinging in
    /// reach on its timer.
    fn mob_tick(&mut self, now: Instant, dt: f32) {
        self.aggro(now);
        let ids: Vec<ActorId> = self.mobs.keys().copied().collect();
        for id in ids {
            let Some(here) = self.actors.get(&id).map(|a| a.pos) else {
                self.go_home(id);
                continue;
            };
            let Some(mut mob) = self.mobs.get(&id).cloned() else { continue };
            mob.hate.retain(|(who, _)| {
                self.health.get(who).is_some_and(|(hp, _)| *hp > 0)
                    && self.actors.get(who).is_some_and(|a| distance2(here, a.pos) <= combat::DEAGGRO_RANGE.powi(2))
            });
            let top = mob.hate.iter().max_by_key(|(_, h)| *h).map(|(who, _)| *who);
            let Some(target) = top.filter(|_| distance2(here, mob.home) <= combat::LEASH.powi(2)) else {
                self.go_home(id);
                continue;
            };
            mob.target = target;
            let next_swing = mob.next_swing;
            self.mobs.insert(id, mob);
            let there = self.actors[&target].pos;
            let reach = combat::MELEE_RANGE * 0.8;
            if distance2(here, there) > reach.powi(2) {
                // a step toward the target, stopping just inside reach; one it cannot get to
                // it gives up on
                if self.move_toward(id, there, combat::CHASE_SPEED * dt, reach * 0.75, false, now) == Moved::Stuck {
                    self.go_home(id);
                }
            } else if next_swing.is_none_or(|t| t <= now) {
                // facing its target as it swings
                let facing = (there[0] - here[0]).atan2(there[2] - here[2]);
                if (facing - self.actors[&id].rotation).abs() > 0.01 {
                    self.place(id, here, facing);
                }
                self.mob_swing(id, target, now);
            }
        }
        self.walk_home(now, dt);
        self.wander(now, dt);
    }

    /// Aggressive battle NPCs, alive and not fighting, go for a live player within their sense
    /// range: [`combat::aggro_range`] of the two levels (Sapphire's `BNpc::checkAggro`).
    fn aggro(&mut self, now: Instant) {
        let players: Vec<(ActorId, [f32; 3], u8)> = self
            .viewers
            .keys()
            .filter(|id| self.health.get(id).is_some_and(|(hp, _)| *hp > 0))
            .filter_map(|id| match self.actors.get(id) {
                Some(ActorView { kind: ActorKind::Player(look), pos, .. }) => Some((*id, *pos, look.level)),
                _ => None,
            })
            .collect();
        if players.is_empty() {
            return;
        }
        let mut found = Vec::new();
        for id in &self.aggressive {
            if self.mobs.contains_key(id)
                || self.returning.contains(id)
                || self.health.get(id).is_none_or(|(hp, _)| *hp == 0)
            {
                continue;
            }
            let Some(ActorView { kind: ActorKind::BattleNpc { level, .. }, pos, .. }) = self.actors.get(id) else {
                continue;
            };
            let nearest = players
                .iter()
                .map(|(p, at, player_level)| (distance2(*pos, *at), *p, *player_level))
                .filter(|(d, _, player_level)| *d < combat::aggro_range(*player_level, *level).powi(2))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, target, _)) = nearest {
                found.push((*id, target));
            }
        }
        for (id, target) in found {
            self.add_hate(id, target, combat::AGGRO_HATE, now);
        }
    }

    /// `who` angers battle NPC `id` by `amount` (Sapphire's `hateListUpdate`): added to its hate
    /// list, and a fight starts when it was not in one (its weapon out, battle stance on; its
    /// first swing a swing and a little after). Nothing on the dead and those running home.
    fn add_hate(&mut self, id: ActorId, who: ActorId, amount: u32, now: Instant) {
        if self.returning.contains(&id) || self.health.get(&id).is_none_or(|(hp, _)| *hp == 0) {
            return;
        }
        if let Some(mob) = self.mobs.get_mut(&id) {
            match mob.hate.iter_mut().find(|(w, _)| *w == who) {
                Some((_, h)) => *h = h.saturating_add(amount),
                None => mob.hate.push((who, amount)),
            }
            return;
        }
        let Some(here) = self.actors.get(&id).map(|a| a.pos) else { return };
        let home = self.spawn_points.get(&id).map_or(here, |s| s.0);
        self.idle.remove(&id);
        self.routes.remove(&id);
        // Sapphire's aggro(): the last attack set 500-1000 ms ahead of now
        let variation = Duration::from_secs_f32(0.5 + 0.5 * self.random());
        let next_swing = Some(now + combat::MOB_SWING + variation);
        self.mobs.insert(id, Mob { home, target: who, next_swing, hate: vec![(who, amount)] });
        self.tell_order(id, crate::order::SET_BATTLE, [1, 0, 0, 0]);
    }

    /// Tells whoever sees `id` an order about it.
    fn tell_order(&mut self, id: ActorId, kind: u16, args: [u32; 4]) {
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if viewer.visible.contains_key(&id) {
                send(vid, viewer, Event::Order { actor: id, kind, args }, failed);
            }
        }
    }

    /// Moves an actor (running) and tells whoever sees it.
    fn place(&mut self, id: ActorId, pos: [f32; 3], rotation: f32) {
        self.place_with(id, pos, rotation, Motion::default());
    }

    /// Moves an actor at a walk (the move's walking flag: the client plays it at walking speed).
    fn place_walking(&mut self, id: ActorId, pos: [f32; 3], rotation: f32) {
        self.place_with(id, pos, rotation, Motion { flags: Motion::WALKING, ..Motion::default() });
    }

    fn place_with(&mut self, id: ActorId, pos: [f32; 3], rotation: f32, motion: Motion) {
        let Some(actor) = self.actors.get_mut(&id) else { return };
        (actor.pos, actor.rotation) = (pos, rotation);
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if viewer.visible.contains_key(&id) {
                send(vid, viewer, Event::Move { actor: id, pos, rotation, motion }, failed);
            }
        }
    }

    /// A battle NPC gives up (Sapphire's `StateRetreat`): weapon away and out of battle stance,
    /// it runs back to its spawn point, taking no damage and fighting no one on the way, and
    /// healing a tenth of its HP each server tick ([`Self::rest_returning`]); whole once there.
    fn go_home(&mut self, id: ActorId) {
        if self.mobs.remove(&id).is_none() {
            return;
        }
        if matches!(self.actors.get(&id).map(|a| &a.kind), Some(ActorKind::BattleNpc { .. })) {
            self.idle.remove(&id);
            self.routes.remove(&id);
            self.returning.insert(id);
            self.tell_order(id, crate::order::TOGGLE_WEAPON, [0, 1, 1, 0]);
            self.tell_order(id, crate::order::SET_BATTLE, [0, 0, 0, 0]);
        }
    }

    /// Battle NPCs running home heal a tenth of their HP (and 1) on the server tick (Sapphire's
    /// `BNpc::restHp`).
    fn rest_returning(&mut self, now: Instant) {
        let ids: Vec<ActorId> = self.returning.iter().copied().collect();
        for id in ids {
            if let Some(&(hp, hp_max)) = self.health.get(&id)
                && hp < hp_max
            {
                self.health.insert(id, ((hp + hp_max / 10 + 1).min(hp_max), hp_max));
                self.tell_hp(id, now);
            }
        }
    }

    /// Battle NPCs going home: a run's step toward their spawn point; there, they face as they
    /// were placed, whole, and are idle again (one with no way home is put there).
    fn walk_home(&mut self, now: Instant, dt: f32) {
        let ids: Vec<ActorId> = self.returning.iter().copied().collect();
        for id in ids {
            let Some(&(home, facing, _)) = self.spawn_points.get(&id).filter(|_| self.actors.contains_key(&id)) else {
                self.returning.remove(&id);
                continue;
            };
            if self.move_toward(id, home, combat::CHASE_SPEED * dt, 0.0, false, now) != Moved::Going {
                self.returning.remove(&id);
                self.place(id, home, facing);
                if let Some(&(hp, hp_max)) = self.health.get(&id)
                    && hp < hp_max
                {
                    self.health.insert(id, (hp_max, hp_max));
                    self.tell_hp(id, now);
                }
            }
        }
    }

    /// Battle NPCs out of a fight that someone sees: a while at rest, then a walk to a point
    /// within their wander range of the spawn point (the spawn's `wander_range`), and so on.
    fn wander(&mut self, now: Instant, dt: f32) {
        let seen: std::collections::HashSet<ActorId> =
            self.viewers.values().flat_map(|v| v.visible.keys().copied()).collect();
        let ids: Vec<ActorId> = self
            .spawn_points
            .iter()
            .filter(|(id, s)| s.2 > 0.0 && seen.contains(id) && !self.mobs.contains_key(id) && !self.returning.contains(id))
            .filter(|(id, _)| self.health.get(id).is_some_and(|(hp, _)| *hp > 0))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let (home, _, range) = self.spawn_points[&id];
            let (dest, next) = match self.idle.get(&id).copied() {
                Some(state) => state,
                None => {
                    // a first rest of up to the longest pause, so they do not all set off at once
                    let rest = Duration::from_secs_f32(self.random() * combat::WANDER_PAUSE_MAX);
                    (None, now + rest)
                }
            };
            let dest = match dest {
                Some(d) => Some(d),
                None if now >= next => {
                    let angle = self.random() * std::f32::consts::TAU;
                    let r = range * self.random().sqrt();
                    Some([home[0] + r * angle.cos(), home[1], home[2] + r * angle.sin()])
                }
                None => None,
            };
            let Some(dest) = dest else {
                self.idle.insert(id, (None, next));
                continue;
            };
            match self.move_toward(id, dest, combat::WALK_SPEED * dt, 0.0, true, now) {
                Moved::Going => {
                    self.idle.insert(id, (Some(dest), next));
                }
                Moved::There | Moved::Stuck => {
                    let pause = combat::WANDER_PAUSE_MIN
                        + self.random() * (combat::WANDER_PAUSE_MAX - combat::WANDER_PAUSE_MIN);
                    self.idle.insert(id, (None, now + Duration::from_secs_f32(pause)));
                }
            }
        }
    }

    /// A battle NPC's swing at a player.
    fn mob_swing(&mut self, id: ActorId, target: ActorId, now: Instant) {
        let Some(ActorKind::BattleNpc { level, .. }) = self.actors.get(&id).map(|a| a.kind.clone()) else { return };
        let Some(ActorKind::Player(look)) = self.actors.get(&target).map(|a| a.kind.clone()) else { return };
        let Some((hp, hp_max)) = self.health.get(&target).copied().filter(|(hp, _)| *hp > 0) else { return };
        let amount = combat::bnpc_damage(level).min(hp);
        let hp = hp - amount;
        self.health.insert(target, (hp, hp_max));
        if let Some(m) = self.mobs.get_mut(&id) {
            m.next_swing = Some(now + combat::MOB_SWING);
        }
        // the dead keep no statuses, and cast nothing
        if hp == 0 {
            self.statuses.remove(&target);
            self.interrupt(target, "died", true);
        }
        let statuses = self.statuses(target, now);
        let mana = self.mana_of(target);
        let player_level = look.level as u16;
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == target || viewer.visible.contains_key(&target) {
                send(
                    vid,
                    viewer,
                    Event::Damage {
                        source: id,
                        target,
                        action: ActionResultKey::AUTO_ATTACK.action,
                        sequence: 0,
                        amount,
                        hp,
                        hp_max,
                        level: player_level,
                        class: look.class,
                        statuses: statuses.clone(),
                    mana,
                    },
                    failed,
                );
                if hp == 0 {
                    let credit = Arc::from([]);
                    send(vid, viewer, Event::Death { actor: target, killer: Some(id), level: player_level, credit }, failed);
                }
            }
        }
        if hp == 0 {
            debug!(territory = self.territory, player = format_args!("{target:#x}"), "player died");
            if let Some(f) = self.fighters.get_mut(&target) {
                f.auto_attack = false;
            }
        }
    }

    /// One auto-attack: on a live battle NPC in reach, damage by the attacker's level and strength.
    fn swing(&mut self, attacker: ActorId, target: ActorId, now: Instant) {
        if !self.actors.contains_key(&attacker) || !self.actors.contains_key(&target) {
            if let Some(f) = self.fighters.get_mut(&attacker) {
                f.auto_attack = false;
            }
            return;
        }
        let reach = match self.actors.get(&attacker).map(|a| &a.kind) {
            Some(ActorKind::Player(look)) => combat::auto_attack_range(look.class),
            _ => combat::MELEE_RANGE,
        };
        let Some(look) = self.can_hit(attacker, target, reach) else { return };
        let amount = self.roll(combat::auto_attack_damage(&look));
        // the weapon's delay between swings (Sapphire's player auto-attack)
        let delay =
            if look.weapon.delay_ms > 0 { Duration::from_millis(look.weapon.delay_ms as u64) } else { combat::SWING };
        if let Some(f) = self.fighters.get_mut(&attacker) {
            f.next_swing = Some(now + delay);
        }
        self.strike(attacker, target, amount, ActionResultKey::auto_attack(look.class), now);
    }

    /// The attacker's look, when it is a live player and `target` a live battle NPC within `range`.
    fn can_hit(&self, attacker: ActorId, target: ActorId, range: f32) -> Option<Arc<PlayerLook>> {
        let (a, t) = (self.actors.get(&attacker)?, self.actors.get(&target)?);
        let ActorKind::Player(look) = &a.kind else { return None };
        let ActorKind::BattleNpc { .. } = t.kind else { return None };
        let alive = |id| self.health.get(&id).is_some_and(|(hp, _)| *hp > 0);
        (distance2(a.pos, t.pos) <= range.powi(2) && alive(attacker) && alive(target)).then(|| look.clone())
    }

    /// The player uses an action on `target`: refused (and logged) while its recast runs, while it
    /// casts another, out of range, or on what it cannot hit or heal. One with a cast time starts
    /// casting (its recast with it) and lands on the tick its cast ends.
    pub fn use_action(&mut self, id: ActorId, target: ActorId, action: ActionUse, now: Instant) -> bool {
        let key = (id, if action.gcd { 0 } else { action.action });
        if self.recasts.get(&key).is_some_and(|t| *t > now) {
            debug!(player = format_args!("{id:#x}"), action = action.action, "action refused: recasting");
            return false;
        }
        if self.casts.contains_key(&id) {
            debug!(player = format_args!("{id:#x}"), action = action.action, "action refused: casting");
            return false;
        }
        // a buff (no potency, not a heal) goes on the user, as a heal would
        let friendly = action.heal || action.potency == 0;
        let target = if friendly { self.heal_target(id, target) } else { target };
        let ready = if friendly { self.can_heal(id, target, action.range) } else { self.can_hit(id, target, action.range) };
        if ready.is_none() {
            debug!(player = format_args!("{id:#x}"), action = action.action, "action refused: no target in range");
            return false;
        }
        if !self.can_afford(id, &action) {
            debug!(player = format_args!("{id:#x}"), action = action.action, mp = action.mp_cost, tp = action.tp_cost,
                   "action refused: not enough MP or TP");
            return false;
        }
        self.recasts.insert(key, now + action.recast);
        if !action.cast.is_zero() {
            let from = self.actors.get(&id).map_or([0.0; 3], |a| a.pos);
            self.casts.insert(id, Cast { action, target, lands: now + action.cast, from });
            let event = Event::Cast { actor: id, action: action.action, target, time: action.cast };
            let failed = &mut self.failed;
            for (&vid, viewer) in &self.viewers {
                if vid == id || viewer.visible.contains_key(&id) {
                    send(vid, viewer, event.clone(), failed);
                }
            }
            self.drop_failed();
            return true;
        }
        let landed = self.land(id, target, action, now);
        self.drop_failed();
        landed
    }

    /// An action lands: checked again (a cast's target can have died or gone), then the hit or
    /// the heal.
    fn land(&mut self, id: ActorId, target: ActorId, action: ActionUse, now: Instant) -> bool {
        let key = ActionResultKey { action: action.action, sequence: action.sequence };
        let friendly = action.heal || action.potency == 0;
        let look = if friendly { self.can_heal(id, target, action.range) } else { self.can_hit(id, target, action.range) };
        let Some(look) = look else {
            debug!(player = format_args!("{id:#x}"), action = action.action, "action lost: its target is gone");
            return false;
        };
        // (paid as it lands: a cast's MP could have gone meanwhile)
        if !self.pay(id, &action) {
            debug!(player = format_args!("{id:#x}"), action = action.action, "action lost: not enough MP or TP");
            return false;
        }
        if action.heal {
            let amount = self.roll(combat::heal_amount(&look, action.potency));
            self.restore(id, target, amount, key, now);
        } else if friendly {
            // a buff: the action played and answered, its statuses (below) the rest
            let failed = &mut self.failed;
            for (&vid, viewer) in &self.viewers {
                if vid == id || viewer.visible.contains_key(&id) {
                    // the request's sequence is the user's alone
                    let sequence = if vid == id { action.sequence } else { 0 };
                    send(vid, viewer, Event::ActionDone { source: id, target, action: action.action, sequence }, failed);
                }
            }
        } else {
            // a combo (Sapphire's isCorrectCombo): the action it follows landed within the
            // window; the chain goes on with any action that is not a broken combo
            let combo = action.combo_from != 0
                && self.combos.get(&id).is_some_and(|(a, t)| {
                    *a == action.combo_from && now.saturating_duration_since(*t) <= combat::COMBO_WINDOW
                });
            if action.combo_from == 0 || combo {
                self.combos.insert(id, (action.action, now));
            } else {
                self.combos.remove(&id);
            }
            let potency = if combo { action.combo_potency } else { action.potency };
            let amount = self.roll(combat::action_damage(&look, potency));
            self.strike(id, target, amount, key, now);
        }
        for effect in action.effects.into_iter().flatten() {
            let on = if effect.on_self { id } else { target };
            let tick = match (effect.tick_potency, effect.tick_heal) {
                (0, _) => 0,
                (p, true) => combat::heal_amount(&look, p),
                (p, false) => combat::action_damage(&look, p),
            };
            let status = Status {
                id: effect.id,
                param: 0,
                until: now + effect.duration,
                source: id,
                tick,
                tick_heal: effect.tick_heal,
                damage_percent: effect.damage_percent,
            };
            self.put_status(on, status, now);
        }
        // the user's own MP and TP, where the result did not carry them (another's HP update)
        if (action.mp_cost > 0 || action.tp_cost > 0) && target != id {
            self.tell_hp(id, now);
        }
        true
    }

    /// Who a heal goes on: the target when it is a player, else the user itself (a heal used with
    /// a battle NPC or nothing targeted).
    fn heal_target(&self, id: ActorId, target: ActorId) -> ActorId {
        match self.actors.get(&target).map(|a| &a.kind) {
            Some(ActorKind::Player(_)) => target,
            _ => id,
        }
    }

    /// The healer's look, when it and `target` are live players within `range` of each other.
    fn can_heal(&self, id: ActorId, target: ActorId, range: f32) -> Option<Arc<PlayerLook>> {
        let (a, t) = (self.actors.get(&id)?, self.actors.get(&target)?);
        let ActorKind::Player(look) = &a.kind else { return None };
        let ActorKind::Player(_) = t.kind else { return None };
        let alive = |id| self.health.get(&id).is_some_and(|(hp, _)| *hp > 0);
        (distance2(a.pos, t.pos) <= range.powi(2) && alive(id) && alive(target)).then(|| look.clone())
    }

    /// A heal on a player: its HP up to its maximum, told to the healer and whoever sees the target.
    fn restore(&mut self, source: ActorId, target: ActorId, amount: u32, key: ActionResultKey, now: Instant) {
        let Some((hp, hp_max)) = self.health.get(&target).copied() else { return };
        let Some(ActorKind::Player(look)) = self.actors.get(&target).map(|a| a.kind.clone()) else { return };
        let amount = amount.min(hp_max - hp);
        let hp = hp + amount;
        self.health.insert(target, (hp, hp_max));
        let statuses = self.statuses(target, now);
        let mana = self.mana_of(target);
        let ActionResultKey { action, sequence } = key;
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == source || vid == target || viewer.visible.contains_key(&target) {
                let sequence = if vid == source { sequence } else { 0 };
                let event = Event::Heal {
                    source,
                    target,
                    action,
                    sequence,
                    amount,
                    hp,
                    hp_max,
                    level: look.level as u16,
                    class: look.class,
                    statuses: statuses.clone(),
                    mana,
                };
                send(vid, viewer, event, failed);
            }
        }
    }

    /// Lands the casts that end by `now`.
    fn finish_casts(&mut self, now: Instant) {
        let due: Vec<(ActorId, Cast)> = self.casts.iter().filter(|(_, c)| c.lands <= now).map(|(id, c)| (*id, *c)).collect();
        for (id, cast) in due {
            self.casts.remove(&id);
            self.land(id, cast.target, cast.action, now);
        }
    }

    /// Stops a cast; the action does not land. `interrupted`: by something else (the caster died),
    /// not cancelled by its own doing (it moved). The caster and whoever sees it are told.
    fn interrupt(&mut self, id: ActorId, why: &str, interrupted: bool) {
        let Some(cast) = self.casts.remove(&id) else { return };
        debug!(player = format_args!("{id:#x}"), action = cast.action.action, why, "cast ended");
        // (its recast given back: an interrupted cast can be cast again at once)
        self.recasts.remove(&(id, if cast.action.gcd { 0 } else { cast.action.action }));
        let event = Event::CastEnd { actor: id, action: cast.action.action, interrupted };
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == id || viewer.visible.contains_key(&id) {
                send(vid, viewer, event.clone(), failed);
            }
        }
    }

    /// A player's hit on a battle NPC (checked by [`Self::can_hit`]): its HP and the hit go out,
    /// and it fights back or dies.
    fn strike(&mut self, attacker: ActorId, target: ActorId, amount: u32, key: ActionResultKey, now: Instant) {
        self.hurt(attacker, target, amount, Some(key), now);
    }

    /// Damage to a battle NPC from `attacker`: by an action (`key`, its hit shown) or by a
    /// status's tick (`None`: its HP update alone). Its statuses' damage bonuses of the attacker
    /// apply (Sapphire's DamageDealtPercent); it hates the attacker, or dies.
    fn hurt(&mut self, attacker: ActorId, target: ActorId, amount: u32, key: Option<ActionResultKey>, now: Instant) {
        let Some((hp, hp_max)) = self.health.get(&target).copied() else { return };
        let Some(t) = self.actors.get(&target) else { return };
        let ActorKind::BattleNpc { level: target_level, .. } = t.kind else { return };
        let bonus: i32 = self.statuses.get(&attacker).map_or(0, |l| l.iter().map(|s| s.damage_percent as i32).sum());
        let amount = (amount as i64 * (100 + bonus as i64) / 100).max(0) as u32;
        // one running home takes no damage (Sapphire's retreat: InvincibilityIgnoreDamage)
        let amount = if self.returning.contains(&target) { 0 } else { amount.min(hp) };
        let hp = hp - amount;
        self.health.insert(target, (hp, hp_max));
        if hp == 0 {
            self.statuses.remove(&target);
        }
        let statuses = self.statuses(target, now);
        let mana = self.mana_of(target);
        // the experience: everyone it hated, and the killer
        let credit: Arc<[ActorId]> = if hp == 0 {
            let mut who: Vec<ActorId> =
                self.mobs.get(&target).map(|m| m.hate.iter().map(|(w, _)| *w).collect()).unwrap_or_default();
            if !who.contains(&attacker) {
                who.push(attacker);
            }
            who.into()
        } else {
            Arc::from([])
        };
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == attacker || viewer.visible.contains_key(&target) {
                let event = match key {
                    Some(ActionResultKey { action, sequence }) => Event::Damage {
                        source: attacker,
                        target,
                        action,
                        // the request's sequence is the user's alone
                        sequence: if vid == attacker { sequence } else { 0 },
                        amount,
                        hp,
                        hp_max,
                        level: target_level,
                        class: 0,
                        statuses: statuses.clone(),
                        mana,
                    },
                    None => Event::Hp {
                        actor: target,
                        hp,
                        hp_max,
                        level: target_level,
                        class: 0,
                        statuses: statuses.clone(),
                        mana,
                    },
                };
                send(vid, viewer, event, failed);
                if hp == 0 {
                    send(
                        vid,
                        viewer,
                        Event::Death {
                            actor: target,
                            killer: Some(attacker),
                            level: target_level,
                            credit: credit.clone(),
                        },
                        failed,
                    );
                }
            }
        }
        if hp > 0 {
            // it hates its attacker by the damage (and fights back when it was not fighting)
            self.add_hate(target, attacker, amount.max(1), now);
        }
        if hp == 0 {
            self.mobs.remove(&target);
            self.routes.remove(&target);
            debug!(territory = self.territory, target = format_args!("{target:#x}"), "battle NPC died");
            for f in self.fighters.values_mut().filter(|f| f.target == Some(target)) {
                f.auto_attack = false;
            }
            self.corpses.push((now + combat::CORPSE, target));
            if let Some(view) = self.actors.get(&target) {
                // back where it was placed, not where it fell
                let mut view = view.clone();
                if let Some(&(pos, rotation, _)) = self.spawn_points.get(&target) {
                    (view.pos, view.rotation) = (pos, rotation);
                }
                self.idle.remove(&target);
                self.returning.remove(&target);
                self.respawns.push((now + combat::RESPAWN, view));
            }
        }
    }

    pub fn players(&self) -> usize {
        self.viewers.len()
    }

    pub fn actors(&self) -> usize {
        self.actors.len()
    }

    pub(crate) fn handle(&mut self, request: Request) {
        match request {
            Request::Enter { id, token, look, pos, rotation, outbox } => {
                self.enter(id, token, look, pos, rotation, outbox)
            }
            Request::Leave { id, token } => self.leave(id, token),
            Request::Move { id, pos, rotation, motion } => self.moved(id, pos, rotation, motion),
            Request::Chat { id, range, kind, text } => self.chat(id, range, kind, text),
            Request::Look { id, look } => self.set_look(id, look),
            Request::Target { id, target } => self.set_target(id, target),
            Request::AutoAttack { id, on } => self.set_auto_attack(id, on),
            Request::Revive { id } => self.revive(id),
            Request::UseAction { id, target, action } => {
                self.use_action(id, target, action, Instant::now());
            }
            Request::Status { target, id, param, duration, source } => {
                self.set_status(target, id, param, duration, source, self.now());
            }
            Request::ClearStatuses { target } => self.clear_statuses(target, self.now()),
            Request::Sprint { id } => {
                self.sprint(id, self.now());
            }
            Request::Nav { grid } => {
                self.nav = Some(grid);
                self.routes.clear();
            }
            Request::AddBnpc { bnpc, reply } => {
                let _ = reply.send(self.add_bnpc(bnpc));
            }
            Request::Locate { id, reply } => {
                let _ = reply.send(self.actors.get(&id).map(|a| (a.pos, a.rotation)));
            }
        }
        self.drop_failed();
    }

    /// A player comes in. Another entry of the same actor (an earlier login) is replaced.
    pub fn enter(
        &mut self,
        id: ActorId,
        token: u64,
        look: Arc<PlayerLook>,
        pos: [f32; 3],
        rotation: f32,
        outbox: mpsc::Sender<Event>,
    ) {
        if let Some(old) = self.viewers.get(&id) {
            let old = old.token;
            info!(territory = self.territory, actor = format_args!("{id:#x}"), "entered again: replacing");
            self.leave(id, old);
        }
        self.health.insert(id, (look.hp_max.max(1), look.hp_max.max(1)));
        self.mana.insert(id, Mana { mp: look.mp_max, mp_max: look.mp_max, tp: combat::TP_MAX });
        self.actors.insert(id, ActorView { id, kind: ActorKind::Player(look), pos, rotation });
        self.viewers.insert(id, Viewer::new(token, outbox));
        debug!(territory = self.territory, players = self.viewers.len(), "player entered");
        self.update_visibility();
    }

    /// A player goes; whoever saw it sees it despawn. Ignored when `token` is not the entry that
    /// is in (a replaced login leaving late).
    pub fn leave(&mut self, id: ActorId, token: u64) {
        if self.viewers.get(&id).is_none_or(|v| v.token != token) {
            return;
        }
        self.viewers.remove(&id);
        self.actors.remove(&id);
        self.fighters.remove(&id);
        self.health.remove(&id);
        self.statuses.remove(&id);
        self.casts.remove(&id);
        self.mana.remove(&id);
        // whoever was after it goes home
        let chasing: Vec<ActorId> = self.mobs.iter().filter(|(_, m)| m.target == id).map(|(m, _)| *m).collect();
        for m in chasing {
            self.go_home(m);
        }
        let failed = &mut self.failed;
        for (&vid, viewer) in &mut self.viewers {
            if let Some(slot) = viewer.visible.remove(&id) {
                viewer.slots[slot as usize] = None;
                send(vid, viewer, Event::Despawn { slot, actor: id }, failed);
            }
        }
        debug!(territory = self.territory, players = self.viewers.len(), "player left");
    }

    /// A player moved; whoever sees it is told.
    /// A player's gear changed: it and whoever sees it are told.
    pub fn set_look(&mut self, id: ActorId, look: Arc<PlayerLook>) {
        let Some(actor) = self.actors.get_mut(&id) else { return };
        actor.kind = ActorKind::Player(look.clone());
        // (a new level or class: its maximum MP, what it has kept within it)
        if let Some(m) = self.mana.get_mut(&id) {
            m.mp_max = look.mp_max;
            m.mp = m.mp.min(m.mp_max);
        }
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == id || viewer.visible.contains_key(&id) {
                send(vid, viewer, Event::Look { actor: id, look: look.clone() }, failed);
            }
        }
    }

    pub fn moved(&mut self, id: ActorId, pos: [f32; 3], rotation: f32, motion: Motion) {
        if self.casts.get(&id).is_some_and(|c| distance2(c.from, pos) > CAST_SLIDE.powi(2)) {
            self.interrupt(id, "moved", false);
        }
        let Some(actor) = self.actors.get_mut(&id) else { return };
        (actor.pos, actor.rotation) = (pos, rotation);
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid != id && viewer.visible.contains_key(&id) {
                send(vid, viewer, Event::Move { actor: id, pos, rotation, motion }, failed);
            }
        }
    }

    /// A player said something; those in range hear it (not the speaker: its client shows its own).
    pub fn chat(&mut self, id: ActorId, range: ChatRange, kind: u16, text: Arc<str>) {
        let Some(speaker) = self.actors.get(&id) else { return };
        let ActorKind::Player(look) = &speaker.kind else { return };
        let (name, content_id, at) = (Arc::<str>::from(look.name.as_str()), look.content_id, speaker.pos);
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == id {
                continue;
            }
            if range == ChatRange::Say && self.actors.get(&vid).is_none_or(|a| distance2(a.pos, at) > SAY_RANGE.powi(2))
            {
                continue;
            }
            let event = Event::Chat { actor: id, content_id, name: name.clone(), kind, text: text.clone() };
            send(vid, viewer, event, failed);
        }
    }

    /// Spawns what came into range of each player and despawns what left it, nearest first while
    /// the client has free slots.
    pub fn update_visibility(&mut self) {
        let (actors, failed) = (&self.actors, &mut self.failed);
        for (&vid, viewer) in &mut self.viewers {
            let Some(me) = actors.get(&vid).map(|a| a.pos) else { continue };
            let gone: Vec<(ActorId, u8)> = viewer
                .visible
                .iter()
                .filter(|(id, _)| actors.get(id).is_none_or(|a| distance2(a.pos, me) > DESPAWN_RANGE.powi(2)))
                .map(|(&id, &slot)| (id, slot))
                .collect();
            for (id, slot) in gone {
                viewer.visible.remove(&id);
                viewer.slots[slot as usize] = None;
                send(vid, viewer, Event::Despawn { slot, actor: id }, failed);
            }
            let mut near: Vec<(f32, &ActorView)> = actors
                .values()
                .filter(|a| a.id != vid && !viewer.visible.contains_key(&a.id))
                .map(|a| (distance2(a.pos, me), a))
                .filter(|(d, _)| *d <= SPAWN_RANGE.powi(2))
                .collect();
            near.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (_, actor) in near {
                let Some(slot) = viewer.free_slot() else { break };
                viewer.slots[slot as usize] = Some(actor.id);
                viewer.visible.insert(actor.id, slot);
                send(vid, viewer, Event::Spawn { slot, actor: actor.clone() }, failed);
            }
        }
        self.drop_failed();
    }

    /// Drops the players whose connections could not keep up (their event channel closes, so the
    /// connection ends).
    fn drop_failed(&mut self) {
        while let Some(id) = self.failed.pop() {
            if let Some(token) = self.viewers.get(&id).map(|v| v.token) {
                warn!(
                    territory = self.territory,
                    actor = format_args!("{id:#x}"),
                    "player's events back up: dropping it"
                );
                self.leave(id, token);
            }
        }
    }
}

/// The territory's loop: requests in order, and a visibility pass every
/// [`VISIBILITY_INTERVAL`].
pub(crate) async fn run(mut state: ZoneState, mut rx: mpsc::Receiver<Request>, shutdown: CancellationToken) {
    info!(territory = state.territory, npcs = state.actors(), "territory up");
    let mut tick = tokio::time::interval(VISIBILITY_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            request = rx.recv() => match request {
                Some(r) => state.handle(r),
                None => break,
            },
            _ = tick.tick() => {
                let started = Instant::now();
                state.combat_tick(started);
                state.update_visibility();
                if started.elapsed() > SLOW_PASS {
                    warn!(territory = state.territory, players = state.players(), took = ?started.elapsed(), "slow visibility pass");
                }
            }
        }
    }
    info!(territory = state.territory, "territory down");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// By when a battle NPC drawn into a fight has swung once: its swing time and Sapphire's
    /// 500-1000 ms on top.
    const FIRST_SWING: Duration = Duration::from_millis(3500);

    fn look(name: &str) -> Arc<PlayerLook> {
        Arc::new(PlayerLook { name: name.into(), content_id: 7, ..Default::default() })
    }

    fn drain(rx: &mut mpsc::Receiver<Event>) -> Vec<Event> {
        let mut all = vec![];
        while let Ok(e) = rx.try_recv() {
            all.push(e);
        }
        all
    }

    fn spawned(events: &[Event]) -> Vec<ActorId> {
        events.iter().filter_map(|e| if let Event::Spawn { actor, .. } = e { Some(actor.id) } else { None }).collect()
    }

    const A: ActorId = 0x1000_0001;
    const B: ActorId = 0x1000_0002;

    #[test]
    fn players_spawn_for_each_other_in_range() {
        let npc = NpcPlacement { enpc: 1000, pos: [5.0, 0.0, 0.0], rotation: 0.0 };
        let far_npc = NpcPlacement { enpc: 1001, pos: [500.0, 0.0, 0.0], rotation: 0.0 };
        let mut zone = ZoneState::new(129, &[npc, far_npc], &[]);
        let (tx_a, mut a) = mpsc::channel(64);
        let (tx_b, mut b) = mpsc::channel(64);
        zone.enter(A, 1, look("Alpha"), [0.0; 3], 0.0, tx_a);
        let npc_id = NPC_ACTOR_BASE + (129 << 12) + 1;
        assert_eq!(spawned(&drain(&mut a)), vec![npc_id], "the near NPC, not the far one");

        zone.enter(B, 2, look("Beta"), [10.0, 0.0, 0.0], 0.0, tx_b);
        let to_b = drain(&mut b);
        assert_eq!(spawned(&to_b), vec![npc_id, A], "nearest first");
        let Event::Spawn { slot, actor } = &to_b[1] else { panic!() };
        assert_eq!(*slot, 2);
        assert!(matches!(&actor.kind, ActorKind::Player(l) if l.name == "Alpha"));
        let to_a = drain(&mut a);
        assert_eq!(spawned(&to_a), vec![B]);

        // A moves: B is told, A is not.
        zone.moved(A, [1.0, 0.0, 0.0], 1.0, Motion::default());
        assert!(matches!(drain(&mut b).as_slice(), [Event::Move { actor: A, pos: [1.0, 0.0, 0.0], .. }]));
        assert!(drain(&mut a).is_empty());

        // B walks off: past the despawn range, each sees the other go.
        zone.moved(B, [200.0, 0.0, 0.0], 0.0, Motion::default());
        zone.update_visibility();
        assert!(drain(&mut a).contains(&Event::Despawn { slot: 2, actor: B }));
        let to_b = drain(&mut b);
        assert!(to_b.contains(&Event::Despawn { slot: 2, actor: A }));
        assert!(to_b.contains(&Event::Despawn { slot: 1, actor: npc_id }));

        // and back
        zone.moved(B, [3.0, 0.0, 0.0], 0.0, Motion::default());
        zone.update_visibility();
        assert_eq!(spawned(&drain(&mut a)), vec![B]);
    }

    #[test]
    fn leaving_despawns_and_stale_leaves_are_ignored() {
        let mut zone = ZoneState::new(129, &[], &[]);
        let (tx_a, mut a) = mpsc::channel(64);
        let (tx_b, _b) = mpsc::channel(64);
        zone.enter(A, 1, look("Alpha"), [0.0; 3], 0.0, tx_a);
        zone.enter(B, 2, look("Beta"), [1.0, 0.0, 0.0], 0.0, tx_b);
        drain(&mut a);

        // B logs in again: the old entry goes, the new one comes.
        let (tx_b2, mut b2) = mpsc::channel(64);
        zone.enter(B, 3, look("Beta"), [1.0, 0.0, 0.0], 0.0, tx_b2);
        let to_a = drain(&mut a);
        assert_eq!(to_a.len(), 2);
        assert!(matches!(to_a[0], Event::Despawn { actor: B, .. }));
        assert!(matches!(to_a[1], Event::Spawn { .. }));
        assert_eq!(spawned(&drain(&mut b2)), vec![A]);

        // the old connection's leave arrives late and changes nothing
        zone.leave(B, 2);
        assert_eq!(zone.players(), 2);
        zone.leave(B, 3);
        assert_eq!(zone.players(), 1);
        assert!(matches!(drain(&mut a).as_slice(), [Event::Despawn { actor: B, .. }]));
    }

    #[test]
    fn say_carries_only_so_far_and_shout_everywhere() {
        let mut zone = ZoneState::new(129, &[], &[]);
        let (tx_a, mut a) = mpsc::channel(64);
        let (tx_b, mut b) = mpsc::channel(64);
        let (tx_c, mut c) = mpsc::channel(64);
        zone.enter(A, 1, look("Alpha"), [0.0; 3], 0.0, tx_a);
        zone.enter(B, 2, look("Beta"), [10.0, 0.0, 0.0], 0.0, tx_b);
        zone.enter(0x1000_0003, 3, look("Gamma"), [300.0, 0.0, 0.0], 0.0, tx_c);
        for rx in [&mut a, &mut b, &mut c] {
            drain(rx);
        }
        zone.chat(A, ChatRange::Say, 0x0a, "hello".into());
        let to_b = drain(&mut b);
        assert!(
            matches!(&to_b[..], [Event::Chat { actor: A, name, text, kind: 0x0a, content_id: 7 }] if &**name == "Alpha" && &**text == "hello")
        );
        assert!(drain(&mut c).is_empty());
        assert!(drain(&mut a).is_empty(), "the speaker's client shows its own message");

        zone.chat(A, ChatRange::Zone, 0x0b, "HELLO".into());
        assert_eq!(drain(&mut b).len(), 1);
        assert_eq!(drain(&mut c).len(), 1);
    }

    #[test]
    fn at_most_99_others_nearest_first() {
        let npcs: Vec<_> =
            (0..150).map(|i| NpcPlacement { enpc: i, pos: [i as f32 * 0.3, 0.0, 0.0], rotation: 0.0 }).collect();
        let mut zone = ZoneState::new(1, &npcs, &[]);
        let (tx, mut rx) = mpsc::channel(512);
        zone.enter(A, 1, look("Alpha"), [0.0; 3], 0.0, tx);
        let ids = spawned(&drain(&mut rx));
        assert_eq!(ids.len(), SLOTS - 1);
        assert_eq!(ids[0], NPC_ACTOR_BASE + (1 << 12) + 1);
        assert_eq!(*ids.last().unwrap(), NPC_ACTOR_BASE + (1 << 12) + 99);
    }

    #[test]
    fn battle_npcs_spawn_by_range_with_their_own_ids() {
        let bnpc = |x: f32| BnpcPlacement {
            base_id: 37,
            name_id: 37,
            level: 2,
            pos: [x, 0.0, 0.0],
            rotation: 0.5,
            aggressive: false,
            wander_range: 0.0,
        };
        let enpc = NpcPlacement { enpc: 1000914, pos: [3.0, 0.0, 0.0], rotation: 0.0 };
        let mut zone = ZoneState::new(134, &[enpc], &[bnpc(1.0), bnpc(400.0)]);
        assert_eq!(zone.actors(), 3);
        let (tx, mut rx) = mpsc::channel(64);
        zone.enter(A, 1, look("Alpha"), [0.0; 3], 0.0, tx);
        let events = drain(&mut rx);
        let first = BNPC_ACTOR_BASE + (134 << 14) + 1;
        assert_eq!(spawned(&events), vec![first, NPC_ACTOR_BASE + (134 << 12) + 1], "the near one, nearest first");
        let Event::Spawn { actor, .. } = &events[0] else { panic!() };
        assert_eq!(actor.kind, ActorKind::BattleNpc { base_id: 37, name_id: 37, level: 2, aggressive: false });
    }

    #[test]
    fn gear_changes_reach_the_player_and_who_sees_it() {
        let mut zone = ZoneState::new(129, &[], &[]);
        let (tx_a, mut a) = mpsc::channel(64);
        let (tx_b, mut b) = mpsc::channel(64);
        let (tx_c, mut c) = mpsc::channel(64);
        zone.enter(A, 1, look("Alpha"), [0.0; 3], 0.0, tx_a);
        zone.enter(B, 2, look("Beta"), [5.0, 0.0, 0.0], 0.0, tx_b);
        zone.enter(0x1000_0003, 3, look("Gamma"), [500.0, 0.0, 0.0], 0.0, tx_c);
        for rx in [&mut a, &mut b, &mut c] {
            drain(rx);
        }
        let sword = Arc::new(PlayerLook { main_weapon: 0x1_002b_00c9, ..(*look("Alpha")).clone() });
        zone.set_look(A, sword.clone());
        assert!(
            matches!(&drain(&mut a)[..], [Event::Look { actor: A, look }] if look.main_weapon == sword.main_weapon)
        );
        assert!(matches!(&drain(&mut b)[..], [Event::Look { actor: A, .. }]));
        assert!(drain(&mut c).is_empty(), "too far to see it");
        // and the territory remembers it: someone spawning Alpha later sees the sword
        zone.moved(0x1000_0003, [1.0, 0.0, 0.0], 0.0, Motion::default());
        zone.update_visibility();
        let to_c = drain(&mut c);
        let alpha = to_c.iter().find_map(|e| match e {
            Event::Spawn { actor, .. } if actor.id == A => Some(actor.clone()),
            _ => None,
        });
        assert!(matches!(alpha.unwrap().kind, ActorKind::Player(l) if l.main_weapon == sword.main_weapon));
    }

    fn fighter(name: &str) -> Arc<PlayerLook> {
        Arc::new(PlayerLook {
            level: 1,
            attributes: [20, 20, 20, 20, 20, 20],
            job_mods: [100; 6],
            // a starter weapon's damage, swinging every 3 s (the server's own swing time)
            weapon: crate::Weapon { physical: 8, magic: 8, delay_ms: combat::SWING.as_millis() as u32 },
            ..(*look(name)).clone()
        })
    }

    #[test]
    fn auto_attacks_land_on_the_swing_and_kill() {
        let rat = BnpcPlacement {
            base_id: 347,
            name_id: 417,
            level: 1,
            pos: [2.0, 0.0, 0.0],
            rotation: 0.0,
            aggressive: false,
            wander_range: 0.0,
        };
        let mut zone = ZoneState::new(129, &[], &[rat]);
        let rat_id = BNPC_ACTOR_BASE + (129 << 14) + 1;
        let (tx_a, mut a) = mpsc::channel(256);
        let (tx_b, mut b) = mpsc::channel(256);
        // tough enough to outlast the rat, which fights back
        zone.enter(A, 1, tough("Alpha"), [0.0; 3], 0.0, tx_a);
        zone.enter(B, 2, look("Beta"), [4.0, 0.0, 0.0], 0.0, tx_b);
        drain(&mut a);
        drain(&mut b);
        let (hp_max, damage) = (combat::bnpc_hp(1), combat::auto_attack_damage(&tough("Alpha")));
        assert_eq!(zone.health(rat_id), Some((hp_max, hp_max)));

        let t0 = Instant::now();
        zone.set_target(A, Some(rat_id));
        zone.set_auto_attack(A, true);
        zone.combat_tick(t0);
        // the first swing at once, seen by the attacker and whoever sees the rat (the rat's
        // battle stance with it)
        let damage_only = |events: Vec<Event>| -> Vec<Event> {
            events.into_iter().filter(|e| !matches!(e, Event::Order { .. })).collect()
        };
        assert!(
            matches!(damage_only(drain(&mut a))[..], [Event::Damage { source: A, target, amount, .. }] if target == rat_id && amount == damage)
        );
        assert_eq!(damage_only(drain(&mut b)).len(), 1);
        // not again before the swing time
        zone.combat_tick(t0 + Duration::from_secs(1));
        assert!(drain(&mut a).is_empty());

        // swing until it dies
        let mut t = t0;
        let mut died = None;
        for _ in 0..100 {
            t += combat::SWING;
            zone.combat_tick(t);
            if drain(&mut a).iter().any(|e| matches!(e, Event::Death { actor, .. } if *actor == rat_id)) {
                died = Some(t);
                break;
            }
        }
        let died = died.expect("it died");
        assert_eq!(zone.health(rat_id), Some((0, hp_max)));
        assert!(drain(&mut b).iter().any(|e| matches!(e, Event::Death { .. })));
        // no more swings at a dead rat
        zone.combat_tick(died + combat::SWING);
        assert!(drain(&mut a).is_empty());

        // the body goes, then the rat is back, whole
        zone.combat_tick(died + combat::CORPSE);
        zone.update_visibility();
        assert!(drain(&mut a).iter().any(|e| matches!(e, Event::Despawn { actor, .. } if *actor == rat_id)));
        zone.combat_tick(died + combat::RESPAWN);
        zone.update_visibility();
        assert!(spawned(&drain(&mut a)).contains(&rat_id));
        assert_eq!(zone.health(rat_id), Some((hp_max, hp_max)));
    }

    #[test]
    fn idle_ones_wander_near_their_spawn_at_a_walk() {
        let rat = BnpcPlacement {
            base_id: 347,
            name_id: 417,
            level: 1,
            pos: [10.0, 0.0, 10.0],
            rotation: 0.0,
            aggressive: false,
            wander_range: 8.0,
        };
        let mut zone = ZoneState::new(129, &[], &[rat]);
        let rat_id = BNPC_ACTOR_BASE + (129 << 14) + 1;
        let (tx, mut a) = mpsc::channel(4096);
        zone.enter(A, 1, look("Alpha"), [0.0; 3], 0.0, tx);
        zone.update_visibility();
        drain(&mut a);
        let t0 = Instant::now();
        let mut moves = 0;
        for i in 0..400 {
            zone.combat_tick(t0 + Duration::from_millis(250 * i));
            for e in drain(&mut a) {
                if let Event::Move { actor, pos, motion, .. } = e
                    && actor == rat_id
                {
                    moves += 1;
                    assert_eq!(motion.flags, Motion::WALKING);
                    assert!(distance2(pos, [10.0, 0.0, 10.0]).sqrt() <= 8.0 + 0.01, "{pos:?}");
                }
            }
        }
        assert!(moves > 10, "100 s: it walked somewhere ({moves} moves)");
    }

    #[test]
    fn leashed_ones_run_home_rather_than_appear_there() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        // far past the leash
        zone.moved(A, [300.0, 0.0, 0.0], 0.0, Motion::default());
        zone.actors.get_mut(&rat_id).unwrap().pos = [80.0, 0.0, 0.0];
        zone.combat_tick(t0 + Duration::from_millis(250));
        assert!(zone.returning.contains(&rat_id));
        drain(&mut a);
        let mut t = t0 + Duration::from_millis(250);
        let mut steps = Vec::new();
        while zone.returning.contains(&rat_id) && steps.len() < 200 {
            t += Duration::from_millis(250);
            zone.combat_tick(t);
            steps.push(zone.actors[&rat_id].pos[0]);
        }
        assert!(steps.len() > 10, "a run, not a jump ({} steps)", steps.len());
        assert!(steps.windows(2).all(|w| w[1] <= w[0]), "always nearer");
        assert_eq!(zone.actors[&rat_id].pos, [3.0, 0.0, 0.0]);
    }

    #[test]
    fn an_archer_shoots_from_afar() {
        let rat = BnpcPlacement {
            base_id: 347,
            name_id: 417,
            level: 1,
            pos: [20.0, 0.0, 0.0],
            rotation: 0.0,
            aggressive: false,
            wander_range: 0.0,
        };
        let mut zone = ZoneState::new(129, &[], &[rat]);
        let rat_id = BNPC_ACTOR_BASE + (129 << 14) + 1;
        let (tx, mut a) = mpsc::channel(256);
        let archer = Arc::new(PlayerLook { class: 5, ..(*tough("Alpha")).clone() });
        zone.enter(A, 1, archer, [0.0; 3], 0.0, tx);
        drain(&mut a);
        zone.set_target(A, Some(rat_id));
        zone.set_auto_attack(A, true);
        zone.combat_tick(Instant::now());
        assert!(drain(&mut a).iter().any(|e| matches!(e, Event::Damage { target, .. } if *target == rat_id)), "20 yalms: in reach");
    }

    #[test]
    fn aggressive_ones_leave_much_higher_levels_alone() {
        let bomb = BnpcPlacement {
            base_id: 347,
            name_id: 417,
            level: 1,
            pos: [10.0, 0.0, 0.0],
            rotation: 0.0,
            aggressive: true,
            wander_range: 0.0,
        };
        assert_eq!(combat::aggro_range(11, 1), 0.0);
        assert!(combat::aggro_range(3, 1) < combat::AGGRO_RANGE);
        let mut zone = ZoneState::new(129, &[], &[bomb]);
        let (tx, _a) = mpsc::channel(256);
        let high = Arc::new(PlayerLook { level: 11, ..(*tough("Alpha")).clone() });
        zone.enter(A, 1, high, [0.0; 3], 0.0, tx);
        zone.combat_tick(Instant::now());
        assert!(zone.mobs.is_empty(), "ten levels up: ignored");
        let mut zone = ZoneState::new(129, &[], &[bomb]);
        let (tx, _a) = mpsc::channel(256);
        zone.enter(A, 1, tough("Alpha"), [0.0; 3], 0.0, tx);
        zone.combat_tick(Instant::now());
        assert_eq!(zone.mobs.values().next().map(|m| m.target), Some(A), "10 yalms, same level: it comes");
    }

    #[test]
    fn it_goes_for_whoever_it_hates_most() {
        let (mut zone, _a, rat_id, t0) = in_a_fight(tough("Alpha"));
        let (tx, _b) = mpsc::channel(256);
        zone.enter(B, 2, tough("Beta"), [1.0, 0.0, 0.0], 0.0, tx);
        assert_eq!(zone.mobs[&rat_id].target, A);
        zone.add_hate(rat_id, B, 10_000, t0);
        zone.combat_tick(t0 + Duration::from_millis(250));
        assert_eq!(zone.mobs[&rat_id].target, B);
    }

    #[test]
    fn going_home_it_takes_no_damage_and_heals_on_the_way() {
        let (mut zone, _a, rat_id, t0) = in_a_fight(tough("Alpha"));
        let (hurt, hp_max) = zone.health(rat_id).unwrap();
        assert!(hurt < hp_max);
        zone.actors.get_mut(&rat_id).unwrap().pos = [30.0, 0.0, 0.0];
        zone.go_home(rat_id);
        zone.strike(A, rat_id, 1, ActionResultKey::AUTO_ATTACK, t0);
        assert_eq!(zone.health(rat_id).unwrap().0, hurt, "no damage");
        assert!(!zone.mobs.contains_key(&rat_id), "and no fight");
        zone.rest_returning(t0);
        assert_eq!(zone.health(rat_id).unwrap().0, (hurt + hp_max / 10 + 1).min(hp_max));
    }

    fn weaponskill(action: u16, sequence: u32, potency: u32) -> ActionUse {
        ActionUse {
            action,
            sequence,
            potency,
            range: combat::MELEE_RANGE,
            recast: Duration::ZERO,
            gcd: false,
            cast: Duration::ZERO,
            heal: false,
            mp_cost: 0,
            tp_cost: 0,
            combo_from: 0,
            combo_potency: 0,
            effects: [None; 2],
        }
    }

    fn damage_to(events: &[Event], target: ActorId) -> Option<u32> {
        events.iter().find_map(|e| match e {
            Event::Damage { target: t, amount, .. } if *t == target => Some(*amount),
            _ => None,
        })
    }

    #[test]
    fn combos_hit_harder_after_the_action_they_follow_and_not_after_too_long() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        zone.health.insert(rat_id, (100_000, 100_000));
        let look = tough("Alpha");
        let savage = ActionUse { combo_from: 9, combo_potency: 200, ..weaponskill(11, 2, 100) };
        // alone: its own potency
        assert!(zone.use_action(A, rat_id, savage, t0));
        assert_eq!(damage_to(&drain(&mut a), rat_id), Some(combat::action_damage(&look, 100)));
        // after Fast Blade: the combo's
        assert!(zone.use_action(A, rat_id, weaponskill(9, 3, 150), t0));
        drain(&mut a);
        assert!(zone.use_action(A, rat_id, ActionUse { sequence: 4, ..savage }, t0 + Duration::from_secs(3)));
        assert_eq!(damage_to(&drain(&mut a), rat_id), Some(combat::action_damage(&look, 200)));
        // past the window: its own again
        assert!(zone.use_action(A, rat_id, weaponskill(9, 5, 150), t0 + Duration::from_secs(4)));
        drain(&mut a);
        let late = t0 + Duration::from_secs(4) + combat::COMBO_WINDOW + Duration::from_secs(1);
        assert!(zone.use_action(A, rat_id, ActionUse { sequence: 6, ..savage }, late));
        assert_eq!(damage_to(&drain(&mut a), rat_id), Some(combat::action_damage(&look, 100)));
    }

    #[test]
    fn damage_over_time_ticks_on_the_server_tick_and_buffs_add_damage() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        zone.health.insert(rat_id, (100_000, 100_000));
        let look = tough("Alpha");
        let dot = crate::ActionEffect { id: 143, duration: Duration::from_secs(18), tick_potency: 25, ..Default::default() };
        let aero = ActionUse { effects: [Some(dot), None], ..weaponskill(121, 7, 50) };
        assert!(zone.use_action(A, rat_id, aero, t0));
        assert!(zone.statuses(rat_id, t0).iter().any(|s| s.id == 143));
        drain(&mut a);
        let before = zone.health(rat_id).unwrap().0;
        zone.combat_tick(t0 + combat::SERVER_TICK);
        assert_eq!(zone.health(rat_id).unwrap().0, before - combat::action_damage(&look, 25), "one tick");
        // no hit is shown for a tick: an HP update
        assert!(damage_to(&drain(&mut a), rat_id).is_none());
        // Raging Strikes: 20% more from its bearer
        let raging = crate::ActionEffect { id: 125, duration: Duration::from_secs(20), on_self: true, damage_percent: 20, ..Default::default() };
        let buff = ActionUse { potency: 0, range: 30.0, effects: [Some(raging), None], ..weaponskill(101, 8, 0) };
        assert!(zone.use_action(A, rat_id, buff, t0 + Duration::from_secs(4)));
        assert!(zone.statuses(A, t0 + Duration::from_secs(4)).iter().any(|s| s.id == 125));
        assert!(drain(&mut a).iter().any(|e| matches!(e, Event::ActionDone { source: A, action: 101, sequence: 8, .. })));
        assert!(zone.use_action(A, rat_id, weaponskill(9, 9, 150), t0 + Duration::from_secs(5)));
        assert_eq!(damage_to(&drain(&mut a), rat_id), Some(combat::action_damage(&look, 150) * 120 / 100));
    }

    #[test]
    fn chasers_go_around_walls() {
        // a floor 30 yalms square, a wall across it at x = 15 with a gap past z = 25
        let floor = |s: f32| vec![[[0.0, 0.0, 0.0], [s, 0.0, 0.0], [s, 0.0, s]], [[0.0, 0.0, 0.0], [s, 0.0, s], [0.0, 0.0, s]]];
        let mut tris = floor(30.0);
        tris.push([[15.0, 0.0, 0.0], [15.0, 5.0, 0.0], [15.0, 5.0, 25.0]]);
        tris.push([[15.0, 0.0, 0.0], [15.0, 5.0, 25.0], [15.0, 0.0, 25.0]]);
        let rat = BnpcPlacement {
            base_id: 347,
            name_id: 417,
            level: 1,
            pos: [5.0, 0.0, 5.0],
            rotation: 0.0,
            aggressive: false,
            wander_range: 0.0,
        };
        let mut zone = ZoneState::new(129, &[], &[rat]);
        zone.set_nav(Arc::new(crate::nav::NavGrid::build(&tris)));
        let rat_id = BNPC_ACTOR_BASE + (129 << 14) + 1;
        let (tx, mut a) = mpsc::channel(4096);
        let archer = Arc::new(PlayerLook { class: 5, ..(*tough("Alpha")).clone() });
        zone.enter(A, 1, archer, [25.0, 0.0, 5.0], 0.0, tx);
        zone.set_target(A, Some(rat_id));
        zone.set_auto_attack(A, true);
        let mut t = Instant::now();
        zone.combat_tick(t);
        zone.set_auto_attack(A, false);
        assert!(zone.mobs.contains_key(&rat_id), "it fights back");
        let mut through_gap = false;
        for _ in 0..200 {
            t += Duration::from_millis(250);
            zone.combat_tick(t);
            drain(&mut a);
            let p = zone.actors[&rat_id].pos;
            through_gap |= p[2] > 24.0;
            assert!(!(p[0] > 14.0 && p[0] < 16.0 && p[2] < 25.0), "through the wall at {p:?}");
            if distance2(p, [25.0, 0.0, 5.0]) <= combat::MELEE_RANGE.powi(2) {
                break;
            }
        }
        assert!(through_gap, "it went round");
        assert!(distance2(zone.actors[&rat_id].pos, [25.0, 0.0, 5.0]) <= combat::MELEE_RANGE.powi(2), "it got there");
    }

    #[test]
    fn no_swing_out_of_reach_or_at_a_changed_target() {
        let rat = BnpcPlacement {
            base_id: 347,
            name_id: 417,
            level: 1,
            pos: [20.0, 0.0, 0.0],
            rotation: 0.0,
            aggressive: false,
            wander_range: 0.0,
        };
        let mut zone = ZoneState::new(129, &[], &[rat]);
        let rat_id = BNPC_ACTOR_BASE + (129 << 14) + 1;
        let (tx, mut a) = mpsc::channel(256);
        zone.enter(A, 1, tough("Alpha"), [0.0; 3], 0.0, tx);
        drain(&mut a);
        let t0 = Instant::now();
        zone.set_target(A, Some(rat_id));
        zone.set_auto_attack(A, true);
        zone.combat_tick(t0);
        assert!(drain(&mut a).is_empty(), "20 yalms off");
        // walks up: it lands
        zone.moved(A, [18.0, 0.0, 0.0], 0.0, Motion::default());
        zone.combat_tick(t0 + Duration::from_millis(250));
        assert_eq!(drain(&mut a).iter().filter(|e| matches!(e, Event::Damage { .. })).count(), 1);
        // another target stops auto-attack (the rat, hit, keeps at it)
        zone.set_target(A, None);
        zone.combat_tick(t0 + Duration::from_secs(10));
        assert!(!drain(&mut a).iter().any(|e| matches!(e, Event::Damage { source: A, .. })));
    }

    #[test]
    fn actions_hit_by_potency_and_wait_out_their_recast() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        let fast_blade = |sequence| ActionUse {
            action: 9,
            sequence,
            potency: 150,
            range: combat::MELEE_RANGE,
            recast: combat::GCD,
            gcd: true,
            cast: Duration::ZERO,
            heal: false,
            mp_cost: 0,
            tp_cost: 70,
            combo_from: 0,
            combo_potency: 0,
            effects: [None; 2],
        };
        let look = tough("Alpha");
        let damage = combat::action_damage(&look, 150);
        assert!(damage > combat::auto_attack_damage(&look));
        let before = zone.health(rat_id).unwrap().0;
        assert!(zone.use_action(A, rat_id, fast_blade(41), t0));
        // the result names the action and echoes the request
        assert!(drain(&mut a).iter().any(|e| matches!(e,
            Event::Damage { source: A, action: 9, sequence: 41, amount, .. } if *amount == damage)));
        assert_eq!(zone.health(rat_id).unwrap().0, before - damage);
        // on the global cooldown: refused until it runs out, even as another weaponskill
        assert!(!zone.use_action(A, rat_id, fast_blade(42), t0 + Duration::from_secs(1)));
        assert!(!zone.use_action(A, rat_id, ActionUse { action: 10, ..fast_blade(43) }, t0 + Duration::from_secs(2)));
        // an ability has its own
        assert!(zone.use_action(A, rat_id, ActionUse { action: 16, gcd: false, ..fast_blade(44) }, t0));
        assert!(zone.use_action(A, rat_id, fast_blade(45), t0 + combat::GCD));
        // out of reach, and at nothing to hit
        zone.moved(A, [30.0, 0.0, 0.0], 0.0, Motion::default());
        assert!(!zone.use_action(A, rat_id, fast_blade(46), t0 + combat::GCD * 2));
        assert!(!zone.use_action(A, B, fast_blade(47), t0 + combat::GCD * 3));
    }

    fn tough(name: &str) -> Arc<PlayerLook> {
        Arc::new(PlayerLook { hp_max: 1000, ..(*fighter(name)).clone() })
    }

    /// A zone with one level-5 rat (enough HP to last) next to Alpha, who has hit it once at `t0`.
    fn in_a_fight(look: Arc<PlayerLook>) -> (ZoneState, mpsc::Receiver<Event>, ActorId, Instant) {
        let rat = BnpcPlacement {
            base_id: 347,
            name_id: 417,
            level: 5,
            pos: [3.0, 0.0, 0.0],
            rotation: 0.0,
            aggressive: false,
            wander_range: 0.0,
        };
        let mut zone = ZoneState::new(129, &[], &[rat]);
        let rat_id = BNPC_ACTOR_BASE + (129 << 14) + 1;
        let (tx, mut a) = mpsc::channel(1024);
        zone.enter(A, 1, look, [0.0; 3], 0.0, tx);
        drain(&mut a);
        let t0 = Instant::now();
        zone.set_target(A, Some(rat_id));
        zone.set_auto_attack(A, true);
        zone.combat_tick(t0);
        drain(&mut a);
        zone.set_auto_attack(A, false);
        (zone, a, rat_id, t0)
    }

    #[test]
    fn a_battle_npc_hit_fights_back() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        let before = zone.health(A).unwrap().0;
        // not before a swing's time
        zone.combat_tick(t0 + Duration::from_secs(1));
        assert!(!drain(&mut a).iter().any(|e| matches!(e, Event::Damage { target: A, .. })));
        zone.combat_tick(t0 + FIRST_SWING);
        let hit = drain(&mut a).into_iter().find_map(|e| match e {
            Event::Damage { source, target: A, amount, .. } => Some((source, amount)),
            _ => None,
        });
        assert_eq!(hit, Some((rat_id, combat::bnpc_damage(5))));
        assert_eq!(zone.health(A).unwrap().0, before - combat::bnpc_damage(5));
    }

    #[test]
    fn it_runs_after_its_target_and_gives_up_at_the_leash() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        // Alpha steps back 12 yalms: the rat follows, a step each tick
        zone.moved(A, [15.0, 0.0, 0.0], 0.0, Motion::default());
        let mut t = t0;
        for _ in 0..8 {
            t += Duration::from_millis(250);
            zone.combat_tick(t);
        }
        let moves = drain(&mut a).iter().filter(|e| matches!(e, Event::Move { actor, .. } if *actor == rat_id)).count();
        assert!(moves >= 4, "{moves} moves");
        let near = zone.actors[&rat_id].pos;
        assert!(distance2(near, [15.0, 0.0, 0.0]).sqrt() <= combat::MELEE_RANGE, "{near:?}");
        // far past the leash: it goes home, whole
        zone.moved(A, [200.0, 0.0, 0.0], 0.0, Motion::default());
        for _ in 0..80 {
            t += Duration::from_millis(250);
            zone.combat_tick(t);
        }
        assert_eq!(zone.actors[&rat_id].pos, [3.0, 0.0, 0.0]);
        assert_eq!(zone.health(rat_id), Some((combat::bnpc_hp(5), combat::bnpc_hp(5))));
    }

    #[test]
    fn a_player_can_die() {
        // one HP: the rat's first swing kills
        let (mut zone, mut a, rat_id, t0) = in_a_fight(fighter("Alpha"));
        zone.combat_tick(t0 + FIRST_SWING);
        let events = drain(&mut a);
        assert!(events.iter().any(|e| matches!(e, Event::Death { actor: A, killer: Some(k), .. } if *k == rat_id)));
        assert_eq!(zone.health(A).unwrap().0, 0);
        // the rat goes home; the dead swing no more
        zone.combat_tick(t0 + FIRST_SWING * 2);
        zone.set_auto_attack(A, true);
        zone.combat_tick(t0 + FIRST_SWING * 3);
        assert!(!drain(&mut a).iter().any(|e| matches!(e, Event::Damage { source: A, .. })));
    }

    fn cure(sequence: u32) -> ActionUse {
        ActionUse {
            action: 120,
            sequence,
            potency: 400,
            range: 30.0,
            recast: combat::GCD,
            gcd: true,
            cast: Duration::from_secs(2),
            heal: true,
            mp_cost: 0,
            tp_cost: 0,
            combo_from: 0,
            combo_potency: 0,
            effects: [None; 2],
        }
    }

    #[test]
    fn actions_cost_mp_and_tp_when_they_land_and_both_come_back_on_the_tick() {
        let caster = Arc::new(PlayerLook { mp_max: 200, ..(*tough("Alpha")).clone() });
        let (mut zone, mut a, rat_id, t0) = in_a_fight(caster);
        drain(&mut a);
        let costly = ActionUse { mp_cost: 133, ..cure(71) };
        // the MP goes when the cast lands, not when it starts
        assert!(zone.use_action(A, A, costly, t0));
        assert_eq!(zone.mana_of(A).mp, 200);
        zone.combat_tick(t0 + Duration::from_secs(2));
        assert_eq!(zone.mana_of(A).mp, 67);
        // not enough for another: refused before it casts
        assert!(!zone.use_action(A, A, ActionUse { sequence: 72, ..costly }, t0 + Duration::from_secs(3)));
        // a weaponskill's TP, at once
        let blade = ActionUse { action: 9, sequence: 73, potency: 150, range: combat::MELEE_RANGE, cast: Duration::ZERO,
                                heal: false, mp_cost: 0, tp_cost: 70, ..cure(73) };
        assert!(zone.use_action(A, rat_id, blade, t0 + Duration::from_secs(3)));
        let tp = zone.mana_of(A).tp;
        assert!(tp < combat::TP_MAX, "{tp}");
        // the server tick: in a fight (the rat is after Alpha), MP at 2% and TP by 60
        let before = zone.mana_of(A);
        zone.combat_tick(t0 + Duration::from_secs(9));
        let after = zone.mana_of(A);
        assert_eq!(after.mp, before.mp + 200 * combat::MP_REGEN_FIGHTING_PERCENT / 100);
        assert_eq!(after.tp, (before.tp + combat::TP_REGEN).min(combat::TP_MAX));
        // and the HUD hears of it, the MP and TP with the HP
        assert!(drain(&mut a).iter().any(|e| matches!(e, Event::Hp { actor: A, mana, .. } if *mana == after)));
    }

    #[test]
    fn a_cast_lands_when_it_ends_and_a_heal_goes_on_its_user() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        // the rat's swing hurts Alpha
        zone.combat_tick(t0 + FIRST_SWING);
        let hurt = zone.health(A).unwrap().0;
        drain(&mut a);
        let t1 = t0 + FIRST_SWING;
        // Cure with the rat targeted: on Alpha itself, its cast bar first
        assert!(zone.use_action(A, rat_id, cure(51), t1));
        assert!(drain(&mut a).iter().any(|e| matches!(e,
            Event::Cast { actor: A, action: 120, target: A, time } if *time == Duration::from_secs(2))));
        // not while it casts, and nothing lands before its end
        assert!(!zone.use_action(A, A, cure(52), t1 + Duration::from_millis(500)));
        zone.combat_tick(t1 + Duration::from_secs(1));
        assert!(!drain(&mut a).iter().any(|e| matches!(e, Event::Heal { .. })));
        zone.combat_tick(t1 + Duration::from_secs(2));
        let look = tough("Alpha");
        let heal = combat::heal_amount(&look, 400).min(look.hp_max - hurt);
        assert!(drain(&mut a).iter().any(|e| matches!(e,
            Event::Heal { source: A, target: A, action: 120, sequence: 51, amount, hp, .. }
                if *amount == heal && *hp == hurt + heal)));
        assert_eq!(zone.health(A).unwrap().0, hurt + heal);
    }

    #[test]
    fn moving_interrupts_a_cast_and_gives_its_recast_back() {
        let (mut zone, mut a, _, t0) = in_a_fight(tough("Alpha"));
        assert!(zone.use_action(A, A, cure(61), t0));
        // a step under the slide keeps it; one past it ends it
        zone.moved(A, [0.3, 0.0, 0.0], 0.0, Motion::default());
        assert!(!drain(&mut a).iter().any(|e| matches!(e, Event::CastEnd { .. })));
        zone.moved(A, [2.0, 0.0, 0.0], 0.0, Motion::default());
        // cancelled by its own doing, not interrupted
        assert!(drain(&mut a).iter().any(|e| matches!(e, Event::CastEnd { actor: A, action: 120, interrupted: false })));
        zone.combat_tick(t0 + Duration::from_secs(3));
        assert!(!drain(&mut a).iter().any(|e| matches!(e, Event::Heal { .. })));
        // cast again at once: the GCD was given back
        assert!(zone.use_action(A, A, cure(62), t0 + Duration::from_secs(3)));
    }

    fn statuses_told(events: &[Event], actor: ActorId) -> Option<Vec<u16>> {
        events.iter().rev().find_map(|e| match e {
            Event::Hp { actor: a, statuses, .. } | Event::Damage { target: a, statuses, .. } if *a == actor => {
                Some(statuses.iter().map(|s| s.id).collect())
            }
            _ => None,
        })
    }

    #[test]
    fn sprint_spends_the_tp_for_a_while_of_status_50() {
        let (mut zone, mut a, _rat, t0) = in_a_fight(tough("Alpha"));
        assert_eq!(zone.mana_of(A).tp, combat::TP_MAX);
        assert!(zone.sprint(A, t0));
        assert_eq!(zone.mana_of(A).tp, 0);
        assert_eq!(statuses_told(&drain(&mut a), A), Some(vec![combat::SPRINT_STATUS]));
        let s = zone.statuses(A, t0);
        assert_eq!(s[0].param, combat::SPRINT_PARAM);
        assert!((s[0].remaining - 20.0).abs() < 0.01, "1000 TP: 20 s, {}", s[0].remaining);
        // nothing left to spend
        assert!(!zone.sprint(A, t0));
    }

    #[test]
    fn statuses_go_on_refresh_and_wear_off_on_the_tick() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        assert!(zone.set_status(A, 50, 0, Some(Duration::from_secs(10)), A, t0));
        assert!(zone.set_status(A, 51, 2, Some(Duration::from_secs(20)), rat_id, t0));
        assert_eq!(statuses_told(&drain(&mut a), A), Some(vec![50, 51]));
        let slots = zone.statuses(A, t0 + Duration::from_secs(4));
        assert_eq!((slots[1].param, slots[1].source), (2, rat_id));
        assert!((slots[0].remaining - 6.0).abs() < 0.01, "{}", slots[0].remaining);
        // the same source's again: refreshed in its slot, no second one
        assert!(zone.set_status(A, 50, 0, Some(Duration::from_secs(30)), A, t0));
        assert_eq!(zone.statuses(A, t0).len(), 2);
        drain(&mut a);
        // 51 wears off after 50's refresh, and is told when it does
        zone.combat_tick(t0 + Duration::from_secs(21));
        assert_eq!(statuses_told(&drain(&mut a), A), Some(vec![50]));
        // a hit carries what is left: the client's HP update replaces them all
        zone.combat_tick(t0 + Duration::from_secs(21) + combat::SWING);
        assert!(drain(&mut a).iter().any(|e| matches!(e,
            Event::Damage { target: A, statuses, .. } if statuses.iter().map(|s| s.id).eq([50]))));
        // taken off by hand
        assert!(zone.set_status(A, 50, 0, None, 0, t0));
        assert!(zone.statuses(A, t0).is_empty());
    }

    #[test]
    fn thirty_statuses_at_most_and_none_on_the_dead() {
        let (mut zone, mut a, _, t0) = in_a_fight(fighter("Alpha"));
        for id in 0..STATUS_SLOTS as u16 {
            assert!(zone.set_status(A, 100 + id, 0, Some(Duration::from_secs(60)), A, t0));
        }
        assert!(!zone.set_status(A, 999, 0, Some(Duration::from_secs(60)), A, t0));
        // one HP: the rat's swing kills, and its hit says no statuses are left
        zone.combat_tick(t0 + FIRST_SWING);
        assert_eq!(statuses_told(&drain(&mut a), A), Some(vec![]));
        assert!(zone.statuses(A, t0).is_empty());
        assert!(!zone.set_status(A, 50, 0, Some(Duration::from_secs(10)), A, t0));
    }

    #[test]
    fn hp_comes_back_on_the_server_tick_out_of_a_fight_and_the_dead_get_up() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        zone.combat_tick(t0 + FIRST_SWING);
        let hurt = zone.health(A).unwrap().0;
        assert!(hurt < 1000);
        // in a fight: nothing back
        zone.combat_tick(t0 + FIRST_SWING + combat::SERVER_TICK);
        assert!(zone.health(A).unwrap().0 <= hurt);
        // out of it (the rat sent home by leaving its reach for good)
        zone.go_home(rat_id);
        drain(&mut a);
        let t = t0 + FIRST_SWING + combat::SERVER_TICK * 2;
        zone.combat_tick(t);
        let back = zone.health(A).unwrap().0;
        assert!(back > hurt, "{back} > {hurt}");
        assert!(drain(&mut a).iter().any(|e| matches!(e, Event::Hp { actor: A, hp, .. } if *hp == back)));

        // the dead do not regenerate; revived, they are whole
        zone.health.insert(A, (0, 1000));
        zone.combat_tick(t + combat::SERVER_TICK);
        assert_eq!(zone.health(A), Some((0, 1000)));
        zone.revive(A);
        assert_eq!(zone.health(A), Some((1000, 1000)));
        zone.revive(A);
        assert_eq!(zone.health(A), Some((1000, 1000)), "only the dead");
    }

    #[test]
    fn aggressive_ones_attack_who_comes_near_passive_ones_do_not() {
        let rat = BnpcPlacement {
            base_id: 347,
            name_id: 417,
            level: 1,
            pos: [3.0, 0.0, 0.0],
            rotation: 0.0,
            aggressive: false,
            wander_range: 0.0,
        };
        let bomb = BnpcPlacement {
            base_id: 99,
            name_id: 99,
            level: 5,
            pos: [20.0, 0.0, 0.0],
            rotation: 0.0,
            aggressive: true,
            wander_range: 0.0,
        };
        let mut zone = ZoneState::new(129, &[], &[rat, bomb]);
        let (rat_id, bomb_id) = (BNPC_ACTOR_BASE + (129 << 14) + 1, BNPC_ACTOR_BASE + (129 << 14) + 2);
        let (tx, mut a) = mpsc::channel(1024);
        zone.enter(A, 1, tough("Alpha"), [0.0; 3], 0.0, tx);
        drain(&mut a);
        let t0 = Instant::now();
        // the rat stands beside Alpha and does nothing; the bomb is out of sight
        for i in 1..=8 {
            zone.combat_tick(t0 + Duration::from_millis(250 * i));
        }
        assert!(!drain(&mut a).iter().any(|e| matches!(e, Event::Damage { target: A, .. })));
        // Alpha walks up to the bomb: it attacks
        zone.moved(A, [14.0, 0.0, 0.0], 0.0, Motion::default());
        let mut t = t0 + Duration::from_secs(2);
        let mut hit_by = None;
        for _ in 0..20 {
            t += Duration::from_millis(250);
            zone.combat_tick(t);
            if let Some(source) = drain(&mut a).iter().find_map(|e| match e {
                Event::Damage { source, target: A, .. } => Some(*source),
                _ => None,
            }) {
                hit_by = Some(source);
                break;
            }
        }
        assert_eq!(hit_by, Some(bomb_id));
        assert_ne!(hit_by, Some(rat_id));
        // the dead draw nothing
        zone.health.insert(A, (0, 1000));
        zone.go_home(bomb_id);
        zone.combat_tick(t + Duration::from_millis(250));
        assert!(!zone.mobs.contains_key(&bomb_id));
    }

    #[test]
    fn a_player_that_cannot_keep_up_is_dropped() {
        let mut zone = ZoneState::new(129, &[], &[]);
        let (tx_a, mut a) = mpsc::channel(64);
        let (tx_b, b) = mpsc::channel(1);
        zone.enter(A, 1, look("Alpha"), [0.0; 3], 0.0, tx_a);
        zone.enter(B, 2, look("Beta"), [1.0, 0.0, 0.0], 0.0, tx_b);
        // B's one slot holds A's spawn; A's moves overflow it
        for i in 0..3 {
            zone.moved(A, [i as f32, 0.0, 0.0], 0.0, Motion::default());
            zone.drop_failed();
        }
        assert_eq!(zone.players(), 1);
        assert!(drain(&mut a).iter().any(|e| matches!(e, Event::Despawn { actor: B, .. })));
        drop(b);
    }
}
