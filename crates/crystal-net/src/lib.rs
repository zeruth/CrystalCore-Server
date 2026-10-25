//! Packet framing shared by the lobby, zone and chat connections, and the TCP listener.

pub mod codec;
pub mod frame;
pub mod listener;

pub use codec::{CodecError, FrameCodec};
pub use frame::{ConnectionType, Frame, FrameHeader, Segment, SegmentHeader, SegmentType};
pub use listener::{Listener, serve};
