use bytes::{Bytes, BytesMut};
use crystal_net::codec::{CodecError, RawFrameCodec};
use crystal_net::{ConnectionType, Frame, FrameCodec, Segment, SegmentType};
use tokio_util::codec::{Decoder, Encoder};

/// The β client's first lobby frames (protocol/672261/captures/README.md).
const CAPTURE: &[u8] = include_bytes!("../../../protocol/672261/captures/lobby-encryption-init.bin");

/// Feeds `input` to the decoder a few bytes at a time, as a socket might deliver it.
fn decode_all<D: Decoder>(mut codec: D, input: &[u8], chunk: usize) -> Vec<D::Item>
where
    D::Error: std::fmt::Debug,
{
    let mut buf = BytesMut::new();
    let mut out = Vec::new();
    for piece in input.chunks(chunk) {
        buf.extend_from_slice(piece);
        while let Some(item) = codec.decode(&mut buf).unwrap() {
            out.push(item);
        }
    }
    assert!(buf.is_empty(), "{} bytes left undecoded", buf.len());
    out
}

#[test]
fn capture_decodes() {
    for chunk in [1, 7, 0x20, 664, CAPTURE.len()] {
        let frames = decode_all(FrameCodec, CAPTURE, chunk);
        assert_eq!(frames.len(), 4);
        for frame in &frames {
            assert_eq!(frame.header.size, 664);
            assert_eq!(frame.header.connection_type, ConnectionType::Lobby);
            assert!(!frame.header.compressed);
            assert_eq!(frame.segments.len(), 1);
            let segment = &frame.segments[0];
            assert_eq!(segment.header.segment_type, SegmentType::EncryptionInit);
            assert_eq!(segment.header.size, 632);
            assert_eq!(segment.payload.len(), 632 - 0x10);
            assert_eq!(&segment.payload[0x24..0x34], b"Test Ticket Data");
        }
    }
}

#[test]
fn capture_reencodes_byte_exact() {
    let raw = decode_all(RawFrameCodec, CAPTURE, 64);
    for raw in raw {
        let frame = crystal_net::codec::decode_frame(raw.clone()).unwrap();
        let mut out = BytesMut::new();
        FrameCodec.encode(&frame, &mut out).unwrap();
        assert_eq!(&out[..], &raw[..]);
    }
}

#[test]
fn compressed_round_trip() {
    let mut frame = Frame::new(
        ConnectionType::Zone,
        vec![
            Segment::new(SegmentType::Ipc, 0x1000_0001, 0x1000_0001, Bytes::from(vec![0xAB; 3000])),
            Segment::new(SegmentType::KeepAlive, 0, 0, Bytes::from_static(&[1, 2, 3, 4, 5, 6, 7, 8])),
        ],
    );
    frame.header.compressed = true;
    let mut wire = BytesMut::new();
    FrameCodec.encode(&frame, &mut wire).unwrap();
    assert!(wire.len() < 3000, "compressed: {} bytes", wire.len());

    let decoded = decode_all(FrameCodec, &wire, 100);
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].segments, frame.segments);
    assert_eq!(decoded[0].header.connection_type, ConnectionType::Zone);
}

#[test]
fn rejects_bad_sizes() {
    // Frame size smaller than its own header.
    let mut bad = BytesMut::from(&CAPTURE[..664]);
    bad[0x10..0x14].copy_from_slice(&0x10u32.to_le_bytes());
    assert!(matches!(FrameCodec.decode(&mut bad), Err(CodecError::FrameSize(0x10))));

    // Segment claims more than the frame holds.
    let mut bad = BytesMut::from(&CAPTURE[..664]);
    bad[0x20..0x24].copy_from_slice(&0x1000u32.to_le_bytes());
    assert!(matches!(FrameCodec.decode(&mut bad), Err(CodecError::SegmentSize { index: 0, .. })));

    // Bytes after the last segment.
    let mut bad = BytesMut::from(&CAPTURE[..664]);
    bad[0x20..0x24].copy_from_slice(&0x200u32.to_le_bytes());
    assert!(matches!(FrameCodec.decode(&mut bad), Err(CodecError::TrailingBytes(0x78))));
}
