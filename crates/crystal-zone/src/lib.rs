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
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
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

/// Where a character with no stored position stands: Limsa Lominsa's opening territory (see the
/// lobby's `chara::start_point`).
pub const DEFAULT_POSITION: Position = Position { zone_id: 181, x: -53.0, y: 18.0, z: 0.0, rotation: 1.5 };

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
    capture_dir: Option<PathBuf>,
}

/// One connection's player, once its session init named it.
struct Session {
    actor_id: u32,
    kind: ConnectionType,
    chara: Option<Character>,
}

impl Zone {
    pub fn new(db: Databases, state: State, capture_dir: Option<PathBuf>) -> Self {
        Self { db, state, capture_dir }
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
                        session = Some(Session { actor_id, kind, chara });
                    }
                    SegmentType::KeepAlive => {
                        let pong =
                            Segment::new(SegmentType::KeepAliveResponse, s.target, s.source, segment.payload.clone());
                        replies.push(Frame::new(kind, vec![pong]));
                    }
                    SegmentType::Ipc => {
                        let Some(sess) = &session else {
                            warn!("ipc before the session init");
                            continue;
                        };
                        let Some(header) = IpcHeader::parse(&segment.payload) else {
                            warn!("short ipc");
                            continue;
                        };
                        let body = &segment.payload[IpcHeader::SIZE..];
                        for (op, body) in self.answer(sess, header.opcode, body) {
                            let payload = packet(op, timestamp(), &body);
                            let seg = Segment::new(SegmentType::Ipc, sess.actor_id, sess.actor_id, payload.freeze());
                            replies.push(Frame::new(sess.kind, vec![seg]));
                            info!(opcode = format_args!("{op:#06x}"), "  ipc sent");
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
        info!("closed");
    }

    /// The packets that answer one IPC request.
    fn answer(&self, sess: &Session, op: u16, body: &[u8]) -> Vec<(u16, Bytes)> {
        match op {
            opcode::PING => vec![(opcode::PING, ipc::ping_reply(body))],
            opcode::INIT => {
                let pos = sess.chara.as_ref().and_then(|c| c.position).unwrap_or(DEFAULT_POSITION);
                info!(zone = pos.zone_id, x = pos.x, y = pos.y, z = pos.z, "  init: sending InitZone");
                vec![(
                    opcode::INIT_ZONE,
                    ipc::init_zone(pos.zone_id as u16, 1, [pos.x as f32, pos.y as f32, pos.z as f32]),
                )]
            }
            opcode::FINISH_LOADING => {
                info!("  finished loading");
                vec![]
            }
            other => {
                info!(opcode = format_args!("{other:#06x}"), len = body.len(), body = %hex::encode(body), "  unhandled ipc");
                vec![]
            }
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
