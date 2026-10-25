//! Zone IPC bodies, as ffxiv.exe 672261 reads them.

use bytes::{Bytes, BytesMut};

pub mod opcode {
    /// Up and down: the ping. Up every 10 s with the client's millisecond clock @0; the client
    /// reads it back @0 of the down packet to measure the round trip, and a u16 @4 above 99 is
    /// passed on (@0x00c2fb20).
    pub const PING: u16 = 0x65;
    /// Up: the client is connected and waits for the zone.
    pub const INIT: u16 = 0x66;
    /// Up: the client finished loading the territory InitZone named.
    pub const FINISH_LOADING: u16 = 0x69;
    /// Down: load a territory and stand at a point in it; see [`super::init_zone`].
    pub const INIT_ZONE: u16 = 0x19a;
}

/// [`opcode::PING`] down: the client's time @0, the rest zero.
pub fn ping_reply(request: &[u8]) -> Bytes {
    let mut body = BytesMut::zeroed(0x40);
    let n = request.len().min(4);
    body[..n].copy_from_slice(&request[..n]);
    body.freeze()
}

/// [`opcode::INIT_ZONE`] (read @0xab56a0):
///
/// ```text
/// 0x02  u16      territory (TerritoryType row): loaded, its weather set up with @0xc
/// 0x04  u32      passed with the territory to the loader (an instance?)
/// 0x08  u32      kept
/// 0x0C  u8       weather
/// 0x0D  u8       flags (bit 0 kept; bits 1-2 reset a global)
/// 0x0E  u16 x4   passed together to the zone setup
/// 0x18  f32 x3   where the player stands
/// ```
pub fn init_zone(territory: u16, weather: u8, pos: [f32; 3]) -> Bytes {
    let mut body = BytesMut::zeroed(0x28);
    body[2..4].copy_from_slice(&territory.to_le_bytes());
    body[0xc] = weather;
    for (i, v) in pos.iter().enumerate() {
        body[0x18 + 4 * i..0x1c + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    body.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_zone_layout() {
        let b = init_zone(181, 1, [-53.0, 18.0, 0.0]);
        assert_eq!(&b[2..4], &181u16.to_le_bytes());
        assert_eq!(b[0xc], 1);
        assert_eq!(&b[0x18..0x1c], &(-53.0f32).to_le_bytes());
        assert_eq!(&b[0x1c..0x20], &18.0f32.to_le_bytes());
    }

    #[test]
    fn ping_echoes_the_time() {
        let b = ping_reply(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(&b[..4], &[1, 2, 3, 4]);
        assert_eq!(b[4], 0);
    }
}
