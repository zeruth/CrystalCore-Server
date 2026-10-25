//! `tokio_util` codec between a byte stream and [`Frame`]s. Segment payloads are zero-copy slices
//! of the received frame (or of its inflated body).

use std::io::{Read, Write};

use bytes::{Buf, Bytes, BytesMut};
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use tokio_util::codec::{Decoder, Encoder};

use crate::frame::{Frame, FrameHeader, Segment, SegmentHeader};

/// Largest frame accepted from a peer.
pub const MAX_FRAME_SIZE: usize = 1 << 20;
/// Largest body a compressed frame may inflate to.
pub const MAX_INFLATED_SIZE: usize = 4 << 20;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("frame size {0:#x} out of range")]
    FrameSize(usize),
    #[error("segment {index}: size {size:#x} out of range ({remaining:#x} bytes left)")]
    SegmentSize { index: usize, size: usize, remaining: usize },
    #[error("{0:#x} bytes after the last segment")]
    TrailingBytes(usize),
    #[error("compressed body inflates past {MAX_INFLATED_SIZE:#x} bytes")]
    InflatedSize,
}

/// Decodes and encodes [`Frame`]s.
#[derive(Debug, Default, Clone, Copy)]
pub struct FrameCodec;

/// Splits a stream into whole frames without decoding them, for callers that keep the raw bytes
/// (captures) and decode with [`decode_frame`].
#[derive(Debug, Default, Clone, Copy)]
pub struct RawFrameCodec;

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, CodecError> {
        match RawFrameCodec.decode(src)? {
            Some(raw) => decode_frame(raw).map(Some),
            None => Ok(None),
        }
    }
}

impl Decoder for RawFrameCodec {
    type Item = Bytes;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Bytes>, CodecError> {
        if src.len() < FrameHeader::SIZE {
            src.reserve(FrameHeader::SIZE - src.len());
            return Ok(None);
        }
        let size = u32::from_le_bytes(src[0x10..0x14].try_into().unwrap()) as usize;
        if !(FrameHeader::SIZE..=MAX_FRAME_SIZE).contains(&size) {
            return Err(CodecError::FrameSize(size));
        }
        if src.len() < size {
            src.reserve(size - src.len());
            return Ok(None);
        }
        Ok(Some(src.split_to(size).freeze()))
    }
}

/// Decodes one whole frame, as [`RawFrameCodec`] yields it.
pub fn decode_frame(raw: Bytes) -> Result<Frame, CodecError> {
    let header = FrameHeader::parse(&raw);
    let mut body = raw.slice(FrameHeader::SIZE..header.size as usize);
    if header.compressed {
        body = inflate(&body)?;
    }
    let mut segments = Vec::with_capacity(header.segment_count as usize);
    for index in 0..header.segment_count as usize {
        let remaining = body.len();
        if remaining < SegmentHeader::SIZE {
            return Err(CodecError::SegmentSize { index, size: 0, remaining });
        }
        let seg = SegmentHeader::parse(&body);
        let size = seg.size as usize;
        if size < SegmentHeader::SIZE || size > remaining {
            return Err(CodecError::SegmentSize { index, size, remaining });
        }
        let payload = body.slice(SegmentHeader::SIZE..size);
        body.advance(size);
        segments.push(Segment { header: seg, payload });
    }
    if !body.is_empty() {
        return Err(CodecError::TrailingBytes(body.len()));
    }
    Ok(Frame { header, segments })
}

fn inflate(body: &[u8]) -> Result<Bytes, CodecError> {
    let mut out = Vec::with_capacity(body.len() * 4);
    let read = ZlibDecoder::new(body).take(MAX_INFLATED_SIZE as u64 + 1).read_to_end(&mut out)?;
    if read > MAX_INFLATED_SIZE {
        return Err(CodecError::InflatedSize);
    }
    Ok(out.into())
}

impl Encoder<Frame> for FrameCodec {
    type Error = CodecError;

    fn encode(&mut self, frame: Frame, dst: &mut BytesMut) -> Result<(), CodecError> {
        self.encode(&frame, dst)
    }
}

impl Encoder<&Frame> for FrameCodec {
    type Error = CodecError;

    /// Writes `frame`, computing `size` and `segment_count`; deflates the body when
    /// `header.compressed` is set.
    fn encode(&mut self, frame: &Frame, dst: &mut BytesMut) -> Result<(), CodecError> {
        let mut body = BytesMut::with_capacity(frame.segments.iter().map(|s| s.header.size as usize).sum());
        for segment in &frame.segments {
            let header = SegmentHeader { size: (SegmentHeader::SIZE + segment.payload.len()) as u32, ..segment.header };
            header.write(&mut body);
            body.extend_from_slice(&segment.payload);
        }
        let body = if frame.header.compressed {
            let mut z = ZlibEncoder::new(Vec::with_capacity(body.len() / 2), Compression::fast());
            z.write_all(&body)?;
            BytesMut::from(&z.finish()?[..])
        } else {
            body
        };
        let size = FrameHeader::SIZE + body.len();
        if size > MAX_FRAME_SIZE {
            return Err(CodecError::FrameSize(size));
        }
        let header = FrameHeader { size: size as u32, segment_count: frame.segments.len() as u16, ..frame.header };
        dst.reserve(size);
        header.write(dst);
        dst.extend_from_slice(&body);
        Ok(())
    }
}
