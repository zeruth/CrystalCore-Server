//! The frame ("bundle") and segment layouts, as the 672261 client sends them.
//!
//! Layouts are read from captures of the client (`protocol/672261/captures`); confirm each field
//! against the client's code when it is extracted in Phase 1. Later ARR clients use a 0x28-byte
//! frame header with a timestamp; this build's header is 0x20 bytes.

use bytes::{BufMut, Bytes, BytesMut};

/// ```text
/// 0x00  [u8; 16]  prefix          zero from the client
/// 0x10  u32       size            whole frame, header included
/// 0x14  u16       connection type
/// 0x16  u16       segment count
/// 0x18  u8        unknown         1 from the client
/// 0x19  u8        compressed      segments are zlib-deflated when nonzero
/// 0x1A  [u8; 6]   reserved
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub prefix: [u8; 16],
    pub size: u32,
    pub connection_type: ConnectionType,
    pub segment_count: u16,
    pub unknown_18: u8,
    pub compressed: bool,
    pub reserved: [u8; 6],
}

impl FrameHeader {
    pub const SIZE: usize = 0x20;

    /// Parses the header from at least [`Self::SIZE`] bytes.
    pub fn parse(b: &[u8]) -> Self {
        Self {
            prefix: b[0x00..0x10].try_into().unwrap(),
            size: u32::from_le_bytes(b[0x10..0x14].try_into().unwrap()),
            connection_type: ConnectionType::from(u16::from_le_bytes(b[0x14..0x16].try_into().unwrap())),
            segment_count: u16::from_le_bytes(b[0x16..0x18].try_into().unwrap()),
            unknown_18: b[0x18],
            compressed: b[0x19] != 0,
            reserved: b[0x1A..0x20].try_into().unwrap(),
        }
    }

    pub fn write(&self, out: &mut BytesMut) {
        out.put_slice(&self.prefix);
        out.put_u32_le(self.size);
        out.put_u16_le(self.connection_type.into());
        out.put_u16_le(self.segment_count);
        out.put_u8(self.unknown_18);
        out.put_u8(self.compressed as u8);
        out.put_slice(&self.reserved);
    }
}

/// ```text
/// 0x00  u32  size    segment, header included
/// 0x04  u32  source  actor id
/// 0x08  u32  target  actor id
/// 0x0C  u16  type
/// 0x0E  u16  reserved
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentHeader {
    pub size: u32,
    pub source: u32,
    pub target: u32,
    pub segment_type: SegmentType,
    pub reserved: u16,
}

impl SegmentHeader {
    pub const SIZE: usize = 0x10;

    pub fn parse(b: &[u8]) -> Self {
        Self {
            size: u32::from_le_bytes(b[0x0..0x4].try_into().unwrap()),
            source: u32::from_le_bytes(b[0x4..0x8].try_into().unwrap()),
            target: u32::from_le_bytes(b[0x8..0xC].try_into().unwrap()),
            segment_type: SegmentType::from(u16::from_le_bytes(b[0xC..0xE].try_into().unwrap())),
            reserved: u16::from_le_bytes(b[0xE..0x10].try_into().unwrap()),
        }
    }

    pub fn write(&self, out: &mut BytesMut) {
        out.put_u32_le(self.size);
        out.put_u32_le(self.source);
        out.put_u32_le(self.target);
        out.put_u16_le(self.segment_type.into());
        out.put_u16_le(self.reserved);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub header: SegmentHeader,
    /// The bytes after the segment header.
    pub payload: Bytes,
}

impl Segment {
    pub fn new(segment_type: SegmentType, source: u32, target: u32, payload: Bytes) -> Self {
        let size = (SegmentHeader::SIZE + payload.len()) as u32;
        Self { header: SegmentHeader { size, source, target, segment_type, reserved: 0 }, payload }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub header: FrameHeader,
    pub segments: Vec<Segment>,
}

impl Frame {
    /// An uncompressed frame; `size` and `segment_count` are filled in when it is encoded.
    pub fn new(connection_type: ConnectionType, segments: Vec<Segment>) -> Self {
        let header = FrameHeader {
            prefix: [0; 16],
            size: 0,
            connection_type,
            segment_count: segments.len() as u16,
            unknown_18: 1,
            compressed: false,
            reserved: [0; 6],
        };
        Self { header, segments }
    }
}

/// Which service a frame belongs to. Values from later ARR clients; 3 (lobby) is seen in captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionType {
    None,
    Zone,
    Chat,
    Lobby,
    Unknown(u16),
}

impl From<u16> for ConnectionType {
    fn from(v: u16) -> Self {
        match v {
            0 => Self::None,
            1 => Self::Zone,
            2 => Self::Chat,
            3 => Self::Lobby,
            v => Self::Unknown(v),
        }
    }
}

impl From<ConnectionType> for u16 {
    fn from(v: ConnectionType) -> Self {
        match v {
            ConnectionType::None => 0,
            ConnectionType::Zone => 1,
            ConnectionType::Chat => 2,
            ConnectionType::Lobby => 3,
            ConnectionType::Unknown(v) => v,
        }
    }
}

/// Segment types. Names from later ARR clients; 9 (encryption init) is seen in captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentType {
    SessionInit,
    Ipc,
    KeepAlive,
    KeepAliveResponse,
    EncryptionInit,
    Unknown(u16),
}

impl From<u16> for SegmentType {
    fn from(v: u16) -> Self {
        match v {
            1 => Self::SessionInit,
            3 => Self::Ipc,
            7 => Self::KeepAlive,
            8 => Self::KeepAliveResponse,
            9 => Self::EncryptionInit,
            v => Self::Unknown(v),
        }
    }
}

impl From<SegmentType> for u16 {
    fn from(v: SegmentType) -> Self {
        match v {
            SegmentType::SessionInit => 1,
            SegmentType::Ipc => 3,
            SegmentType::KeepAlive => 7,
            SegmentType::KeepAliveResponse => 8,
            SegmentType::EncryptionInit => 9,
            SegmentType::Unknown(v) => v,
        }
    }
}
