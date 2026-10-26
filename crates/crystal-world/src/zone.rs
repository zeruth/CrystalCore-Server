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
    NPC_ACTOR_BASE, NpcPlacement, PlayerLook, SAY_RANGE, SLOTS, SPAWN_RANGE, VISIBILITY_INTERVAL,
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
        let actors = enpcs.chain(bnpcs).collect();
        Self { territory, actors, viewers: HashMap::new(), failed: Vec::new(), bnpcs: placed }
    }

    /// Places a battle NPC now (players see it on the next visibility pass); its actor id.
    pub fn add_bnpc(&mut self, b: BnpcPlacement) -> ActorId {
        self.bnpcs += 1;
        let id = BNPC_ACTOR_BASE + ((self.territory as u32) << 14) + self.bnpcs;
        let kind = ActorKind::BattleNpc { base_id: b.base_id, name_id: b.name_id, level: b.level };
        self.actors.insert(id, ActorView { id, kind, pos: b.pos, rotation: b.rotation });
        id
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
