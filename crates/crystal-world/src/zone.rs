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
}

/// A battle NPC in a fight: where it came from, who it is after, when it next swings.
#[derive(Debug, Clone, Copy)]
struct Mob {
    home: [f32; 3],
    target: ActorId,
    next_swing: Option<Instant>,
}

/// The action a hit came from, and the player's request it answers (0: none).
#[derive(Debug, Clone, Copy)]
struct ActionResultKey {
    action: u16,
    sequence: u32,
}

impl ActionResultKey {
    const AUTO_ATTACK: Self = Self { action: 7, sequence: 0 };
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
            let kind = ActorKind::BattleNpc { base_id: b.base_id, name_id: b.name_id, level: b.level };
            (id, ActorView { id, kind, pos: b.pos, rotation: b.rotation })
        });
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
        }
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
                let status = Status { id, param, until: now + d, source };
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

    /// Takes every status off `target`.
    pub fn clear_statuses(&mut self, target: ActorId, now: Instant) {
        if self.statuses.remove(&target).is_some_and(|list| !list.is_empty()) {
            self.tell_hp(target, now);
        }
    }

    /// Takes off the statuses that wore off by `now`.
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
        let kind = ActorKind::BattleNpc { base_id: b.base_id, name_id: b.name_id, level: b.level };
        self.actors.insert(id, ActorView { id, kind, pos: b.pos, rotation: b.rotation });
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
            self.regenerate();
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

    /// Battle NPCs in a fight: past the leash they go home; out of reach they run after their
    /// target; in reach they swing on their timer.
    fn mob_tick(&mut self, now: Instant, dt: f32) {
        self.aggro();
        let ids: Vec<ActorId> = self.mobs.keys().copied().collect();
        for id in ids {
            let Some(mob) = self.mobs.get(&id).copied() else { continue };
            let (Some(me), Some(them)) = (self.actors.get(&id), self.actors.get(&mob.target)) else {
                self.go_home(id);
                continue;
            };
            let target_alive = self.health.get(&mob.target).is_some_and(|(hp, _)| *hp > 0);
            if !target_alive || distance2(me.pos, mob.home) > combat::LEASH.powi(2) {
                self.go_home(id);
                continue;
            }
            let (here, there) = (me.pos, them.pos);
            let d = distance2(here, there).sqrt();
            let reach = combat::MELEE_RANGE * 0.8;
            if d > reach {
                // a step toward the target, stopping just inside reach
                let step = (combat::CHASE_SPEED * dt).min(d - reach * 0.75);
                let dir = [(there[0] - here[0]) / d, (there[1] - here[1]) / d, (there[2] - here[2]) / d];
                let pos = [here[0] + dir[0] * step, here[1] + dir[1] * step, here[2] + dir[2] * step];
                self.place(id, pos, dir[0].atan2(dir[2]));
            } else if mob.next_swing.is_none_or(|t| t <= now) {
                self.mob_swing(id, mob.target, now);
            }
        }
    }

    /// Aggressive battle NPCs, alive and not fighting, go for the nearest live player in sight.
    fn aggro(&mut self) {
        let players: Vec<(ActorId, [f32; 3])> = self
            .viewers
            .keys()
            .filter(|id| self.health.get(id).is_some_and(|(hp, _)| *hp > 0))
            .filter_map(|id| self.actors.get(id).map(|a| (*id, a.pos)))
            .collect();
        if players.is_empty() {
            return;
        }
        let mut found = Vec::new();
        for id in &self.aggressive {
            if self.mobs.contains_key(id) || self.health.get(id).is_none_or(|(hp, _)| *hp == 0) {
                continue;
            }
            let Some(me) = self.actors.get(id) else { continue };
            let nearest = players
                .iter()
                .map(|(p, pos)| (distance2(me.pos, *pos), *p))
                .filter(|(d, _)| *d <= combat::AGGRO_RANGE.powi(2))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, target)) = nearest {
                found.push((*id, me.pos, target));
            }
        }
        for (id, home, target) in found {
            self.mobs.insert(id, Mob { home, target, next_swing: None });
        }
    }

    /// Moves an actor and tells whoever sees it.
    fn place(&mut self, id: ActorId, pos: [f32; 3], rotation: f32) {
        let Some(actor) = self.actors.get_mut(&id) else { return };
        (actor.pos, actor.rotation) = (pos, rotation);
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if viewer.visible.contains_key(&id) {
                send(vid, viewer, Event::Move { actor: id, pos, rotation, motion: Motion::default() }, failed);
            }
        }
    }

    /// A battle NPC gives up: back where it stood, whole again.
    fn go_home(&mut self, id: ActorId) {
        let Some(mob) = self.mobs.remove(&id) else { return };
        if let Some(ActorKind::BattleNpc { level, .. }) = self.actors.get(&id).map(|a| a.kind.clone()) {
            self.health.insert(id, (combat::bnpc_hp(level), combat::bnpc_hp(level)));
            let rotation = self.actors[&id].rotation;
            self.place(id, mob.home, rotation);
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
            m.next_swing = Some(now + combat::SWING);
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
                    send(vid, viewer, Event::Death { actor: target, killer: Some(id), level: player_level }, failed);
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
        let Some(look) = self.can_hit(attacker, target, combat::MELEE_RANGE) else { return };
        let amount = combat::auto_attack_damage(look.level, look.attributes[0]);
        if let Some(f) = self.fighters.get_mut(&attacker) {
            f.next_swing = Some(now + combat::SWING);
        }
        self.strike(attacker, target, amount, ActionResultKey::AUTO_ATTACK, now);
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
        let target = if action.heal { self.heal_target(id, target) } else { target };
        let ready = if action.heal { self.can_heal(id, target, action.range) } else { self.can_hit(id, target, action.range) };
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
        let look = if action.heal { self.can_heal(id, target, action.range) } else { self.can_hit(id, target, action.range) };
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
            let amount = combat::heal_amount(look.level, look.attributes[4], action.potency);
            self.restore(id, target, amount, key, now);
        }
        else {
            let amount = combat::action_damage(look.level, look.attributes[0], action.potency);
            self.strike(id, target, amount, key, now);
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
        let Some((hp, hp_max)) = self.health.get(&target).copied() else { return };
        let Some(t) = self.actors.get(&target) else { return };
        let ActorKind::BattleNpc { level: target_level, .. } = t.kind else { return };
        let home = t.pos;
        let amount = amount.min(hp);
        let hp = hp - amount;
        self.health.insert(target, (hp, hp_max));
        if hp == 0 {
            self.statuses.remove(&target);
        }
        let statuses = self.statuses(target, now);
        let mana = self.mana_of(target);
        let ActionResultKey { action, sequence } = key;
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == attacker || viewer.visible.contains_key(&target) {
                // the request's sequence is the user's alone
                let sequence = if vid == attacker { sequence } else { 0 };
                let event = Event::Damage {
                    source: attacker,
                    target,
                    action,
                    sequence,
                    amount,
                    hp,
                    hp_max,
                    level: target_level,
                    class: 0,
                    statuses: statuses.clone(),
                    mana,
                };
                send(vid, viewer, event, failed);
                if hp == 0 {
                    send(
                        vid,
                        viewer,
                        Event::Death { actor: target, killer: Some(attacker), level: target_level },
                        failed,
                    );
                }
            }
        }
        if hp > 0 {
            // it fights back (its first swing a swing's time after this one)
            self.mobs.entry(target).or_insert(Mob { home, target: attacker, next_swing: Some(now + combat::SWING) });
        }
        if hp == 0 {
            self.mobs.remove(&target);
            debug!(territory = self.territory, target = format_args!("{target:#x}"), "battle NPC died");
            for f in self.fighters.values_mut().filter(|f| f.target == Some(target)) {
                f.auto_attack = false;
            }
            self.corpses.push((now + combat::CORPSE, target));
            if let Some(view) = self.actors.get(&target) {
                self.respawns.push((now + combat::RESPAWN, view.clone()));
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
        assert_eq!(actor.kind, ActorKind::BattleNpc { base_id: 37, name_id: 37, level: 2 });
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
        Arc::new(PlayerLook { level: 1, attributes: [20, 20, 20, 20, 20, 20], ..(*look(name)).clone() })
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
        let (hp_max, damage) = (combat::bnpc_hp(1), combat::auto_attack_damage(1, 20));
        assert_eq!(zone.health(rat_id), Some((hp_max, hp_max)));

        let t0 = Instant::now();
        zone.set_target(A, Some(rat_id));
        zone.set_auto_attack(A, true);
        zone.combat_tick(t0);
        // the first swing at once, seen by the attacker and whoever sees the rat
        assert!(
            matches!(drain(&mut a)[..], [Event::Damage { source: A, target, amount, .. }] if target == rat_id && amount == damage)
        );
        assert_eq!(drain(&mut b).len(), 1);
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
    fn no_swing_out_of_reach_or_at_a_changed_target() {
        let rat = BnpcPlacement {
            base_id: 347,
            name_id: 417,
            level: 1,
            pos: [20.0, 0.0, 0.0],
            rotation: 0.0,
            aggressive: false,
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
        };
        let look = tough("Alpha");
        let damage = combat::action_damage(look.level, look.attributes[0], 150);
        assert!(damage > combat::auto_attack_damage(look.level, look.attributes[0]));
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
        zone.combat_tick(t0 + combat::SWING);
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
        zone.combat_tick(t0 + combat::SWING);
        let events = drain(&mut a);
        assert!(events.iter().any(|e| matches!(e, Event::Death { actor: A, killer: Some(k), .. } if *k == rat_id)));
        assert_eq!(zone.health(A).unwrap().0, 0);
        // the rat goes home; the dead swing no more
        zone.combat_tick(t0 + combat::SWING * 2);
        zone.set_auto_attack(A, true);
        zone.combat_tick(t0 + combat::SWING * 3);
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
        zone.combat_tick(t0 + combat::SWING);
        let hurt = zone.health(A).unwrap().0;
        drain(&mut a);
        let t1 = t0 + combat::SWING;
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
        let heal = combat::heal_amount(look.level, look.attributes[4], 400).min(look.hp_max - hurt);
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
        zone.combat_tick(t0 + combat::SWING);
        assert_eq!(statuses_told(&drain(&mut a), A), Some(vec![]));
        assert!(zone.statuses(A, t0).is_empty());
        assert!(!zone.set_status(A, 50, 0, Some(Duration::from_secs(10)), A, t0));
    }

    #[test]
    fn hp_comes_back_on_the_server_tick_out_of_a_fight_and_the_dead_get_up() {
        let (mut zone, mut a, rat_id, t0) = in_a_fight(tough("Alpha"));
        zone.combat_tick(t0 + combat::SWING);
        let hurt = zone.health(A).unwrap().0;
        assert!(hurt < 1000);
        // in a fight: nothing back
        zone.combat_tick(t0 + combat::SWING + combat::SERVER_TICK);
        assert!(zone.health(A).unwrap().0 <= hurt);
        // out of it (the rat sent home by leaving its reach for good)
        zone.go_home(rat_id);
        drain(&mut a);
        let t = t0 + combat::SWING + combat::SERVER_TICK * 2;
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
        };
        let bomb = BnpcPlacement {
            base_id: 99,
            name_id: 99,
            level: 5,
            pos: [20.0, 0.0, 0.0],
            rotation: 0.0,
            aggressive: true,
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
