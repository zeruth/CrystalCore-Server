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

pub mod commands;

pub use crystal_protocol::zone as ipc;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use crystal_common::chara::CharaMake;
use crystal_data::WorldData;
use crystal_db::{Character, Databases, Position};
use crystal_net::codec::{FrameCodec, RawFrameCodec, decode_frame};
use crystal_net::ipc::{IpcHeader, packet, timestamp};
use crystal_net::{ConnectionType, Frame, Segment, SegmentType};
use crystal_state::State;
use crystal_world::{ActorKind, ActorView, ChatRange, Entry, Event, Motion, PlayerLook, World};
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
/// `crystal_common::chara::start_point`).
pub const DEFAULT_POSITION: Position = Position { zone_id: 129, x: -62.0, y: 18.0, z: -4.0, rotation: 0.2 };

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
}

/// How often a moving player's position is written to the database (and once more when the zone
/// connection closes).
pub const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(10);

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
}

impl Session {
    fn new(actor_id: u32, kind: ConnectionType, chara: Option<Character>, gm_level: u8) -> Self {
        let position = chara.as_ref().and_then(|c| c.position).unwrap_or(DEFAULT_POSITION);
        let look = Arc::new(look_of(chara.as_ref()));
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
        Self { db, state, data, world, commands, capture_dir }
    }

    pub async fn handle(self: Arc<Self>, stream: TcpStream, peer: SocketAddr, shutdown: CancellationToken) {
        info!("connected");
        let mut capture = self.open_capture(peer).await;
        let (reader, writer) = stream.into_split();
        let mut frames = FramedRead::new(reader, RawFrameCodec);
        let mut out = FramedWrite::new(writer, FrameCodec);
        let mut session: Option<Session> = None;
        loop {
            let input = tokio::select! {
                _ = shutdown.cancelled() => break,
                next = frames.next() => Input::Frame(next),
                event = next_event(&mut session) => Input::Event(event),
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
                Input::Event(Some(first)) => {
                    let sess = session.as_mut().expect("events come to a session");
                    let mut segments = vec![event_segment(sess, first)];
                    while segments.len() < EVENTS_PER_FRAME
                        && let Some(Ok(e)) = sess.entry.as_mut().map(|e| e.events.try_recv())
                    {
                        segments.push(event_segment(sess, e));
                    }
                    replies.push(Frame::new(sess.kind, segments));
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
                    *session = Some(Session::new(actor_id, kind, chara, gm_level));
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
            opcode::LOGOUT => {
                info!("  logout");
                self.save_position(sess).await;
                vec![(opcode::LOGOUT_REPLY, Bytes::from_static(&[0; 8]))]
            }
            opcode::INIT => {
                let pos = sess.position;
                info!(zone = pos.zone_id, x = pos.x, y = pos.y, z = pos.z, "  init: sending InitZone");
                vec![(opcode::INIT_ZONE, ipc::init_zone(pos.zone_id as u16, 1, sess.pos()))]
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
                vec![(opcode::SPAWN, me.to_bytes()), (opcode::BLACKLIST, ipc::blacklist(0, 0))]
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
        let mut cx = commands::Context::new(player, &self.world, self.data.as_deref(), &self.commands);
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

/// What others see of a stored character: its CharaMake text gives the appearance and class;
/// level 1 until stats exist.
fn look_of(chara: Option<&Character>) -> PlayerLook {
    let make = chara.and_then(|c| CharaMake::parse(&c.appearance));
    PlayerLook {
        name: chara.map_or_else(|| "Crystal".into(), |c| c.name.clone()),
        content_id: chara.map_or(0, |c| c.content_id),
        class: make.as_ref().map_or(1, |m| m.class),
        level: 1,
        customize: make.as_ref().map_or([0; 26], |m| m.customize),
        ..Default::default()
    }
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
        hp: 100,
        hp_max: 100,
        mp: 100,
        mp_max: 100,
        name: look.name.clone(),
        customize: look.customize,
        main_weapon: look.main_weapon,
        sub_weapon: look.sub_weapon,
        models: look.models,
        pos,
        ..Default::default()
    }
}

fn actor_spawn(slot: u8, actor: &ActorView) -> ipc::Spawn {
    match &actor.kind {
        ActorKind::Player(look) => player_spawn(look, slot, actor.pos, actor.rotation),
        ActorKind::BattleNpc { base_id, name_id, level } => ipc::Spawn {
            base_id: *base_id,
            name_id: *name_id,
            index: slot,
            status: 1,
            kind: ipc::kind::BATTLE_NPC,
            rotation: actor.rotation,
            pos: actor.pos,
            level: (*level).min(255) as u8,
            hp: 100,
            hp_max: 100,
            ..Default::default()
        },
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

/// A world event as the packet the client gets, sent as the actor it is about.
fn event_segment(sess: &Session, event: Event) -> Segment {
    match event {
        Event::Spawn { slot, actor } => sess.segment(actor.id, opcode::SPAWN, &actor_spawn(slot, &actor).to_bytes()),
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
        Event::Chat { actor, content_id, name, kind, text } => {
            let body = ipc::ChatDown { kind, actor, content_id, name: name.to_string(), text: text.to_string() };
            sess.segment(actor, opcode::CHAT_DOWN, &body.to_bytes())
        }
    }
}

/// A line from the server in the chat log.
fn system_message(text: &str) -> Bytes {
    ipc::ChatDown { kind: chat::SYSTEM, text: text.into(), ..Default::default() }.to_bytes()
}
