//! The lobby service: the encryption handshake, then the client's lobby requests (version, service
//! login with the world and character lists, making a character, game login). Frames received are
//! captured for replay when configured.

pub use crystal_common::chara;
pub mod crypt;
pub mod ipc;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
use crystal_common::chara::{CharaMake, ClientSelectData, starting_zone};

/// The world characters are played on.
pub const CRYSTAL_WORLD: u16 = 1;
/// The world characters are made on; nothing is played there.
pub const LOBBY_WORLD: u16 = 2;
/// The account the client's test ticket (`DEV.TestSID=0`) logs in to, until accounts are real.
pub const TEST_ACCOUNT: &str = "test";
/// How long a game login's session waits for the client to reach the zone server.
pub const ZONE_SESSION_TTL: Duration = Duration::from_secs(120);
/// The first content id given (characters get the next above the highest stored).
const FIRST_CONTENT_ID: u64 = 0x0040_0000_0000_0001;
/// Player actor ids are this plus the character's row id.
pub const PLAYER_ACTOR_BASE: u32 = 0x1000_0000;

/// The shared-state key a zone server reads a game login's session from; the value is the
/// character's content id, in decimal.
pub fn zone_session_key(session: &str) -> String {
    format!("zone-session:{session}")
}

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

    /// Whether this host lists the stored characters (the creation host lists none).
    fn lists_characters(self) -> bool {
        self != HostRole::Create
    }
}

pub struct Lobby {
    db: Databases,
    state: State,
    /// Where game login sends the client: the zone server.
    zone_addr: SocketAddr,
    capture_dir: Option<PathBuf>,
    /// The id the next connection is given in its encryption reply (never 0: the client ignores a
    /// reply whose id is 0).
    next_connection_id: AtomicU32,
    /// The next content id to give; 0 until read from the database.
    next_content_id: AtomicU64,
}

/// What one connection knows between requests.
#[derive(Default)]
struct Conn {
    account: Option<i64>,
    /// The character being made: its content id (given in step 1), world, name, and the CharaMake
    /// text collected so far.
    making: Option<Making>,
}

#[derive(Default)]
struct Making {
    content_id: u64,
    world_id: u16,
    name: String,
    appearance: Vec<u8>,
    /// The next chunk of the appearance continues the last.
    appending: bool,
}

impl Lobby {
    pub fn new(db: Databases, state: State, zone_addr: SocketAddr, capture_dir: Option<PathBuf>) -> Self {
        Self {
            db,
            state,
            zone_addr,
            capture_dir,
            next_connection_id: AtomicU32::new(1),
            next_content_id: AtomicU64::new(0),
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
        let mut conn = Conn::default();
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
                        if let Err(e) = out.send(encryption_reply(&bf, id)).await {
                            warn!("send: {e}");
                            break 'frames;
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
                        debug!(len = payload.len(), payload = %hex::encode(&payload), "  ipc (decrypted)");
                        let Some(header) = IpcHeader::parse(&payload) else {
                            warn!("short ipc");
                            continue;
                        };
                        let replies = self.answer(&mut conn, role, header.opcode, &payload[IpcHeader::SIZE..]).await;
                        for (op, body) in replies {
                            let frame = ipc_frame(bf, segment.header.target, op, &body);
                            if let Err(e) = out.send(frame).await {
                                warn!("send: {e}");
                                break 'frames;
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
                            break 'frames;
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

    /// The packets that answer one request.
    async fn answer(&self, conn: &mut Conn, role: HostRole, op: u16, body: &[u8]) -> Vec<(u16, Bytes)> {
        let seq = ipc::sequence(body);
        let result = match op {
            opcode::CLIENT_VERSION => {
                let accounts = [ServiceAccount { id: 1, index: 0, name: "FINAL FANTASY XIV".into() }];
                Ok(vec![(opcode::SERVICE_ACCOUNT_LIST, ipc::service_account_list(seq, &accounts))])
            }
            opcode::SERVICE_LOGIN => self.service_login(conn, role, seq).await,
            opcode::CHARA_MAKE => self.chara_make(conn, role, seq, body).await,
            opcode::GAME_LOGIN => self.game_login(conn, role, seq, body).await,
            other => {
                info!(opcode = format_args!("{other:#06x}"), payload = %hex::encode(body), "  unhandled ipc");
                Ok(vec![])
            }
        };
        result.unwrap_or_else(|e| {
            warn!(opcode = format_args!("{op:#06x}"), "  {e}");
            vec![(opcode::ERROR, ipc::error(seq, 1001, ""))]
        })
    }

    async fn account(&self, conn: &mut Conn) -> anyhow::Result<i64> {
        if let Some(id) = conn.account {
            return Ok(id);
        }
        let id = self.db.login.ensure_account(TEST_ACCOUNT, now()).await?;
        conn.account = Some(id);
        Ok(id)
    }

    async fn service_login(&self, conn: &mut Conn, role: HostRole, seq: u64) -> anyhow::Result<Vec<(u16, Bytes)>> {
        let account = self.account(conn).await?;
        let mut charas = Vec::new();
        if role.lists_characters() {
            for c in self.db.characters.characters_of(account).await? {
                charas.push(CharaEntry {
                    content_id: c.content_id,
                    world_id: c.world_id,
                    name: c.name.clone(),
                    appearance: select_data(&c.name, &c.appearance, c.position.map(|p| p.zone_id as u16)).into_bytes(),
                });
            }
        }
        info!(account, characters = charas.len(), "  service login");
        // character select waits on every entry; an empty list never shows (see empty_chara_list)
        if charas.is_empty() {
            charas.push(CharaEntry::default());
        }
        let mut out = vec![(opcode::SERVER_LIST, ipc::server_list(seq, &role.worlds()))];
        out.extend(ipc::chara_list(seq, &charas).into_iter().map(|b| (opcode::CHARA_LIST, b)));
        Ok(out)
    }

    async fn chara_make(
        &self,
        conn: &mut Conn,
        role: HostRole,
        seq: u64,
        body: &[u8],
    ) -> anyhow::Result<Vec<(u16, Bytes)>> {
        let req = ipc::CharaMakeRequest::parse(body).ok_or_else(|| anyhow::anyhow!("short chara make"))?;
        info!(step = req.step, more = req.more, world = req.world_id, name = %req.name, "  chara make");
        let account = self.account(conn).await?;
        if conn.making.is_none() || req.step == 1 {
            conn.making = Some(Making { content_id: self.next_content_id().await?, ..Default::default() });
        }
        let making = conn.making.as_mut().unwrap();
        // only the first step names the world and the character; made on Lobby, played on Crystal
        if req.world_id != 0 {
            making.world_id =
                if role == HostRole::Create && req.world_id == LOBBY_WORLD { CRYSTAL_WORLD } else { req.world_id };
        }
        if !req.name.is_empty() {
            making.name = req.name.clone();
        }
        if making.appending {
            making.appearance.extend_from_slice(&req.appearance);
        } else {
            making.appearance = req.appearance.clone();
        }
        making.appending = req.more;
        if req.more {
            return Ok(vec![]);
        }

        if req.step == 1 && !self.db.characters.character_name_free(making.world_id, &making.name).await? {
            anyhow::bail!("name {:?} is taken", making.name);
        }
        let appearance = String::from_utf8_lossy(&making.appearance).into_owned();
        if req.step == 2 {
            let make =
                CharaMake::parse(&appearance).ok_or_else(|| anyhow::anyhow!("unreadable CharaMake: {appearance}"))?;
            let id = self
                .db
                .characters
                .create_character(account, making.content_id, making.world_id, &making.name, &appearance, now())
                .await?;
            let (zone_id, [x, y, z, rotation]) = chara::opening_point(make.city);
            let start = crystal_db::Position { zone_id: zone_id as u32, x, y, z, rotation };
            self.db.characters.save_position(making.content_id, start).await?;
            info!(id, content_id = making.content_id, name = %making.name, city = make.city, class = make.class, zone_id, "  character made");
        }
        let entry = CharaEntry {
            content_id: making.content_id,
            world_id: making.world_id,
            name: making.name.clone(),
            appearance: select_data(&making.name, &appearance, None).into_bytes(),
        };
        Ok(vec![(opcode::CHARA_MAKE_REPLY, ipc::chara_make_reply(seq, req.step, &entry))])
    }

    async fn game_login(
        &self,
        conn: &mut Conn,
        role: HostRole,
        seq: u64,
        body: &[u8],
    ) -> anyhow::Result<Vec<(u16, Bytes)>> {
        if role == HostRole::Create {
            anyhow::bail!("game login refused: this host only makes characters");
        }
        let content_id = body.get(8..0x10).map(|b| u64::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
        let account = self.account(conn).await?;
        let chara = self
            .db
            .characters
            .character_by_content_id(content_id)
            .await?
            .filter(|c| c.account_id == account)
            .ok_or_else(|| anyhow::anyhow!("game login: no character {content_id:#x} on this account"))?;
        let session = format!("{content_id:016x}{:08x}", rand_u32());
        self.state.set(&zone_session_key(&session), content_id.to_string().as_bytes(), Some(ZONE_SESSION_TTL)).await?;
        let actor_id = PLAYER_ACTOR_BASE + chara.id as u32;
        let host = self.zone_addr.ip().to_string();
        info!(name = %chara.name, actor_id = format_args!("{actor_id:#x}"), %session, zone = %self.zone_addr, "  game login");
        Ok(vec![(
            opcode::GAME_LOGIN_REPLY,
            ipc::game_login_reply(seq, actor_id, content_id, &session, &host, self.zone_addr.port()),
        )])
    }

    /// The next content id: one above the highest stored, the first time.
    async fn next_content_id(&self) -> anyhow::Result<u64> {
        if self.next_content_id.load(Ordering::Relaxed) == 0 {
            let start = (self.db.characters.max_content_id().await? + 1).max(FIRST_CONTENT_ID);
            let _ = self.next_content_id.compare_exchange(0, start, Ordering::Relaxed, Ordering::Relaxed);
        }
        Ok(self.next_content_id.fetch_add(1, Ordering::Relaxed))
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

/// The `ClientSelectData` text for a character from its stored CharaMake text (empty when that
/// cannot be read: the client then shows a default appearance).
fn select_data(name: &str, chara_make: &str, zone: Option<u16>) -> String {
    match CharaMake::parse(chara_make) {
        Some(make) => {
            ClientSelectData::from_chara_make(name, &make, zone.unwrap_or_else(|| starting_zone(make.city))).to_json()
        }
        None => String::new(),
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Not cryptographic: a session id only has to differ between logins.
fn rand_u32() -> u32 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    nanos ^ (std::process::id().rotate_left(16))
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
    debug!(
        size = h.size,
        connection = ?h.connection_type,
        segments = h.segment_count,
        compressed = h.compressed,
        "frame"
    );
    for segment in &frame.segments {
        let s = &segment.header;
        debug!(r#type = ?s.segment_type, size = s.size, source = s.source, target = s.target, "  segment");
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
