//! The zone service: a character in the world.
//!
//! The client opens two connections to the address game login gives it, a zone one (frame
//! connection type 1) and a chat one (2), each starting with a session init that names the actor id
//! game login gave. Zone traffic is not encrypted: the zone channel has no crypt engine
//! (`ZoneProtoChannel` has none of the lobby's `LobbyCryptEngine`).
//!
//! Once the zone connection's player has loaded its territory, it enters that territory in the
//! [`World`]: its moves and chat go there, and what the territory tells it (others appearing,
//! moving, talking, going away) comes back as packets. The packet layouts are in
//! [`crystal_protocol::zone`] (re-exported as [`ipc`]).

mod aetherytes;
pub mod commands;
mod objects;
mod quests;

pub use crystal_protocol::zone as ipc;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use crystal_common::chara::CharaMake;
use crystal_common::gathering::Placement;
use crystal_common::quest::QuestDef;
use crystal_data::WorldData;
use crystal_db::{Character, Databases, InventoryRow, Position};
use crystal_net::codec::{FrameCodec, RawFrameCodec, decode_frame};
use crystal_net::ipc::{IpcHeader, packet, timestamp};
use crystal_net::{ConnectionType, Frame, Segment, SegmentType};
use crystal_state::State;
use crystal_world::inventory::{Inventory, Item, container};
use crystal_world::progress::Progress;
use crystal_world::{ActionUse, ActorKind, ActorView, ChatRange, Entry, Event, Mana, Motion, PlayerLook, StatusSlot, World};
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::commands::Commands;
use crate::ipc::{chat, opcode};

/// Player actor ids are this plus the character's row id (as the lobby's game login gives them).
pub const PLAYER_ACTOR_BASE: u32 = 0x1000_0000;
pub use crystal_world::NPC_ACTOR_BASE;

/// Where a character with no stored position stands: Limsa Lominsa's start (see
/// `crystal_common::chara::start_point(1)`).
pub const DEFAULT_POSITION: Position = Position { zone_id: 129, x: -62.0, y: 18.0, z: -4.0, rotation: 0.2 };

/// The Sprint action (`Action` row 3).
const SPRINT: u32 = 3;

/// Events sent together in one frame, at most.
const EVENTS_PER_FRAME: usize = 32;

pub use ipc::session_actor;

pub struct Zone {
    db: Databases,
    #[allow(dead_code)] // the lobby's session handoff
    state: State,
    /// The client's game data (zone exits); none when not configured.
    data: Option<Arc<WorldData>>,
    world: Arc<World>,
    commands: Commands,
    capture_dir: Option<PathBuf>,
    /// The quest definitions (`data/<build>/quests`), by quest id.
    quests: Arc<HashMap<u32, QuestDef>>,
    /// Where gathering points stand (`data/<build>/gathering.json`), by territory.
    gathering: Arc<HashMap<u16, Vec<Placement>>>,
}

/// How often a moving player's position is written to the database (and once more when the zone
/// connection closes).
pub const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(10);

/// An event the server has started for the client.
#[derive(Debug, Clone, Copy)]
struct RunningEvent {
    target: u64,
    event_id: u32,
    kind: u8,
    arg: u32,
}

/// One connection's player, once its session init named it.
struct Session {
    actor_id: u32,
    kind: ConnectionType,
    chara: Option<Character>,
    look: Arc<PlayerLook>,
    /// Where the player is now (from its position updates).
    position: Position,
    /// The position changed since it was last saved.
    moved: bool,
    last_save: Instant,
    /// The territory the player stands in, once it has loaded it.
    entry: Option<Entry>,
    /// The account's GM level (0: a player), and whether GM mode is on.
    gm_level: u8,
    gm_mode: bool,
    /// The character's items (the zone connection's; loaded at its session init).
    inventory: Inventory,
    /// The next item batch's sequence.
    item_sequence: u32,
    /// Level and experience per class slot (ClassJob col 4), and the current class's slot.
    progress: [Progress; 19],
    class_slot: usize,
    /// What the player has targeted (for actions sent without one).
    target: Option<u32>,
    /// The events the client is in, innermost last, as their starts named them: a finish must
    /// repeat the event, kind and argument (@0x6fd010). More than one when an event nests another
    /// (kind 7, which the client starts while one runs: @0x6f0be0).
    events: Vec<RunningEvent>,
    /// The character's active quests, as the client keeps them.
    quests: Vec<ipc::QuestSlot>,
    /// The quests it has completed.
    completed: Vec<u32>,
    /// The aetherytes the character is attuned to, and its unlocks.
    aetherytes: Vec<u32>,
    unlocks: Vec<u32>,
    /// The map parts it has revealed: (`Map` row, part).
    discovery: Vec<(u32, u8)>,
    /// A move begun with its effect (order 0xcf), made when its moment comes: (when, territory,
    /// position, rotation).
    pending_move: Option<(tokio::time::Instant, u16, [f32; 3], f32)>,
    /// The client's object slots: the gathering point in each.
    objects: [Option<u32>; ipc::object::SLOTS as usize],
    /// Where the player stood when the objects were last worked out.
    objects_at: Option<[f32; 3]>,
}

impl Session {
    fn new(
        actor_id: u32,
        kind: ConnectionType,
        chara: Option<Character>,
        gm_level: u8,
        inventory: Inventory,
        progress: [Progress; 19],
        data: Option<&WorldData>,
    ) -> Self {
        let position = chara.as_ref().and_then(|c| c.position).unwrap_or(DEFAULT_POSITION);
        let class = chara.as_ref().and_then(|c| CharaMake::parse(&c.appearance)).map_or(1, |m| m.class);
        let class_slot = data.and_then(|d| d.class_level_slot(class)).unwrap_or(0);
        let look = Arc::new(look_of(chara.as_ref(), &inventory, progress[class_slot].level, data));
        Self {
            actor_id,
            kind,
            chara,
            look,
            position,
            moved: false,
            last_save: Instant::now(),
            entry: None,
            gm_level,
            gm_mode: gm_level > 0,
            inventory,
            item_sequence: 1,
            progress,
            class_slot,
            target: None,
            events: Vec::new(),
            quests: Vec::new(),
            completed: Vec::new(),
            aetherytes: Vec::new(),
            unlocks: Vec::new(),
            discovery: Vec::new(),
            pending_move: None,
            objects: [None; ipc::object::SLOTS as usize],
            objects_at: None,
        }
    }

    fn pos(&self) -> [f32; 3] {
        let p = &self.position;
        [p.x as f32, p.y as f32, p.z as f32]
    }

    fn segment(&self, source: u32, op: u16, body: &[u8]) -> Segment {
        Segment::new(SegmentType::Ipc, source, self.actor_id, packet(op, timestamp(), body).freeze())
    }
}

/// What the connection waits on.
enum Input {
    Frame(Option<Result<Bytes, crystal_net::CodecError>>),
    /// `None`: the territory dropped the player.
    Event(Option<Event>),
    /// A move begun earlier is due.
    Move,
}

async fn next_move(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn next_event(session: &mut Option<Session>) -> Option<Event> {
    match session.as_mut().and_then(|s| s.entry.as_mut()) {
        Some(entry) => entry.events.recv().await,
        None => std::future::pending().await,
    }
}

impl Zone {
    pub fn new(
        db: Databases,
        state: State,
        data: Option<Arc<WorldData>>,
        world: Arc<World>,
        commands: Commands,
        capture_dir: Option<PathBuf>,
    ) -> Self {
        Self { db, state, data, world, commands, capture_dir, quests: Arc::default(), gathering: Arc::default() }
    }

    /// Where the gathering points the zone shows stand.
    pub fn with_gathering(mut self, points: Vec<Placement>) -> Self {
        let mut by_territory: HashMap<u16, Vec<Placement>> = HashMap::new();
        for p in points {
            by_territory.entry(p.territory).or_default().push(p);
        }
        self.gathering = Arc::new(by_territory);
        self
    }

    /// The quest definitions the zone runs.
    pub fn with_quests(mut self, quests: HashMap<u32, QuestDef>) -> Self {
        self.quests = Arc::new(quests);
        self
    }

    pub async fn handle(self: Arc<Self>, stream: TcpStream, peer: SocketAddr, shutdown: CancellationToken) {
        info!("connected");
        let mut capture = self.open_capture(peer).await;
        let (reader, writer) = stream.into_split();
        let mut frames = FramedRead::new(reader, RawFrameCodec);
        let mut out = FramedWrite::new(writer, FrameCodec);
        let mut session: Option<Session> = None;
        loop {
            let move_at = session.as_ref().and_then(|s| s.pending_move).map(|(at, ..)| at);
            let input = tokio::select! {
                _ = shutdown.cancelled() => break,
                next = frames.next() => Input::Frame(next),
                event = next_event(&mut session) => Input::Event(event),
                _ = next_move(move_at) => Input::Move,
            };
            let mut replies: Vec<Frame> = Vec::new();
            match input {
                Input::Frame(None) => break,
                Input::Frame(Some(Err(e))) => {
                    warn!("framing: {e}");
                    break;
                }
                Input::Frame(Some(Ok(raw))) => {
                    if let Some(file) = &mut capture
                        && let Err(e) = file.write_all(&raw).await
                    {
                        warn!("capture: {e}");
                        capture = None;
                    }
                    self.on_frame(raw, &mut session, &mut replies).await;
                }
                Input::Event(None) => {
                    warn!("dropped by its territory");
                    break;
                }
                Input::Move => {
                    let sess = session.as_mut().expect("a move is a session's");
                    if let Some((_, territory, pos, rotation)) = sess.pending_move.take() {
                        let packets = self.move_to(sess, territory, pos, rotation).await;
                        let segments: Vec<Segment> =
                            packets.iter().map(|(op, body)| sess.segment(sess.actor_id, *op, body)).collect();
                        replies.push(Frame::new(sess.kind, segments));
                    }
                }
                Input::Event(Some(first)) => {
                    let sess = session.as_mut().expect("events come to a session");
                    // the player's own kills, to credit after the frame
                    let mut kills: Vec<u16> = Vec::new();
                    let me = sess.actor_id;
                    let mut note = |e: &Event| {
                        if let Event::Death { killer: Some(k), level, .. } = e
                            && *k == me
                        {
                            kills.push(*level);
                        }
                    };
                    note(&first);
                    let mut segments: Vec<Segment> =
                        event_segment(sess, first, self.data.as_deref()).into_iter().collect();
                    while segments.len() < EVENTS_PER_FRAME
                        && let Some(Ok(e)) = sess.entry.as_mut().map(|e| e.events.try_recv())
                    {
                        note(&e);
                        segments.extend(event_segment(sess, e, self.data.as_deref()));
                    }
                    for level in kills {
                        let exp = crystal_world::combat::kill_exp(sess.look.level, level);
                        if exp > 0 {
                            for (op, body) in self.change_progress(sess, None, Some(exp)).await {
                                segments.push(sess.segment(sess.actor_id, op, &body));
                            }
                        }
                    }
                    if !segments.is_empty() {
                        replies.push(Frame::new(sess.kind, segments));
                    }
                }
            }
            let mut failed = false;
            for reply in replies {
                if let Err(e) = out.feed(reply).await {
                    warn!("send: {e}");
                    failed = true;
                    break;
                }
            }
            if failed || SinkExt::<Frame>::flush(&mut out).await.is_err() {
                break;
            }
        }
        if let Some(mut file) = capture {
            let _ = file.flush().await;
        }
        if let Some(mut sess) = session
            && sess.kind == ConnectionType::Zone
        {
            if let Some(entry) = sess.entry.take() {
                self.world.leave(entry).await;
            }
            self.save_position(&mut sess).await;
            self.save_inventory(&sess).await;
        }
        info!("closed");
    }

    async fn on_frame(&self, raw: Bytes, session: &mut Option<Session>, replies: &mut Vec<Frame>) {
        let frame = match decode_frame(raw.clone()) {
            Ok(frame) => frame,
            Err(e) => {
                warn!(bytes = %hex::encode(&raw), "undecodable frame: {e}");
                return;
            }
        };
        let kind = frame.header.connection_type;
        for segment in &frame.segments {
            let s = &segment.header;
            match s.segment_type {
                SegmentType::SessionInit => {
                    // The client's first segment on its zone and chat connections (0x38 bytes):
                    // the actor id game login gave it, as decimal text @4. Answered like the
                    // lobby's encryption reply, by a type-2 segment of 0x38 bytes whose u32 @0
                    // (not 0) becomes the connection's id.
                    let Some(actor_id) = session_actor(&segment.payload) else {
                        warn!("session init without an actor id");
                        continue;
                    };
                    let chara = self.character_of(actor_id).await;
                    let gm_level = match &chara {
                        Some(c) => self.db.login.gm_level(c.account_id).await.unwrap_or_else(|e| {
                            warn!("gm level: {e}");
                            0
                        }),
                        None => 0,
                    };
                    info!(
                        actor_id = format_args!("{actor_id:#x}"),
                        connection = ?kind,
                        character = chara.as_ref().map(|c| c.name.as_str()).unwrap_or("?"),
                        gm_level,
                        "session init"
                    );
                    let mut payload = vec![0u8; 0x38 - 0x10];
                    payload[..4].copy_from_slice(&actor_id.to_le_bytes());
                    let reply = Segment::new(SegmentType::Unknown(2), actor_id, actor_id, payload.into());
                    replies.push(Frame::new(kind, vec![reply]));
                    let inventory = match (&chara, kind) {
                        (Some(c), ConnectionType::Zone) => self.load_inventory(c).await,
                        _ => Inventory::default(),
                    };
                    let progress = match &chara {
                        Some(c) => self.load_progress(c).await,
                        None => Default::default(),
                    };
                    let quests = match &chara {
                        Some(c) => self.db.characters.quests(c.id).await.unwrap_or_else(|e| {
                            warn!("quests: {e}");
                            vec![]
                        }),
                        None => vec![],
                    };
                    let completed = match &chara {
                        Some(c) => self.db.characters.completed_quests(c.id).await.unwrap_or_else(|e| {
                            warn!("completed quests: {e}");
                            vec![]
                        }),
                        None => vec![],
                    };
                    let (aetherytes, unlocks, discovery) = match &chara {
                        Some(c) => {
                            let a = self.db.characters.aetherytes(c.id).await.unwrap_or_else(|e| {
                                warn!("aetherytes: {e}");
                                vec![]
                            });
                            let u = self.db.characters.unlocks(c.id).await.unwrap_or_else(|e| {
                                warn!("unlocks: {e}");
                                vec![]
                            });
                            let d = self.db.characters.discovery(c.id).await.unwrap_or_else(|e| {
                                warn!("discovery: {e}");
                                vec![]
                            });
                            (a, u, d)
                        }
                        None => (vec![], vec![], vec![]),
                    };
                    let mut s =
                        Session::new(actor_id, kind, chara, gm_level, inventory, progress, self.data.as_deref());
                    (s.aetherytes, s.unlocks, s.discovery) = (aetherytes, unlocks, discovery);
                    s.quests = quests
                        .into_iter()
                        .map(|q| ipc::QuestSlot { slot: q.slot, quest_id: q.quest_id, sequence: q.sequence, vars: q.vars })
                        .collect();
                    s.completed = completed;
                    *session = Some(s);
                }
                SegmentType::KeepAlive => {
                    let pong =
                        Segment::new(SegmentType::KeepAliveResponse, s.target, s.source, segment.payload.clone());
                    replies.push(Frame::new(kind, vec![pong]));
                }
                SegmentType::Ipc => {
                    let Some(sess) = session.as_mut() else {
                        warn!("ipc before the session init");
                        continue;
                    };
                    let Some(header) = IpcHeader::parse(&segment.payload) else {
                        warn!("short ipc");
                        continue;
                    };
                    let body = &segment.payload[IpcHeader::SIZE..];
                    let answers = self.answer(sess, header.opcode, body).await;
                    if !answers.is_empty() {
                        let segments = answers
                            .iter()
                            .map(|(op, body)| {
                                debug!(opcode = format_args!("{op:#06x}"), "  ipc sent");
                                sess.segment(sess.actor_id, *op, body)
                            })
                            .collect();
                        replies.push(Frame::new(sess.kind, segments));
                    }
                }
                other => debug!(r#type = ?other, payload = %hex::encode(&segment.payload), "segment"),
            }
        }
    }

    /// The packets that answer one IPC request.
    async fn answer(&self, sess: &mut Session, op: u16, body: &[u8]) -> Vec<(u16, Bytes)> {
        match op {
            opcode::PING => vec![(opcode::PING, ipc::ping_reply(body))],
            opcode::UPDATE_POSITION => {
                if let Some(u) = ipc::PositionUpdate::parse(body) {
                    let p = &mut sess.position;
                    (p.x, p.y, p.z, p.rotation) =
                        (u.pos[0] as f64, u.pos[1] as f64, u.pos[2] as f64, u.rotation as f64);
                    sess.moved = true;
                    if let Some(entry) = &sess.entry {
                        let motion = Motion {
                            rotation_before_slip: u.rotation_before_slip,
                            flags: u.flags,
                            collision: u.collision,
                        };
                        entry.zone.moved(sess.actor_id, u.pos, u.rotation, motion);
                    }
                    if sess.last_save.elapsed() >= POSITION_SAVE_INTERVAL {
                        self.save_position(sess).await;
                    }
                    if sess.entry.is_some() {
                        return self.objects_moved(sess);
                    }
                }
                vec![]
            }
            opcode::ZONE_LINE => {
                let Some(exit) = body.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap())) else {
                    return vec![];
                };
                let here = sess.position.zone_id as u16;
                let Some(data) = &self.data else {
                    warn!(exit, "zone exit, but no game data is configured");
                    return vec![];
                };
                match data.zone_line(here, exit) {
                    Ok(to) => {
                        info!(
                            exit,
                            from = here,
                            to = to.territory,
                            x = to.pos[0],
                            y = to.pos[1],
                            z = to.pos[2],
                            "  zone line"
                        );
                        self.move_to(sess, to.territory, to.pos, to.rotation).await
                    }
                    Err(e) => {
                        warn!(exit, territory = here, "zone line: {e}");
                        vec![]
                    }
                }
            }
            opcode::CHAT => {
                let Some(msg) = ipc::ChatUp::parse(body) else { return vec![] };
                info!(kind = msg.kind, text = %msg.text, "  chat");
                if let Some((name, args)) = commands::parse(&msg.text) {
                    // The level is read again for each command, so a demotion from the console
                    // holds at once. Players' text that looks like a command is chat.
                    if let Some(chara) = &sess.chara {
                        match self.db.login.gm_level(chara.account_id).await {
                            Ok(level) => sess.gm_level = level,
                            Err(e) => warn!("gm level: {e}"),
                        }
                    }
                    if sess.gm_level > 0 {
                        return self.run_command(sess, name, args).await;
                    }
                }
                let range = match msg.kind {
                    chat::SAY => ChatRange::Say,
                    chat::SHOUT | chat::YELL => ChatRange::Zone,
                    other => {
                        debug!(kind = other, "  chat type not relayed yet");
                        return vec![];
                    }
                };
                if let Some(entry) = &sess.entry {
                    entry.zone.chat(sess.actor_id, range, msg.kind, &msg.text);
                }
                vec![]
            }
            opcode::TALK_EVENT => self.talk_event(sess, body).await,
            opcode::EVENT_REQUEST => self.event_request(sess, body),
            opcode::RANGE_EVENT => self.range_event(sess, body, ipc::event::WITHIN_RANGE),
            opcode::OUTSIDE_RANGE_EVENT => self.range_event(sess, body, ipc::event::OUTSIDE_RANGE),
            opcode::SCENE_RESULT | opcode::SCENE_RESULT_4 | opcode::SCENE_RESULT_8 => {
                self.scene_result(sess, body).await
            }
            opcode::ITEM_OPERATION_REQUEST => self.item_operation(sess, body).await,
            opcode::COMMAND => {
                self.trigger(sess, body);
                vec![]
            }
            opcode::ACTION_REQUEST => {
                self.action_request(sess, body);
                vec![]
            }
            opcode::DISCOVERY => self.discover(sess, body).await,
            opcode::LINKSHELL_LIST => {
                debug!(opcode = format_args!("{op:#06x}"), body = %hex::encode(body), "  not acted on yet");
                vec![]
            }
            opcode::LOGOUT => {
                info!("  logout");
                self.save_position(sess).await;
                vec![(opcode::LOGOUT_REPLY, Bytes::from_static(&[0; 8]))]
            }
            opcode::INIT => {
                let pos = sess.position;
                info!(zone = pos.zone_id, x = pos.x, y = pos.y, z = pos.z, "  init: sending PlayerStatus, InitZone");
                let level = sess.look.level as u16;
                vec![
                    (opcode::PLAYER_STATUS, self.player_status(sess)),
                    (
                        opcode::CLASS_INFO,
                        ipc::class_info(sess.look.class, level, level, sess.progress[sess.class_slot].exp),
                    ),
                    (opcode::BASE_PARAM, starting_params(&sess.look)),
                ]
                .into_iter()
                .chain(inventory_packets(&sess.inventory, &mut sess.item_sequence))
                .chain([(opcode::INIT_ZONE, ipc::init_zone(pos.zone_id as u16, 1, sess.pos()))])
                // the quest lists (no completed ones kept yet): until both have come the client
                // leaves its quest-givers unshown (QUEST_COMPLETE_FLAGS)
                .chain([
                    (opcode::QUESTS, ipc::quests(&sess.quests)),
                    (opcode::QUEST_COMPLETE_FLAGS, ipc::quest_complete_flags(&sess.completed)),
                ])
                .collect()
            }
            opcode::FINISH_LOADING => {
                info!("  finished loading: spawning the player");
                let me = player_spawn(&sess.look, 0, sess.pos(), sess.position.rotation as f32);
                // In the territory now: its event NPCs and players appear as they come in range.
                if let Some(old) = sess.entry.take() {
                    self.world.leave(old).await;
                }
                let territory = sess.position.zone_id as u16;
                sess.entry = self
                    .world
                    .enter(territory, sess.actor_id, sess.look.clone(), sess.pos(), sess.position.rotation as f32)
                    .await;
                // An empty block list, for the client's request key 0 (it has made none yet):
                // without it the client drops everyone's chat as blocked.
                let mut out = vec![(opcode::SPAWN, me.to_bytes()), (opcode::BLACKLIST, ipc::blacklist(0, 0))];
                out.extend(self.objects_entered(sess));
                out
            }
            opcode::BLACKLIST => {
                let (index, key) = ipc::parse_blacklist_request(body).unwrap_or((0, 0));
                debug!(index, key, "  block list request");
                vec![(opcode::BLACKLIST, ipc::blacklist(index, key))]
            }
            other => {
                info!(opcode = format_args!("{other:#06x}"), len = body.len(), body = %hex::encode(body), "  unhandled ipc");
                vec![]
            }
        }
    }

    /// An item operation: done on the server's inventory, then answered (a batch of no steps,
    /// which releases the client's pending operation) and every slot it touched sent as it now
    /// is, so the client ends up with the server's state whatever it predicted. Only moves (8) are
    /// done so far; the others are answered and change nothing.
    async fn item_operation(&self, sess: &mut Session, body: &[u8]) -> Vec<(u16, Bytes)> {
        let Some(op) = ipc::ItemOperation::parse(body) else { return vec![] };
        let mut out = vec![(opcode::ITEM_OPERATION_BATCH, ipc::item_operation_batch(op.context, op.operation, 0, 0))];
        let (Ok(src_slot), Ok(dst_slot)) = (u16::try_from(op.src_slot), u16::try_from(op.dst_slot)) else {
            return out;
        };
        let (from, to) = ((op.src_container as u16, src_slot), (op.dst_container as u16, dst_slot));
        let touched = if op.operation == ipc::ItemOperation::MOVE {
            let stack = sess
                .inventory
                .get(from.0, from.1)
                .and_then(|i| self.data.as_deref().and_then(|d| d.item(i.item_id)))
                .map_or(1, |i| i.stack_size);
            match sess.inventory.move_item(from, to, stack) {
                Ok(changed) => {
                    info!(from = ?from, to = ?to, "  item moved");
                    self.save_inventory(sess).await;
                    changed
                }
                Err(e) => {
                    warn!(from = ?from, to = ?to, "  item move refused: {e:?}");
                    vec![from, to]
                }
            }
        } else {
            info!(operation = op.operation, from = ?from, "  item operation not done yet");
            vec![from]
        };
        for (c, s) in &touched {
            out.push((opcode::UPDATE_ITEM, slot_update(&sess.inventory, *c, *s)));
        }
        // gear changed: the look others get from now on
        if touched.iter().any(|(c, _)| *c == container::EQUIPPED) {
            sess.look = Arc::new(look_of(
                sess.chara.as_ref(),
                &sess.inventory,
                sess.progress[sess.class_slot].level,
                self.data.as_deref(),
            ));
            // the territory tells whoever sees the player, and the player's own client
            if let Some(entry) = &sess.entry {
                entry.zone.set_look(sess.actor_id, sess.look.clone());
            }
        }
        out
    }

    /// The character's level and experience per class slot; a slot never played is level 0 (the
    /// client's `not started`), and the current class starts at 1.
    async fn load_progress(&self, chara: &Character) -> [Progress; 19] {
        let mut progress = [Progress { level: 0, exp: 0 }; 19];
        match self.db.characters.class_progress(chara.id).await {
            Ok(rows) => {
                for (slot, level, exp) in rows {
                    if let Some(p) = progress.get_mut(slot as usize) {
                        *p = Progress { level, exp };
                    }
                }
            }
            Err(e) => warn!("class progress: {e}"),
        }
        let class = CharaMake::parse(&chara.appearance).map_or(1, |m| m.class);
        let slot = self.data.as_deref().and_then(|d| d.class_level_slot(class)).unwrap_or(0);
        if progress[slot].level == 0 {
            progress[slot] = Progress::default();
        }
        progress
    }

    /// Sets the current class's level and/or adds experience (GM commands now, battles later):
    /// saved, and the client told its class info and attributes; who sees the player sees the new
    /// level on its next spawn.
    async fn change_progress(&self, sess: &mut Session, level: Option<u8>, exp: Option<u32>) -> Vec<(u16, Bytes)> {
        let slot = sess.class_slot;
        if let Some(l) = level {
            sess.progress[slot].set_level(l);
        }
        if let Some(n) = exp {
            let data = self.data.as_deref();
            sess.progress[slot].gain(n, |l| data.and_then(|d| d.exp_to_next(l)));
        }
        let p = sess.progress[slot];
        info!(level = p.level, exp = p.exp, "  progress");
        if let Some(chara) = &sess.chara
            && let Err(e) = self.db.characters.set_class_progress(chara.id, slot as u8, p.level, p.exp).await
        {
            warn!("saving class progress: {e}");
        }
        sess.look = Arc::new(look_of(sess.chara.as_ref(), &sess.inventory, p.level, self.data.as_deref()));
        if let Some(entry) = &sess.entry {
            entry.zone.set_look(sess.actor_id, sess.look.clone());
        }
        vec![
            (opcode::CLASS_INFO, ipc::class_info(sess.look.class, p.level as u16, p.level as u16, p.exp)),
            (opcode::BASE_PARAM, starting_params(&sess.look)),
        ]
    }

    /// Puts items in the player's bags (stacking by the Item row's stack size): saved, every
    /// changed slot sent, a line in the chat log either way.
    async fn give_item(&self, sess: &mut Session, item_id: u32, quantity: u32) -> Vec<(u16, Bytes)> {
        let row = self.data.as_deref().map(|d| d.item(item_id));
        let (name, stack) = match row {
            Some(None) => return vec![(opcode::CHAT_DOWN, system_message(&format!("No item {item_id}.")))],
            Some(Some(i)) => (i.name, i.stack_size),
            None => (format!("item {item_id}"), 1),
        };
        let changed = match sess.inventory.add_to_bags(item_id, quantity, false, stack) {
            Ok(c) => c,
            Err(e) => {
                return vec![(opcode::CHAT_DOWN, system_message(&format!("No room for {quantity} {name} ({e:?}).")))];
            }
        };
        info!(item_id, quantity, "  item given");
        self.save_inventory(sess).await;
        let mut out: Vec<(u16, Bytes)> =
            changed.iter().map(|(c, s)| (opcode::UPDATE_ITEM, slot_update(&sess.inventory, *c, *s))).collect();
        out.push((opcode::CHAT_DOWN, system_message(&format!("You obtain {quantity} {name}."))));
        out
    }

    /// A client command: targeting and auto-attack go to the territory; the rest is logged.
    fn trigger(&self, sess: &mut Session, body: &[u8]) {
        let Some((command, args)) = ipc::trigger::parse(body) else { return };
        let Some(entry) = &sess.entry else { return };
        match command {
            ipc::trigger::TARGET => {
                let target = (args[0] != ipc::trigger::NO_TARGET && args[0] != 0).then_some(args[0]);
                debug!(target = ?target.map(|t| format!("{t:#x}")), "  target");
                entry.zone.set_target(sess.actor_id, target);
                sess.target = target;
            }
            ipc::trigger::AUTO_ATTACK => {
                info!(on = args[0] != 0, target = format_args!("{:#x}", args[1]), "  auto-attack");
                if args[0] != 0 && args[1] != ipc::trigger::NO_TARGET {
                    entry.zone.set_target(sess.actor_id, Some(args[1]));
                    sess.target = Some(args[1]);
                }
                entry.zone.set_auto_attack(sess.actor_id, args[0] != 0);
            }
            ipc::trigger::REVIVE => {
                info!(choice = args[0], "  revive");
                entry.zone.revive(sess.actor_id);
            }
            other => debug!(command = other, args = ?args, "  command not acted on yet"),
        }
    }

    /// An action: read from the Action sheet, checked against the player's class and level, and
    /// handed to the territory (which checks range and recast, and answers with the result).
    fn action_request(&self, sess: &mut Session, body: &[u8]) {
        let Some(req) = ipc::ActionRequest::parse(body) else { return };
        let Some(entry) = &sess.entry else { return };
        let Some(row) = self.data.as_ref().and_then(|d| d.action(req.action)) else {
            debug!(action = req.action, "  action: not in the Action sheet");
            return;
        };
        info!(action = req.action, name = %row.name, target = format_args!("{:#x}", req.target), "  action");
        // a class's own (0: any), and learnt
        if (row.class > 0 && row.class as u8 != sess.look.class) || row.level > sess.look.level {
            debug!(class = row.class, level = row.level, "  action: not this class's yet");
            return;
        }
        // Sprint: a status for the TP the player has
        if req.action == SPRINT {
            entry.zone.sprint(sess.actor_id);
            return;
        }
        // (a friendly one with a potency heals: Cure; the others, buffs and the like, wait for
        // their statuses)
        let heal = !row.hostile && row.potency > 0;
        if !row.hostile && !heal {
            debug!(action = req.action, "  action: only hostile ones and heals are carried out yet");
            return;
        }
        let gcd = matches!(row.category, 2 | 3);
        let recast = match row.recast_100ms {
            0 if gcd => crystal_world::combat::GCD,
            r => Duration::from_millis(r as u64 * 100),
        };
        // -1 (or 0): the class's reach (Sapphire's rule: archer and bard 25, the rest melee),
        // by the action's class, else the player's
        let range = if row.range <= 0 {
            let class = if row.class > 0 { row.class as u8 } else { sess.look.class };
            if crystal_world::combat::is_ranged_class(class) {
                crystal_world::combat::RANGED_RANGE + 1.0
            } else {
                crystal_world::combat::MELEE_RANGE
            }
        } else {
            row.range as f32 + 1.0
        };
        let target = if req.target == 0 || req.target as u32 == ipc::trigger::NO_TARGET {
            sess.target.unwrap_or(0)
        } else {
            req.target as u32
        };
        let action = ActionUse {
            action: req.action as u16,
            sequence: req.sequence,
            potency: row.potency.max(100),
            range,
            recast,
            gcd,
            cast: Duration::from_millis(row.cast_100ms as u64 * 100),
            heal,
            // (Action col 27: 3 MP, a percent of the level's MP base; 5 TP as it is)
            mp_cost: match row.cost_type {
                3 => {
                    let base = self.data.as_ref().and_then(|d| d.mp_base(sess.look.level)).unwrap_or(0);
                    crystal_world::combat::mp_cost(row.cost as u32, base)
                }
                _ => 0,
            },
            tp_cost: if row.cost_type == 5 { row.cost as u32 } else { 0 },
        };
        entry.zone.use_action(sess.actor_id, target, action);
    }

    /// The character's items; a character with none (a new one) gets its class's starting
    /// items, worn.
    async fn load_inventory(&self, chara: &Character) -> Inventory {
        let rows = match self.db.characters.inventory(chara.id).await {
            Ok(r) => r,
            Err(e) => {
                warn!("inventory: {e}");
                return Inventory::default();
            }
        };
        let mut inv = Inventory::default();
        for r in &rows {
            let item = Item {
                item_id: r.item_id,
                quantity: r.quantity,
                hq: r.hq,
                condition: r.condition,
                spiritbond: r.spiritbond,
                stain: r.stain,
            };
            if inv.set(r.container, r.slot, item).is_err() {
                warn!(container = r.container, slot = r.slot, "inventory row in no slot the character has: left out");
            }
        }
        if rows.is_empty()
            && let Some(data) = self.data.as_deref()
        {
            let make = CharaMake::parse(&chara.appearance);
            let class = make.as_ref().map_or(1, |m| m.class);
            let (race, gender) = make.as_ref().map_or((1, 0), |m| (m.customize[0], m.customize[1]));
            for (slot, item_id) in data.starting_items(class, race, gender) {
                let _ = inv.set(container::EQUIPPED, slot, Item::new(item_id, 1));
            }
            info!(character = %chara.name, items = inv.all().count(), "starting items");
            self.write_inventory(chara.id, &inv).await;
        }
        inv
    }

    async fn save_inventory(&self, sess: &Session) {
        if let Some(chara) = &sess.chara {
            self.write_inventory(chara.id, &sess.inventory).await;
        }
    }

    async fn write_inventory(&self, character_id: i64, inv: &Inventory) {
        let rows: Vec<InventoryRow> = inv
            .all()
            .map(|(container, slot, i)| InventoryRow {
                container,
                slot,
                item_id: i.item_id,
                quantity: i.quantity,
                hq: i.hq,
                condition: i.condition,
                spiritbond: i.spiritbond,
                stain: i.stain,
            })
            .collect();
        if let Err(e) = self.db.characters.save_inventory(character_id, &rows).await {
            warn!("saving the inventory: {e}");
        }
    }

    /// The player's own setup: who it is and its class, level 1 in that class's slot until there
    /// are stats (Sapphire sends this before InitZone, `PlayerMgr::onMoveZone`).
    fn player_status(&self, sess: &Session) -> Bytes {
        let look = &sess.look;
        let mut levels = [0u16; 19];
        let mut exp = [0u32; 19];
        for (i, p) in sess.progress.iter().enumerate() {
            (levels[i], exp[i]) = (p.level as u16, p.exp);
        }
        // the class chosen when the character was made
        let first_class =
            sess.chara.as_ref().and_then(|c| CharaMake::parse(&c.appearance)).map_or(look.class, |m| m.class);
        ipc::PlayerStatus {
            content_id: look.content_id,
            actor_id: sess.actor_id,
            class: look.class,
            first_class,
            name: look.name.clone(),
            levels,
            exp,
            unlocks: ipc::id_bits(&sess.unlocks),
            aetherytes: ipc::id_bits(&sess.aetherytes),
            discovery: sess
                .discovery
                .iter()
                .filter_map(|&(map, part)| {
                    let (slot, sixteen) = self.data.as_deref()?.map_discovery_slot(map)?;
                    Some((slot, sixteen, part))
                })
                .collect(),
        }
        .to_bytes()
    }

    /// The player talked to something with an event: start it and end it at once, so the client
    /// is never left waiting (event scripts come with Phase 6), and for a warp (handler type 2)
    /// move the player where its `Warp` row leads: the client asked the question already.
    async fn talk_event(&self, sess: &mut Session, body: &[u8]) -> Vec<(u16, Bytes)> {
        let Some((target, event_id)) = ipc::parse_talk_event(body) else { return vec![] };
        let handler = (event_id >> 16) as u16;
        info!(target = format_args!("{target:#x}"), event = format_args!("{event_id:#x}"), handler, "  talk event");
        let mut out = start_event(sess, target, event_id, ipc::event::TALK, 0);
        if handler == ipc::event::QUEST
            && let Some((scene, then)) = self.quest_talk(sess, event_id, target)
        {
            out.extend(self.quest_talk_actions(sess, &then).await);
            out.extend(play_scene(sess, scene, &[]));
            return out;
        }
        if handler == ipc::event::AETHERYTE {
            out.extend(self.aetheryte_talk(sess, event_id).await);
            return out;
        }
        // nothing to play: ended at once (the end clears what the talk set)
        out.extend(finish_event(sess));
        if handler == ipc::event::WARP {
            match self.data.as_deref().and_then(|d| d.warp(event_id)) {
                Some(to) => {
                    info!(to = to.territory, x = to.pos[0], y = to.pos[1], z = to.pos[2], "  warp");
                    out.extend(self.move_to(sess, to.territory, to.pos, to.rotation).await);
                }
                None => warn!(event = format_args!("{event_id:#x}"), "  warp: no Warp/Level row"),
            }
        }
        out
    }

    /// The client asks for an event it found on entering its territory (an opening's, so far).
    /// The start must match what the client holds pending: its own id (`GameObject::vf0`
    /// @0xa5f080: the actor id, high half 0), the event, kind 0xf and the territory. An opening
    /// then plays scene 0 (the cutscenes) for a character that has not seen them, else scene 40
    /// (the scripts' re-entry: only `onLogin`). Both scripts hand their first parameter to
    /// `onLogin` as the opening's sequence (`OPENING_SEQ_0`-`2` = 0-2, registered @0x6efc70).
    fn event_request(&self, sess: &mut Session, body: &[u8]) -> Vec<(u16, Bytes)> {
        let Some(event_id) = ipc::parse_event_request(body) else { return vec![] };
        let territory = sess.position.zone_id;
        let opening = sess.chara.as_ref().map_or(0, |c| c.opening);
        info!(event = format_args!("{event_id:#x}"), territory, opening, "  event request");
        let mut out = start_event(sess, sess.actor_id as u64, event_id, ipc::event::ENTER_TERRITORY, territory);
        if (event_id >> 16) as u16 == ipc::event::OPENING {
            let scene = if opening == 0 { 0 } else { 40 };
            out.extend(play_scene(sess, scene, &[opening as u32]));
        }
        out
    }

    /// The player walked into an event range of an opening. Its script enables, while the first
    /// quest is not accepted (`OPENING_SEQ_1`), the `ERANGE_SEQ_1_CLOSED_*` ranges (the way out,
    /// before talking to the quest's NPC), and always the `ERANGE_ALWAYS_CLOSED_*` ones and two
    /// tutorial ranges it handles itself (`OnPreWithinRange` returns false for those). Scene 10
    /// answers an always-closed range (its first parameter picks the NPC and the way back), scene
    /// 20 a seq-1 one (the quest NPC calls the player back). The client names a range by its
    /// object's +0x14, the script's id for it (the Gridania opening's six registered ranges, read
    /// in the client, carry exactly the `Opening` sheet's ids).
    /// `kind`: entering a range (10, Gridania's way out) or leaving one (11, Limsa's and Ul'dah's:
    /// their scripts enable it as `EVENT_OUTSIDE_RANGE`); the same scenes answer either.
    fn range_event(&self, sess: &mut Session, body: &[u8], kind: u8) -> Vec<(u16, Bytes)> {
        let Some((range, event_id, pos)) = ipc::parse_range_event(body) else { return vec![] };
        info!(event = format_args!("{event_id:#x}"), range, kind, ?pos, "  range event");
        let mut out = start_event(sess, sess.actor_id as u64, event_id, kind, range);
        let is = |name: &str| self.data.as_deref().and_then(|d| d.opening_value(event_id, name)) == Some(range);
        let scene = if (event_id >> 16) as u16 != ipc::event::OPENING {
            None
        } else if (1..=6).any(|i| is(&format!("ERANGE_ALWAYS_CLOSED_{i}"))) {
            Some((10, vec![range]))
        } else if (1..=2).any(|i| is(&format!("ERANGE_SEQ_1_CLOSED_{i}"))) {
            Some((20, vec![]))
        } else {
            None
        };
        match scene {
            Some((scene, params)) => out.extend(play_scene(sess, scene, &params)),
            None => {
                warn!(event = format_args!("{event_id:#x}"), range, "  range event: no scene for it; ending it");
                out.extend(finish_event(sess));
            }
        }
        out
    }

    /// A scene ended. An opening's scene 0 (its cutscenes) is followed by scene 1, the welcome:
    /// the scripts' system lines 20-22 ("Welcome to Gridania...", "Follow the instructions...",
    /// "When you are ready to proceed, speak to Bertennant."), the control mode question and the
    /// movement tutorial; after it (and after scene 40, the re-entry) the event ends, freeing the
    /// player. The cutscenes are recorded as seen (`OPENING_SEQ_1`) once scene 0 has ended, so
    /// they play once.
    async fn scene_result(&self, sess: &mut Session, body: &[u8]) -> Vec<(u16, Bytes)> {
        let Some(r) = ipc::parse_scene_result(body) else { return vec![] };
        info!(event = format_args!("{:#x}", r.event_id), scene = r.scene, values = ?r.values, byte_6 = r.byte_6, "  scene result");
        match (r.event_id >> 16) as u16 {
            ipc::event::OPENING => self.opening_scene_result(sess, &r).await,
            ipc::event::QUEST => self.quest_scene_result(sess, &r).await,
            ipc::event::AETHERYTE => self.aetheryte_scene_result(sess, &r).await,
            _ => vec![],
        }
    }

    /// An opening's scenes; see [`Self::event_request`] and [`Self::range_event`].
    async fn opening_scene_result(&self, sess: &mut Session, r: &ipc::SceneResult) -> Vec<(u16, Bytes)> {
        match r.scene {
            0 => {
                if let Some(chara) = sess.chara.as_mut()
                    && chara.opening == 0
                {
                    chara.opening = 1;
                    if let Err(e) = self.db.characters.save_opening(chara.content_id, 1).await {
                        warn!(character = %chara.name, "opening not saved: {e}");
                    }
                }
                play_scene(sess, 1, &[])
            }
            // the welcome, the re-entry, an always-closed or a seq-1 range's scene
            1 | 40 | 10 | 20 => finish_event(sess),
            // onQuestAccepted, nested in the quest's talk: ends both
            30 => {
                let mut out = finish_event(sess);
                out.extend(finish_event(sess));
                out
            }
            _ => vec![],
        }
    }

    /// The player walked into a map range: the map part it reveals is kept and told to the client
    /// (which waits for the answer before it asks about another range, so every request is
    /// answered, again or not).
    async fn discover(&self, sess: &mut Session, body: &[u8]) -> Vec<(u16, Bytes)> {
        let Some(range) = ipc::parse_discovery(body) else { return vec![] };
        let territory = sess.position.zone_id as u16;
        let Some((map, part)) = self.data.as_deref().and_then(|d| d.map_discovery(territory, range)) else {
            debug!(range, territory, "  discovery: not a map range that reveals anything");
            return vec![];
        };
        if !sess.discovery.contains(&(map, part)) {
            info!(map, part, range, "  map part revealed");
            sess.discovery.push((map, part));
            if let Some(chara) = &sess.chara
                && let Err(e) = self.db.characters.add_discovery(chara.id, map, part).await
            {
                warn!(character = %chara.name, "discovery not saved: {e}");
            }
        }
        vec![(opcode::DISCOVERY_REPLY, ipc::discovery_reply(map, part))]
    }

    /// Takes the player out of its territory and has its client load `territory` at `pos`; it
    /// enters the new one when it has loaded (FinishLoading).
    async fn move_to(&self, sess: &mut Session, territory: u16, pos: [f32; 3], rotation: f32) -> Vec<(u16, Bytes)> {
        if let Some(entry) = sess.entry.take() {
            self.world.leave(entry).await;
        }
        sess.position = Position {
            zone_id: territory as u32,
            x: pos[0] as f64,
            y: pos[1] as f64,
            z: pos[2] as f64,
            rotation: rotation as f64,
        };
        sess.moved = true;
        self.save_position(sess).await;
        vec![(opcode::INIT_ZONE, ipc::init_zone(territory, 1, pos))]
    }

    async fn run_command(&self, sess: &mut Session, name: &str, args: &str) -> Vec<(u16, Bytes)> {
        let player = commands::Player {
            actor: sess.actor_id,
            name: sess.look.name.clone(),
            territory: sess.position.zone_id as u16,
            pos: sess.pos(),
            rotation: sess.position.rotation as f32,
            gm_level: sess.gm_level,
            gm_mode: sess.gm_mode,
        };
        let mut cx = commands::Context::new(player, &self.world, &self.db, self.data.as_deref(), &self.commands);
        match self.commands.find(name) {
            Some(command) if command.level() > sess.gm_level => cx.reply(format!(
                ".{} needs GM level {} (yours is {}).",
                command.name(),
                command.level(),
                sess.gm_level
            )),
            Some(command) if !sess.gm_mode && !commands::ALWAYS.contains(&command.name()) => {
                cx.reply("GM mode is off: .gm on to use commands.")
            }
            Some(command) => {
                info!(command = name, args, gm_level = sess.gm_level, "  command");
                command.run(&mut cx, args).await;
            }
            None => cx.reply(format!("No command .{name}. .help lists them.")),
        }
        if let Some(on) = cx.gm_mode {
            info!(on, "  gm mode");
            sess.gm_mode = on;
        }
        let mut out: Vec<(u16, Bytes)> =
            cx.replies.iter().map(|line| (opcode::CHAT_DOWN, system_message(line))).collect();
        if cx.level.is_some() || cx.exp.is_some() {
            out.extend(self.change_progress(sess, cx.level, cx.exp).await);
        }
        for (item_id, quantity) in cx.items {
            out.extend(self.give_item(sess, item_id, quantity).await);
        }
        if let Some(t) = cx.teleport {
            out.extend(self.move_to(sess, t.territory, t.pos, t.rotation).await);
        }
        out
    }

    /// Writes the session's position to its character, if it moved.
    async fn save_position(&self, sess: &mut Session) {
        let Some(chara) = &sess.chara else { return };
        if !sess.moved {
            return;
        }
        match self.db.characters.save_position(chara.content_id, sess.position).await {
            Ok(()) => {
                let p = sess.position;
                debug!(zone = p.zone_id, x = p.x, y = p.y, z = p.z, "position saved");
                sess.moved = false;
                sess.last_save = Instant::now();
            }
            Err(e) => warn!("saving the position: {e}"),
        }
    }

    /// The character an actor id belongs to (`PLAYER_ACTOR_BASE` + its row id).
    async fn character_of(&self, actor_id: u32) -> Option<Character> {
        let row_id = actor_id.checked_sub(PLAYER_ACTOR_BASE)? as i64;
        match self.db.characters.character_by_id(row_id).await {
            Ok(c) => c,
            Err(e) => {
                warn!("character {row_id}: {e}");
                None
            }
        }
    }

    async fn open_capture(&self, peer: SocketAddr) -> Option<tokio::fs::File> {
        let dir = self.capture_dir.as_ref()?;
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let path = dir.join(format!("zone-{ms}-{}.bin", peer.port()));
        let opened = async {
            tokio::fs::create_dir_all(dir).await?;
            tokio::fs::File::create(&path).await
        };
        match opened.await {
            Ok(file) => Some(file),
            Err(e) => {
                warn!(path = %path.display(), "capture: {e}");
                None
            }
        }
    }
}

/// Starts an event the client asked for, as it holds it pending (@0x6b9110): the target (the
/// player's own id for events it asked for itself, `GameObject::GetObjectId` @0xa5f080: the actor
/// id, high half 0; the talked-to object's id as the client sent it for talk), the event, its kind
/// and the argument; flags 0 (the client only stores them, @0x6f0be0 → +0x250). Kept on the
/// session, so scenes play on the same target and [`finish_event`] repeats it.
fn start_event(sess: &mut Session, target: u64, event_id: u32, kind: u8, arg: u32) -> Vec<(u16, Bytes)> {
    sess.events.push(RunningEvent { target, event_id, kind, arg });
    vec![(opcode::EVENT_START, ipc::event_start(target, event_id, kind, 0, arg))]
}

/// Plays a scene of the session's innermost event on its target.
fn play_scene(sess: &Session, scene: u16, params: &[u32]) -> Vec<(u16, Bytes)> {
    let Some(e) = sess.events.last() else { return vec![] };
    vec![(opcode::EVENT_PLAY, ipc::event_play(e.target, e.event_id, scene, 0, params))]
}

/// Ends the session's innermost event with what started it (the client finds the running event
/// by event, kind and argument and, with no match, writes through a null entry: @0x6fd010, an
/// access violation at 0x6fd1bb). When it was the last, sends no conditions: asking for an event
/// set one (0x10 after the Gridania opening) that only the server clears (@0xa4f150).
fn finish_event(sess: &mut Session) -> Vec<(u16, Bytes)> {
    let Some(RunningEvent { event_id, kind, arg, .. }) = sess.events.pop() else { return vec![] };
    let mut out = vec![(opcode::EVENT_FINISH, ipc::event_finish(event_id, kind, 1, arg))];
    if sess.events.is_empty() {
        out.push((opcode::CONDITIONS, ipc::conditions(&[])));
    }
    out
}

/// What others see of a stored character: its CharaMake text gives the appearance and class,
/// the class its starting gear (`CharaMakeClassEquip`) until there is an inventory; level 1
/// until stats exist.
fn look_of(chara: Option<&Character>, inventory: &Inventory, level: u8, data: Option<&WorldData>) -> PlayerLook {
    let make = chara.and_then(|c| CharaMake::parse(&c.appearance));
    let class = make.as_ref().map_or(1, |m| m.class);
    // what it wears, from its equipped items; the class's starting look when it has none
    let equipped: Vec<(u16, u32)> = inventory.container(container::EQUIPPED).map(|(s, i)| (s, i.item_id)).collect();
    let (main_weapon, sub_weapon, models) = match data {
        Some(d) if !equipped.is_empty() => d.equipment_look(&equipped),
        Some(d) => d.starting_gear(class).unwrap_or_default(),
        None => Default::default(),
    };
    // base stats from the class, the tribe (customize[4]) and the level
    let tribe = make.as_ref().map_or(1, |m| m.customize[4]);
    let stats = data.and_then(|d| d.base_stats(class, tribe, level)).unwrap_or(crystal_data::Stats {
        strength: 20,
        dexterity: 20,
        vitality: 20,
        intelligence: 20,
        mind: 20,
        piety: 20,
        hp: 100,
        mp: 100,
    });
    PlayerLook {
        name: chara.map_or_else(|| "Crystal".into(), |c| c.name.clone()),
        content_id: chara.map_or(0, |c| c.content_id),
        class,
        level,
        customize: make.as_ref().map_or([0; 26], |m| m.customize),
        main_weapon,
        sub_weapon,
        models,
        hp_max: stats.hp,
        mp_max: stats.mp,
        attributes: [stats.strength, stats.dexterity, stats.vitality, stats.intelligence, stats.mind, stats.piety],
    }
}

/// An UPDATE_ITEM body for a slot as it now is (an empty slot: item 0).
fn slot_update(inv: &Inventory, container: u16, slot: u16) -> Bytes {
    let item = inv.get(container, slot).copied().unwrap_or_default();
    ipc::ItemInfo {
        container,
        slot,
        quantity: item.quantity,
        item_id: item.item_id,
        hq: item.hq,
        condition: item.condition,
        spiritbond: item.spiritbond,
        stain: item.stain,
        ..Default::default()
    }
    .to_bytes()
}

/// Every container that holds something, as the client takes them: each item (currency and
/// crystals as GIL_ITEM, the rest NORMAL_ITEM) under one sequence, then ITEM_SIZE with the count.
fn inventory_packets(inv: &Inventory, sequence: &mut u32) -> Vec<(u16, Bytes)> {
    let mut out = Vec::new();
    for (c, _) in container::ALL {
        let items: Vec<(u16, Item)> = inv.container(c).map(|(s, i)| (s, *i)).collect();
        if items.is_empty() {
            continue;
        }
        let seq = *sequence;
        *sequence += 1;
        let op =
            if c == container::CURRENCY || c == container::CRYSTAL { opcode::GIL_ITEM } else { opcode::NORMAL_ITEM };
        for (slot, i) in &items {
            let info = ipc::ItemInfo {
                sequence: seq,
                container: c,
                slot: *slot,
                quantity: i.quantity,
                item_id: i.item_id,
                hq: i.hq,
                condition: i.condition,
                spiritbond: i.spiritbond,
                stain: i.stain,
                ..Default::default()
            };
            out.push((op, info.to_bytes()));
        }
        out.push((opcode::ITEM_SIZE, ipc::container_info(seq, items.len() as u32, c)));
    }
    out
}

/// The player's attributes (BaseParam slots 0-5 are strength, dexterity, vitality, intelligence,
/// mind, piety, as `PlayerLook::attributes`), its maximum HP and MP, full TP.
fn starting_params(look: &PlayerLook) -> Bytes {
    let mut v = [0u32; 40];
    v[ipc::param::STRENGTH..=ipc::param::PIETY].copy_from_slice(&look.attributes);
    (v[ipc::param::HP], v[ipc::param::MP], v[ipc::param::TP]) = (look.hp_max, look.mp_max, 1000);
    ipc::base_params(&v, &[0; 6])
}

/// A player's spawn in `slot` (0: the client's own); placeholder points until stats exist.
fn player_spawn(look: &PlayerLook, slot: u8, pos: [f32; 3], rotation: f32) -> ipc::Spawn {
    ipc::Spawn {
        index: slot,
        status: 1,
        kind: ipc::kind::PLAYER,
        rotation,
        level: look.level,
        class: look.class,
        hp: look.hp_max,
        hp_max: look.hp_max,
        mp: look.mp_max as u16,
        mp_max: look.mp_max as u16,
        name: look.name.clone(),
        customize: look.customize,
        main_weapon: look.main_weapon,
        sub_weapon: look.sub_weapon,
        models: look.models,
        pos,
        ..Default::default()
    }
}

fn actor_spawn(slot: u8, actor: &ActorView, data: Option<&WorldData>) -> ipc::Spawn {
    match &actor.kind {
        ActorKind::Player(look) => player_spawn(look, slot, actor.pos, actor.rotation),
        ActorKind::BattleNpc { base_id, name_id, level, aggressive } => {
            // the body is the spawn's (ModelChara, customize, gear), from the BNpcBase row
            let look = data.and_then(|d| d.bnpc_look(*base_id)).unwrap_or_default();
            ipc::Spawn {
                base_id: *base_id,
                model: look.model,
                customize: look.customize,
                main_weapon: look.main_weapon,
                sub_weapon: look.sub_weapon,
                models: look.models,
                name_id: *name_id,
                index: slot,
                status: 1,
                active_type: if *aggressive { ipc::active_type::AGGRESSIVE } else { ipc::active_type::PASSIVE },
                kind: ipc::kind::BATTLE_NPC,
                // an enemy when its battalion fights players (Battalion), else a friendly NPC
                sub_kind: if data.and_then(|d| d.bnpc_hostile(*base_id)).unwrap_or(true) { 5 } else { 0 },
                // its side (BNpcBase col 1): what makes the client treat it as an enemy
                battalion: data.and_then(|d| d.bnpc_battalion(*base_id)).unwrap_or(4),
                rotation: actor.rotation,
                pos: actor.pos,
                level: (*level).min(255) as u8,
                hp: 100,
                hp_max: 100,
                ..Default::default()
            }
        }
        ActorKind::EventNpc { enpc } => ipc::Spawn {
            base_id: *enpc,
            // ENpcBase and ENpcResident share row ids: the resident row names it
            name_id: *enpc,
            index: slot,
            status: 1,
            kind: ipc::kind::EVENT_NPC,
            rotation: actor.rotation,
            pos: actor.pos,
            hp: 1,
            hp_max: 1,
            ..Default::default()
        },
    }
}

/// A character's HP update with its statuses (all of them: the client replaces its slots), and
/// for the player's own character its status list too.
#[allow(clippy::too_many_arguments)]
fn hp_segments(
    sess: &Session,
    actor: crystal_world::ActorId,
    hp: u32,
    hp_max: u32,
    level: u16,
    class: u8,
    statuses: &[StatusSlot],
    mana: Mana,
) -> Vec<Segment> {
    let statuses: Vec<ipc::StatusEntry> = statuses
        .iter()
        .map(|s| ipc::StatusEntry { id: s.id, param: s.param, remaining: s.remaining, source: s.source })
        .collect();
    let own = actor == sess.actor_id;
    let w = |v: u32| v.min(u16::MAX as u32) as u16;
    let hud = ipc::hud_param(class, level.min(255) as u8, hp, hp_max, w(mana.mp), w(mana.mp_max), w(mana.tp), &statuses);
    let mut out = vec![sess.segment(actor, opcode::HUD_PARAM, &hud)];
    if own {
        out.push(sess.segment(actor, opcode::STATUS, &ipc::status_list(&statuses)));
    }
    out
}

/// A world event as the packet the client gets, sent as the actor it is about.
fn event_segment(sess: &Session, event: Event, data: Option<&WorldData>) -> Vec<Segment> {
    vec![match event {
        Event::Spawn { slot, actor } => {
            sess.segment(actor.id, opcode::SPAWN, &actor_spawn(slot, &actor, data).to_bytes())
        }
        Event::Despawn { slot, actor } => sess.segment(actor, opcode::DESPAWN, &ipc::despawn(slot, actor)),
        Event::Move { actor, pos, rotation, motion } => {
            let speed = if motion.flags & ipc::ActorMove::FLAG_WALKING != 0 {
                ipc::ActorMove::SPEED_WALK
            } else {
                ipc::ActorMove::SPEED_RUN
            };
            let body = ipc::ActorMove {
                rotation,
                rotation_before_slip: motion.rotation_before_slip,
                flags: motion.flags,
                collision: motion.collision,
                speed,
                pos,
            };
            sess.segment(actor, opcode::ACTOR_MOVE, &body.to_bytes())
        }
        // the hit, as the attacker's action (a damage effect; the request's sequence frees the
        // user's client), then the target's HP
        Event::Damage { source, target, action, sequence, amount, hp, hp_max, level, class, statuses, mana } => {
            let mut effects = [ipc::Effect::default(); 8];
            effects[0] = ipc::Effect::damage(amount.min(u16::MAX as u32) as u16);
            let hit = ipc::ActionResult { action, kind: 1, sequence, lock: 0.6, target, effects, ..Default::default() };
            let mut out = vec![sess.segment(source, opcode::ACTION_RESULT1, &hit.to_bytes())];
            out.extend(hp_segments(sess, target, hp, hp_max, level, class, &statuses, mana));
            return out;
        }
        // a heal, as the healer's action (an HP-gain effect; the request's sequence frees the
        // user's client), then the target's HP
        Event::Heal { source, target, action, sequence, amount, hp, hp_max, level, class, statuses, mana } => {
            let mut effects = [ipc::Effect::default(); 8];
            effects[0] = ipc::Effect::heal(amount.min(u16::MAX as u32) as u16);
            let result = ipc::ActionResult { action, kind: 1, sequence, lock: 0.6, target, effects, ..Default::default() };
            let mut out = vec![sess.segment(source, opcode::ACTION_RESULT1, &result.to_bytes())];
            out.extend(hp_segments(sess, target, hp, hp_max, level, class, &statuses, mana));
            return out;
        }
        // a cast's start, sent as the caster: its cast bar
        Event::Cast { actor, action, target, time } => {
            let body = ipc::RequestCast { action, kind: 1, cast_time: time.as_secs_f32(), target, rotation: 0.0 };
            sess.segment(actor, opcode::REQUEST_CAST, &body.to_bytes())
        }
        // a cast's end before it landed, sent as the caster: its cast bar stops and the log says why
        Event::CastEnd { actor, action, interrupted } => {
            let log = if interrupted { ipc::log_message::CAST_INTERRUPTED } else { ipc::log_message::CAST_CANCELLED };
            let args = [log, 1, action as u32, interrupted as u32];
            sess.segment(actor, opcode::ORDER, &ipc::order(ipc::order_kind::CAST_END, args))
        }
        // regeneration, revives, statuses on and off: the HP update alone
        Event::Hp { actor, hp, hp_max, level, class, statuses, mana } => {
            return hp_segments(sess, actor, hp, hp_max, level, class, &statuses, mana);
        }
        // sent as the dead, naming the killer
        Event::Death { actor, killer, .. } => {
            let args = [killer.unwrap_or(ipc::trigger::NO_TARGET), 0, 0, 0];
            sess.segment(actor, opcode::ORDER, &ipc::order(ipc::order_kind::DEATH, args))
        }
        Event::Look { actor, look } => {
            sess.segment(actor, opcode::EQUIP, &ipc::equip(look.main_weapon, look.sub_weapon, &look.models))
        }
        Event::Chat { actor, content_id, name, kind, text } => {
            let body = ipc::ChatDown { kind, actor, content_id, name: name.to_string(), text: text.to_string() };
            sess.segment(actor, opcode::CHAT_DOWN, &body.to_bytes())
        }
    }]
}

/// A line from the server in the chat log.
fn system_message(text: &str) -> Bytes {
    ipc::ChatDown { kind: chat::SYSTEM, text: text.into(), ..Default::default() }.to_bytes()
}
