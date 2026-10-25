//! The IPC header every lobby, zone and chat IPC segment (type 3) starts with.

use bytes::{BufMut, BytesMut};

/// ```text
/// 0x00  u16  0x14
/// 0x02  u16  opcode
/// 0x04  u16  0
/// 0x06  u16  server id
/// 0x08  u32  timestamp (unix seconds)
/// 0x0C  u32  0
/// ```
/// As the client writes it. The lobby reads only the opcode back (@0xe3b8f0); the zone also keeps
/// the server id of an InitZone (0x19a) @6 (@0x1047bd0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcHeader {
    pub magic: u16,
    pub opcode: u16,
    pub server_id: u16,
    pub timestamp: u32,
}

impl IpcHeader {
    pub const SIZE: usize = 0x10;
    pub const MAGIC: u16 = 0x14;

    pub fn parse(b: &[u8]) -> Option<Self> {
        let b = b.get(..Self::SIZE)?;
        Some(Self {
            magic: u16::from_le_bytes([b[0], b[1]]),
            opcode: u16::from_le_bytes([b[2], b[3]]),
            server_id: u16::from_le_bytes([b[6], b[7]]),
            timestamp: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        })
    }

    pub fn write(&self, out: &mut BytesMut) {
        out.put_u16_le(self.magic);
        out.put_u16_le(self.opcode);
        out.put_u16_le(0);
        out.put_u16_le(self.server_id);
        out.put_u32_le(self.timestamp);
        out.put_u32_le(0);
    }
}

/// An IPC segment payload: the header, then `body`.
pub fn packet(opcode: u16, timestamp: u32, body: &[u8]) -> BytesMut {
    let mut out = BytesMut::with_capacity(IpcHeader::SIZE + body.len());
    IpcHeader { magic: IpcHeader::MAGIC, opcode, server_id: 0, timestamp }.write(&mut out);
    out.extend_from_slice(body);
    out
}

/// Unix seconds, as IPC headers carry them.
pub fn timestamp() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        // the client's version request, as captured and decrypted
        let raw = hex::decode("1400050000000000f67fde6a00000000").unwrap();
        let h = IpcHeader::parse(&raw).unwrap();
        assert_eq!(h, IpcHeader { magic: 0x14, opcode: 5, server_id: 0, timestamp: 0x6ade_7ff6 });
        let mut out = BytesMut::new();
        h.write(&mut out);
        assert_eq!(&out[..], &raw[..]);
    }
}
