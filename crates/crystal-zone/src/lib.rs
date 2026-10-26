//! The zone service: a character in the world.
//!
//! The client opens two connections to the address game login gives it, a zone one (frame
//! connection type 1) and a chat one (2), each starting with a session init that names the actor id
//! game login gave. Zone traffic is not encrypted: the zone channel has no crypt engine
//! (`ZoneProtoChannel` has none of the lobby's `LobbyCryptEngine`).
//!
//! Zone IPC so far (ffxiv.exe 672261; the pre-4.0 opcodes Sapphire marks "unchanged for sb" agree):
//! - up 0x66 Init: the client is connected → down 0x19a InitZone, which starts loading the
//!   territory (`PacketDispatcher` @0xba51d0 → @0xab56a0).
//! - up 0x65 Ping (every 10 s; @0x00c2fb20) → down 0x65, the client's time @0 echoed.
//! - up 0x69 FinishLoading: the client sends it after InitZone (@0x00c2fb20).

pub mod ipc;

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
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::ipc::opcode;

/// Player actor ids are this plus the character's row id (as the lobby's game login gives them).
pub const PLAYER_ACTOR_BASE: u32 = 0x1000_0000;
/// Event NPC actor ids are this plus their character slot (per connection, until zones own actors).
pub const NPC_ACTOR_BASE: u32 = 0x4000_0000;
/// The client keeps 100 characters (@0xa64860); slot 0 is the player.
pub const MAX_SPAWNED_NPCS: usize = 99;

/// Where a character with no stored position stands: Limsa Lominsa's start (see
/// `crystal_common::chara::start_point`).
pub const DEFAULT_POSITION: Position = Position { zone_id: 129, x: -62.0, y: 18.0, z: -4.0, rotation: 0.2 };

/// The actor id in a session init's payload: decimal text @4, zero-terminated.
pub fn session_actor(payload: &[u8]) -> Option<u32> {
    let text = payload.get(4..)?;
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    std::str::from_utf8(&text[..end]).ok()?.parse().ok()
}

pub struct Zone {
    db: Databases,
    #[allow(dead_code)] // the lobby's session handoff
    state: State,
    /// The client's game data (zone exits); none when not configured.
    world: Option<Arc<WorldData>>,
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
    /// Where the player is now (from its position updates).
    position: Position,
    /// The position changed since it was last saved.
    moved: bool,
    last_save: Instant,
}

impl Session {
    fn new(actor_id: u32, kind: ConnectionType, chara: Option<Character>) -> Self {
        let position = chara.as_ref().and_then(|c| c.position).unwrap_or(DEFAULT_POSITION);
        Self { actor_id, kind, chara, position, moved: false, last_save: Instant::now() }
    }
}

impl Zone {
    pub fn new(db: Databases, state: State, world: Option<Arc<WorldData>>, capture_dir: Option<PathBuf>) -> Self {
        Self { db, state, world, capture_dir }
    }

    pub async fn handle(self: Arc<Self>, stream: TcpStream, peer: SocketAddr, shutdown: CancellationToken) {
        info!("connected");
        let mut capture = self.open_capture(peer).await;
        let (reader, writer) = stream.into_split();
        let mut frames = FramedRead::new(reader, RawFrameCodec);
        let mut out = FramedWrite::new(writer, FrameCodec);
        let mut session: Option<Session> = None;
        'frames: loop {
            let next = tokio::select! {
                _ = shutdown.cancelled() => break,
                next = frames.next() => next,
            };
            let raw = match next {
                None => break,
                Some(Ok(raw)) => raw,
                Some(Err(e)) => {
                    warn!("framing: {e}");
                    break;
                }
            };
            if let Some(file) = &mut capture
                && let Err(e) = file.write_all(&raw).await
            {
                warn!("capture: {e}");
                capture = None;
            }
            let frame = match decode_frame(raw.clone()) {
                Ok(frame) => frame,
                Err(e) => {
                    warn!(bytes = %hex::encode(&raw), "undecodable frame: {e}");
                    continue;
                }
            };
            let kind = frame.header.connection_type;
            let mut replies: Vec<Frame> = Vec::new();
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
                        info!(
                            actor_id = format_args!("{actor_id:#x}"),
                            connection = ?kind,
                            character = chara.as_ref().map(|c| c.name.as_str()).unwrap_or("?"),
                            "session init"
                        );
                        let mut payload = vec![0u8; 0x38 - 0x10];
                        payload[..4].copy_from_slice(&actor_id.to_le_bytes());
                        let reply = Segment::new(SegmentType::Unknown(2), actor_id, actor_id, payload.into());
                        replies.push(Frame::new(kind, vec![reply]));
                        session = Some(Session::new(actor_id, kind, chara));
                    }
                    SegmentType::KeepAlive => {
                        let pong =
                            Segment::new(SegmentType::KeepAliveResponse, s.target, s.source, segment.payload.clone());
                        replies.push(Frame::new(kind, vec![pong]));
                    }
                    SegmentType::Ipc => {
                        let Some(sess) = &mut session else {
                            warn!("ipc before the session init");
                            continue;
                        };
                        let Some(header) = IpcHeader::parse(&segment.payload) else {
                            warn!("short ipc");
                            continue;
                        };
                        let body = &segment.payload[IpcHeader::SIZE..];
                        for (op, body) in self.answer(sess, header.opcode, body).await {
                            let payload = packet(op, timestamp(), &body);
                            let seg = Segment::new(SegmentType::Ipc, sess.actor_id, sess.actor_id, payload.freeze());
                            replies.push(Frame::new(sess.kind, vec![seg]));
                            info!(opcode = format_args!("{op:#06x}"), "  ipc sent");
                        }
                        // Once the player stands in the territory, its event NPCs appear: each
                        // spawn is sent as the NPC's own actor id (the segment's source).
                        if header.opcode == opcode::FINISH_LOADING {
                            let npcs = self.npc_spawns(sess.position.zone_id as u16);
                            info!(count = npcs.len(), "  spawning event NPCs");
                            for (actor, spawn) in npcs {
                                let payload = packet(opcode::SPAWN, timestamp(), &spawn.to_bytes());
                                let seg = Segment::new(SegmentType::Ipc, actor, sess.actor_id, payload.freeze());
                                replies.push(Frame::new(sess.kind, vec![seg]));
                            }
                        }
                    }
                    other => debug!(r#type = ?other, payload = %hex::encode(&segment.payload), "segment"),
                }
            }
            for reply in replies {
                if let Err(e) = out.send(reply).await {
                    warn!("send: {e}");
                    break 'frames;
                }
            }
        }
        if let Some(mut file) = capture {
            let _ = file.flush().await;
        }
        if let Some(sess) = &mut session
            && sess.kind == ConnectionType::Zone
        {
            self.save_position(sess).await;
        }
        info!("closed");
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
                let Some(world) = &self.world else {
                    warn!(exit, "zone exit, but no game data is configured");
                    return vec![];
                };
                match world.zone_line(here, exit) {
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
                        sess.position = Position {
                            zone_id: to.territory as u32,
                            x: to.pos[0] as f64,
                            y: to.pos[1] as f64,
                            z: to.pos[2] as f64,
                            rotation: to.rotation as f64,
                        };
                        sess.moved = true;
                        self.save_position(sess).await;
                        vec![(opcode::INIT_ZONE, ipc::init_zone(to.territory, 1, to.pos))]
                    }
                    Err(e) => {
                        warn!(exit, territory = here, "zone line: {e}");
                        vec![]
                    }
                }
            }
            opcode::CHAT => {
                let kind = body.get(0x18..0x1a).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
                let text = body.get(0x1a..).unwrap_or_default();
                let text = &text[..text.iter().position(|&b| b == 0).unwrap_or(text.len())];
                info!(kind, text = %String::from_utf8_lossy(text), "  chat");
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
                vec![(
                    opcode::INIT_ZONE,
                    ipc::init_zone(pos.zone_id as u16, 1, [pos.x as f32, pos.y as f32, pos.z as f32]),
                )]
            }
            opcode::FINISH_LOADING => {
                info!("  finished loading: spawning the player");
                vec![(opcode::SPAWN, player_spawn(sess.chara.as_ref(), sess.position).to_bytes())]
            }
            other => {
                info!(opcode = format_args!("{other:#06x}"), len = body.len(), body = %hex::encode(body), "  unhandled ipc");
                vec![]
            }
        }
    }

    /// Spawns for the event NPCs the game data places in `territory` (up to the client's 99 free
    /// character slots), each with its actor id.
    fn npc_spawns(&self, territory: u16) -> Vec<(u32, ipc::Spawn)> {
        let Some(world) = &self.world else { return vec![] };
        let npcs = match world.npcs(territory) {
            Ok(n) => n,
            Err(e) => {
                warn!(territory, "event NPCs: {e}");
                return vec![];
            }
        };
        npcs.iter()
            .take(MAX_SPAWNED_NPCS)
            .enumerate()
            .map(|(i, n)| {
                let index = i as u8 + 1;
                let spawn = ipc::Spawn {
                    base_id: n.enpc,
                    index,
                    status: 1,
                    kind: ipc::kind::EVENT_NPC,
                    rotation: n.rotation,
                    pos: n.pos,
                    hp: 1,
                    hp_max: 1,
                    ..Default::default()
                };
                (NPC_ACTOR_BASE + index as u32, spawn)
            })
            .collect()
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

/// The player's own spawn, from the stored character (its CharaMake text gives the appearance and
/// class); level 1 and placeholder points until stats exist.
fn player_spawn(chara: Option<&Character>, pos: Position) -> ipc::Spawn {
    let make = chara.and_then(|c| CharaMake::parse(&c.appearance));
    ipc::Spawn {
        index: 0,
        status: 1,
        kind: 1,
        rotation: pos.rotation as f32,
        level: 1,
        class: make.as_ref().map_or(1, |m| m.class),
        hp: 100,
        hp_max: 100,
        mp: 100,
        mp_max: 100,
        name: chara.map_or_else(|| "Crystal".into(), |c| c.name.clone()),
        customize: make.as_ref().map_or([0; 26], |m| m.customize),
        pos: [pos.x as f32, pos.y as f32, pos.z as f32],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_actor_from_client() {
        // the client's session init payload, as captured on both connections
        let payload =
            hex::decode("00000000323638343335343537000000000000000000000000000000000000000000000000000000").unwrap();
        assert_eq!(session_actor(&payload), Some(0x1000_0001));
        assert_eq!(session_actor(&[0; 8]), None);
    }
}
