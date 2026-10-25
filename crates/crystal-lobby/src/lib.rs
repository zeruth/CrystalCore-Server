//! The lobby service. Phase 1: answer the client's encryption init, so its connection is up, and
//! log (decrypted) what it sends after. Frames received are captured for replay.

pub mod crypt;
pub mod ipc;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::{BufMut, Bytes, BytesMut};
use crystal_db::Databases;
use crystal_net::codec::{FrameCodec, RawFrameCodec, decode_frame};
use crystal_net::{ConnectionType, Frame, Segment, SegmentType};
use crystal_state::State;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::crypt::{Blowfish, KEY_PHRASE_LEN, lobby_key};
use crate::ipc::{CharaEntry, IpcHeader, ServiceAccount, World, opcode};

/// The world characters are played on.
pub const CRYSTAL_WORLD: u16 = 1;
/// The world characters are made on; nothing is played there.
pub const LOBBY_WORLD: u16 = 2;

/// What one of the client's two lobby hosts is for. The client binds character creation to the
/// first region (host 01, shown as JP) whose character list arrived (@0x8a2540), and its world
/// picker offers that region's worlds; so creation gets a region of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRole {
    /// One port for both hosts: every region shows Crystal, open for new characters.
    Both,
    /// The Lobby world, open for new characters, which the server places on Crystal; logging in
    /// is refused.
    Create,
    /// Crystal, to log in to; no new characters.
    Play,
}

impl HostRole {
    pub fn worlds(self) -> Vec<World> {
        use ipc::world_flags::OPEN;
        let world = |id, flags, name: &str| World { id, index: id, flags, name: name.into() };
        match self {
            HostRole::Both => vec![world(CRYSTAL_WORLD, OPEN, "Crystal")],
            HostRole::Create => vec![world(LOBBY_WORLD, OPEN, "Lobby")],
            HostRole::Play => vec![world(CRYSTAL_WORLD, 0, "Crystal")],
        }
    }
}

pub struct Lobby {
    #[allow(dead_code)] // Phase 1: accounts and characters
    db: Databases,
    #[allow(dead_code)] // Phase 1: sessions
    state: State,
    capture_dir: Option<PathBuf>,
    /// The id the next connection is given in its encryption reply (never 0: the client ignores a
    /// reply whose id is 0).
    next_connection_id: AtomicU32,
    /// Content ids for new characters (in memory until characters are stored).
    next_content_id: AtomicU64,
}

impl Lobby {
    pub fn new(db: Databases, state: State, capture_dir: Option<PathBuf>) -> Self {
        Self {
            db,
            state,
            capture_dir,
            next_connection_id: AtomicU32::new(1),
            next_content_id: AtomicU64::new(0x0040_0000_0000_0001),
        }
    }

    /// One lobby connection, from the lobby host `role` names (see `crystal_common::LobbyConfig`).
    pub async fn handle(
        self: Arc<Self>,
        stream: TcpStream,
        peer: SocketAddr,
        role: HostRole,
        shutdown: CancellationToken,
    ) {
        let local = stream.local_addr().map(|a| a.port()).unwrap_or(0);
        info!(local_port = local, ?role, "connected");
        let mut capture = self.open_capture(peer).await;
        let (reader, writer) = stream.into_split();
        let mut frames = FramedRead::new(reader, RawFrameCodec);
        let mut out = FramedWrite::new(writer, FrameCodec);
        let mut crypt: Option<Blowfish> = None;
        // The character being made on this connection (Phase 1 keeps it in memory only), and
        // whether the next appearance chunk continues the last.
        let mut making: Option<CharaEntry> = None;
        let mut appending = false;
        loop {
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
            log_frame(&frame);
            for segment in &frame.segments {
                match segment.header.segment_type {
                    SegmentType::EncryptionInit => {
                        let Some(init) = EncryptionInit::parse(&segment.payload) else {
                            warn!(len = segment.payload.len(), "short encryption init");
                            continue;
                        };
                        let bf = Blowfish::new(&lobby_key(init.key, &init.key_phrase));
                        let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed).max(1);
                        let reply = encryption_reply(&bf, id);
                        if let Err(e) = out.send(reply).await {
                            warn!("send: {e}");
                            break;
                        }
                        info!(connection_id = id, "encryption reply sent");
                        crypt = Some(bf);
                    }
                    SegmentType::Ipc => {
                        let Some(bf) = &crypt else {
                            warn!("ipc before the encryption init");
                            continue;
                        };
                        let mut payload = segment.payload.to_vec();
                        bf.decrypt_ipc(&mut payload);
                        info!(len = payload.len(), payload = %hex::encode(&payload), "  ipc (decrypted)");
                        let Some(header) = IpcHeader::parse(&payload) else {
                            warn!("short ipc");
                            continue;
                        };
                        let body = &payload[IpcHeader::SIZE..];
                        let seq = ipc::sequence(body);
                        let replies = match header.opcode {
                            opcode::CLIENT_VERSION => {
                                let accounts = [ServiceAccount { id: 1, index: 0, name: "FINAL FANTASY XIV".into() }];
                                vec![(opcode::SERVICE_ACCOUNT_LIST, ipc::service_account_list(seq, &accounts))]
                            }
                            opcode::SERVICE_LOGIN => vec![
                                (opcode::SERVER_LIST, ipc::server_list(seq, &role.worlds())),
                                (opcode::CHARA_LIST, ipc::empty_chara_list(seq)),
                            ],
                            opcode::GAME_LOGIN if role == HostRole::Create => {
                                info!("  game login refused: this host only makes characters");
                                vec![(opcode::ERROR, ipc::error(seq, 1001, ""))]
                            }
                            opcode::CHARA_MAKE => match ipc::CharaMakeRequest::parse(body) {
                                Some(req) => {
                                    info!(step = req.step, more = req.more, world = req.world_id, name = %req.name, "  chara make");
                                    let chara = making.get_or_insert_with(|| CharaEntry {
                                        content_id: self.next_content_id.fetch_add(1, Ordering::Relaxed),
                                        ..Default::default()
                                    });
                                    // only the first step names the world and the character
                                    if req.world_id != 0 {
                                        chara.world_id = req.world_id;
                                    }
                                    if !req.name.is_empty() {
                                        chara.name = req.name.clone();
                                    }
                                    // made on the Lobby world, played on Crystal
                                    if role == HostRole::Create && chara.world_id == LOBBY_WORLD {
                                        chara.world_id = CRYSTAL_WORLD;
                                    }
                                    if appending {
                                        chara.appearance.extend_from_slice(&req.appearance);
                                    } else {
                                        chara.appearance = req.appearance.clone();
                                    }
                                    appending = req.more;
                                    if req.more {
                                        vec![]
                                    } else {
                                        info!(appearance = %String::from_utf8_lossy(&chara.appearance), "  chara");
                                        vec![(opcode::CHARA_MAKE_REPLY, ipc::chara_make_reply(seq, req.step, chara))]
                                    }
                                }
                                None => {
                                    warn!("short chara make");
                                    vec![]
                                }
                            },
                            other => {
                                info!(opcode = format_args!("{other:#06x}"), "  unhandled ipc");
                                vec![]
                            }
                        };
                        for (op, body) in replies {
                            let frame = ipc_frame(bf, segment.header.target, op, &body);
                            if let Err(e) = out.send(frame).await {
                                warn!("send: {e}");
                                break;
                            }
                            info!(opcode = format_args!("{op:#06x}"), "  ipc sent");
                        }
                    }
                    // The client's keep-alive (0x18 bytes: an id and its time): echoed back as the
                    // response type.
                    SegmentType::KeepAlive => {
                        let s = &segment.header;
                        let pong =
                            Segment::new(SegmentType::KeepAliveResponse, s.target, s.source, segment.payload.clone());
                        if let Err(e) = out.send(Frame::new(ConnectionType::Lobby, vec![pong])).await {
                            warn!("send: {e}");
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(mut file) = capture {
            let _ = file.flush().await;
        }
        info!("closed");
    }

    /// One file per connection, the raw frames received back to back, named
    /// `lobby-<unix ms>-<peer port>.bin`.
    async fn open_capture(&self, peer: SocketAddr) -> Option<tokio::fs::File> {
        let dir = self.capture_dir.as_ref()?;
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let path = dir.join(format!("lobby-{ms}-{}.bin", peer.port()));
        let opened = async {
            tokio::fs::create_dir_all(dir).await?;
            tokio::fs::File::create(&path).await
        };
        match opened.await {
            Ok(file) => {
                info!(path = %path.display(), "capturing");
                Some(file)
            }
            Err(e) => {
                warn!(path = %path.display(), "capture: {e}");
                None
            }
        }
    }
}

/// The answer to an encryption init: a type-10 segment of 0x290 bytes whose 0x280-byte payload is
/// encrypted whole. The client decrypts it (`LobbyCryptEngine::ReadEncryptionReply` @0xe38140),
/// keeps the `u32` @0 as the connection's id (@0xe392e0, which ignores 0) and copies the 32 bytes
/// @0xc along, unused so far.
pub fn encryption_reply(bf: &Blowfish, connection_id: u32) -> Frame {
    const PAYLOAD: usize = 0x290 - 0x10;
    let mut payload = BytesMut::with_capacity(PAYLOAD);
    payload.put_u32_le(connection_id);
    payload.resize(PAYLOAD, 0);
    bf.encrypt(&mut payload);
    Frame::new(ConnectionType::Lobby, vec![Segment::new(SegmentType::EncryptionReply, 0, 0, payload.freeze())])
}

/// An IPC segment to the client, its payload encrypted. `connection_id` is the id the encryption
/// reply gave the connection, which the client puts in its segments' source and target.
fn ipc_frame(bf: &Blowfish, connection_id: u32, op: u16, body: &[u8]) -> Frame {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32);
    let mut payload = ipc::packet(op, ts, body);
    bf.encrypt_ipc(&mut payload);
    let segment = Segment::new(SegmentType::Ipc, connection_id, connection_id, payload.freeze());
    Frame::new(ConnectionType::Lobby, vec![segment])
}

fn log_frame(frame: &Frame) {
    let h = &frame.header;
    info!(
        size = h.size,
        connection = ?h.connection_type,
        segments = h.segment_count,
        compressed = h.compressed,
        "frame"
    );
    for segment in &frame.segments {
        let s = &segment.header;
        info!(r#type = ?s.segment_type, size = s.size, source = s.source, target = s.target, "  segment");
        if s.segment_type == SegmentType::EncryptionInit
            && let Some(init) = EncryptionInit::parse(&segment.payload)
        {
            info!(key_phrase = %init.phrase(), key = format_args!("{:#010x}", init.key), "  encryption init");
        }
        debug!(payload = %hex::encode(&segment.payload), "  payload");
    }
}

/// The client's first lobby segment, written by `LobbyCryptEngine::WriteEncryptionInit` @0xe36880:
/// the key phrase (32 bytes, zero-padded) @0x24 and the key, the client's `time()` when it first
/// connected, @0x64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionInit {
    /// "Test Ticket Data" with `DEV.TestSID=0`.
    pub key_phrase: [u8; KEY_PHRASE_LEN],
    pub key: u32,
}

impl EncryptionInit {
    const PHRASE: usize = 0x24;
    const KEY: usize = 0x64;

    pub fn parse(payload: &Bytes) -> Option<Self> {
        let key_phrase = payload.get(Self::PHRASE..Self::PHRASE + KEY_PHRASE_LEN)?.try_into().ok()?;
        let key = u32::from_le_bytes(payload.get(Self::KEY..Self::KEY + 4)?.try_into().ok()?);
        Some(Self { key_phrase, key })
    }

    /// The key phrase as text, up to its first zero byte.
    pub fn phrase(&self) -> String {
        let end = self.key_phrase.iter().position(|&b| b == 0).unwrap_or(KEY_PHRASE_LEN);
        String::from_utf8_lossy(&self.key_phrase[..end]).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture_init() -> EncryptionInit {
        let capture = include_bytes!("../../../protocol/672261/captures/lobby-encryption-init.bin");
        let frame = decode_frame(Bytes::from_static(&capture[..664])).unwrap();
        EncryptionInit::parse(&frame.segments[0].payload).unwrap()
    }

    #[test]
    fn encryption_init_from_capture() {
        let init = capture_init();
        assert_eq!(init.phrase(), "Test Ticket Data");
        assert_eq!(init.key, 0x6ade_798e);
        assert!(init.key_phrase[16..].iter().all(|&b| b == 0));
        let capture = include_bytes!("../../../protocol/672261/captures/lobby-encryption-init.bin");
        let frame = decode_frame(Bytes::from_static(&capture[..664])).unwrap();
        assert_eq!(EncryptionInit::parse(&frame.segments[0].payload.slice(..0x60)), None);
    }

    #[test]
    fn encryption_reply_is_what_the_client_reads() {
        let init = capture_init();
        let bf = Blowfish::new(&lobby_key(init.key, &init.key_phrase));
        let frame = encryption_reply(&bf, 0xe000_3c2a);
        let mut wire = BytesMut::new();
        tokio_util::codec::Encoder::encode(&mut FrameCodec, &frame, &mut wire).unwrap();
        let back = decode_frame(wire.freeze()).unwrap();
        let segment = &back.segments[0];
        assert_eq!(segment.header.segment_type, SegmentType::EncryptionReply);
        assert_eq!(Some(segment.header.size), SegmentType::EncryptionReply.fixed_size());
        let mut payload = segment.payload.to_vec();
        bf.decrypt(&mut payload);
        assert_eq!(payload[..4], 0xe000_3c2au32.to_le_bytes());
        assert!(payload[4..].iter().all(|&b| b == 0));
    }
}
