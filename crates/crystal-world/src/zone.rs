//! One territory: its actors, who sees whom, and the loop that owns them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    ActorId, ActorKind, ActorView, BNPC_ACTOR_BASE, BnpcPlacement, ChatRange, DESPAWN_RANGE, Event, Motion,
    NPC_ACTOR_BASE, NpcPlacement, PlayerLook, SAY_RANGE, SLOTS, SPAWN_RANGE, VISIBILITY_INTERVAL, combat,
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
}

/// A battle NPC in a fight: where it came from, who it is after, when it next swings.
#[derive(Debug, Clone, Copy)]
struct Mob {
    home: [f32; 3],
    target: ActorId,
    next_swing: Option<Instant>,
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
        }
    }

    /// Places a battle NPC now (players see it on the next visibility pass); its actor id.
    pub fn add_bnpc(&mut self, b: BnpcPlacement) -> ActorId {
        self.bnpcs += 1;
        let id = BNPC_ACTOR_BASE + ((self.territory as u32) << 14) + self.bnpcs;
        let kind = ActorKind::BattleNpc { base_id: b.base_id, name_id: b.name_id, level: b.level };
        self.actors.insert(id, ActorView { id, kind, pos: b.pos, rotation: b.rotation });
        self.health.insert(id, (combat::bnpc_hp(b.level), combat::bnpc_hp(b.level)));
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
        let healing: Vec<(ActorId, u32, u32)> = self
            .viewers
            .keys()
            .filter(|id| !fighting.contains(id))
            .filter_map(|id| self.health.get(id).map(|(hp, max)| (*id, *hp, *max)))
            .filter(|(_, hp, max)| *hp > 0 && hp < max)
            .collect();
        for (id, hp, max) in healing {
            let hp = (hp + (max * combat::REGEN_PERCENT / 100).max(1)).min(max);
            self.set_hp(id, hp, max);
        }
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
        let Some(ActorKind::Player(look)) = self.actors.get(&id).map(|a| a.kind.clone()) else { return };
        let event = Event::Hp { actor: id, hp, hp_max, level: look.level as u16, class: look.class };
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == id || viewer.visible.contains_key(&id) {
                send(vid, viewer, event.clone(), failed);
            }
        }
    }

    /// Battle NPCs in a fight: past the leash they go home; out of reach they run after their
    /// target; in reach they swing on their timer.
    fn mob_tick(&mut self, now: Instant, dt: f32) {
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
        let player_level = look.level as u16;
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == target || viewer.visible.contains_key(&target) {
                send(
                    vid,
                    viewer,
                    Event::Damage { source: id, target, amount, hp, hp_max, level: player_level, class: look.class },
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
        let (Some(a), Some(t)) = (self.actors.get(&attacker), self.actors.get(&target)) else {
            if let Some(f) = self.fighters.get_mut(&attacker) {
                f.auto_attack = false;
            }
            return;
        };
        let ActorKind::Player(look) = &a.kind else { return };
        let ActorKind::BattleNpc { level: target_level, .. } = t.kind else { return };
        if distance2(a.pos, t.pos) > combat::MELEE_RANGE.powi(2) {
            return;
        }
        // the dead do not swing
        if self.health.get(&attacker).is_some_and(|(hp, _)| *hp == 0) {
            return;
        }
        let Some((hp, hp_max)) = self.health.get(&target).copied().filter(|(hp, _)| *hp > 0) else { return };
        let amount = combat::auto_attack_damage(look.level, look.attributes[0]).min(hp);
        let home = t.pos;
        let hp = hp - amount;
        self.health.insert(target, (hp, hp_max));
        if let Some(f) = self.fighters.get_mut(&attacker) {
            f.next_swing = Some(now + combat::SWING);
        }
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == attacker || viewer.visible.contains_key(&target) {
                send(
                    vid,
                    viewer,
                    Event::Damage { source: attacker, target, amount, hp, hp_max, level: target_level, class: 0 },
                    failed,
                );
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
        let failed = &mut self.failed;
        for (&vid, viewer) in &self.viewers {
            if vid == id || viewer.visible.contains_key(&id) {
                send(vid, viewer, Event::Look { actor: id, look: look.clone() }, failed);
            }
        }
    }

    pub fn moved(&mut self, id: ActorId, pos: [f32; 3], rotation: f32, motion: Motion) {
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
        let bnpc = |x: f32| BnpcPlacement { base_id: 37, name_id: 37, level: 2, pos: [x, 0.0, 0.0], rotation: 0.5 };
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
        let rat = BnpcPlacement { base_id: 347, name_id: 417, level: 1, pos: [2.0, 0.0, 0.0], rotation: 0.0 };
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
        let rat = BnpcPlacement { base_id: 347, name_id: 417, level: 1, pos: [20.0, 0.0, 0.0], rotation: 0.0 };
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

    fn tough(name: &str) -> Arc<PlayerLook> {
        Arc::new(PlayerLook { hp_max: 1000, ..(*fighter(name)).clone() })
    }

    /// A zone with one level-5 rat (enough HP to last) next to Alpha, who has hit it once at `t0`.
    fn in_a_fight(look: Arc<PlayerLook>) -> (ZoneState, mpsc::Receiver<Event>, ActorId, Instant) {
        let rat = BnpcPlacement { base_id: 347, name_id: 417, level: 5, pos: [3.0, 0.0, 0.0], rotation: 0.0 };
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
