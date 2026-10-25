//! The lobby service. Phase 0: accept the client's lobby connection, decode and log its frames,
//! and capture them for replay. Nothing is answered yet.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use crystal_db::Databases;
use crystal_net::codec::{RawFrameCodec, decode_frame};
use crystal_net::{Frame, SegmentType};
use crystal_state::State;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_util::codec::FramedRead;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub struct Lobby {
    #[allow(dead_code)] // Phase 1: accounts and characters
    db: Databases,
    #[allow(dead_code)] // Phase 1: sessions
    state: State,
    capture_dir: Option<PathBuf>,
}

impl Lobby {
    pub fn new(db: Databases, state: State, capture_dir: Option<PathBuf>) -> Self {
        Self { db, state, capture_dir }
    }

    pub async fn handle(self: Arc<Self>, stream: TcpStream, peer: SocketAddr, shutdown: CancellationToken) {
        info!("connected");
        let mut capture = self.open_capture(peer).await;
        let mut frames = FramedRead::new(stream, RawFrameCodec);
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
            match decode_frame(raw.clone()) {
                Ok(frame) => log_frame(&frame),
                Err(e) => warn!(bytes = %hex::encode(&raw), "undecodable frame: {e}"),
            }
        }
        if let Some(mut file) = capture {
            let _ = file.flush().await;
        }
        info!("closed");
    }

    /// One file per connection, the raw frames back to back, named
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
        if s.segment_type == SegmentType::EncryptionInit {
            match EncryptionInit::parse(&segment.payload) {
                Some(init) => {
                    info!(key_phrase = %init.key_phrase, key = format_args!("{:#010x}", init.key), "  encryption init")
                }
                None => warn!(len = segment.payload.len(), "  short encryption init"),
            }
        }
        debug!(payload = %hex::encode(&segment.payload), "  payload");
    }
}

/// The client's first lobby segment. Offsets from captures of 672261; the key derivation that uses
/// them is Phase 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionInit {
    /// The session ticket text ("Test Ticket Data" with `DEV.TestSID=0`).
    pub key_phrase: String,
    pub key: u32,
}

impl EncryptionInit {
    const PHRASE: std::ops::Range<usize> = 0x24..0x64;
    const KEY: usize = 0x64;

    pub fn parse(payload: &Bytes) -> Option<Self> {
        let phrase = payload.get(Self::PHRASE)?;
        let phrase = &phrase[..phrase.iter().position(|&b| b == 0).unwrap_or(phrase.len())];
        let key = u32::from_le_bytes(payload.get(Self::KEY..Self::KEY + 4)?.try_into().ok()?);
        Some(Self { key_phrase: String::from_utf8_lossy(phrase).into_owned(), key })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_init_from_capture() {
        let capture = include_bytes!("../../../protocol/672261/captures/lobby-encryption-init.bin");
        let frame = decode_frame(Bytes::from_static(&capture[..664])).unwrap();
        let init = EncryptionInit::parse(&frame.segments[0].payload).unwrap();
        assert_eq!(init, EncryptionInit { key_phrase: "Test Ticket Data".into(), key: 0x6ade_798e });
        assert_eq!(EncryptionInit::parse(&frame.segments[0].payload.slice(..0x60)), None);
    }
}
