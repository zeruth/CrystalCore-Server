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
    /// Up (the same number as [`INIT_ZONE`]; the directions have their own tables): where the
    /// player is, sent as it moves; see [`super::PositionUpdate`].
    pub const UPDATE_POSITION: u16 = 0x19a;
    /// Down: a character appears; see [`super::Spawn`]. Sent as the client's own actor id (the
    /// segment's source), it becomes the player (@0xa64860 compares it with the id game login gave).
    pub const SPAWN: u16 = 0x190;
}

/// The body of [`opcode::UPDATE_POSITION`] (0x18 bytes, as the client sends it): rotation @0, two
/// u32s (zero so far), x, y, z @0xc.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionUpdate {
    pub rotation: f32,
    pub pos: [f32; 3],
}

impl PositionUpdate {
    pub fn parse(body: &[u8]) -> Option<Self> {
        let f = |at: usize| body.get(at..at + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
        Some(Self { rotation: f(0)?, pos: [f(0xc)?, f(0x10)?, f(0x14)?] })
    }
}

/// [`opcode::SPAWN`]'s body, 0x288 bytes. The client queues it whole (@0xa5d350) and reads it when
/// the character is made (@0xaac200). The offsets below are the client's; where Sapphire's 4.0
/// ActorSpawn agrees (weapons @0x48 on), its names are used.
#[derive(Debug, Clone, Default)]
pub struct Spawn {
    /// Which of the client's 100 character slots (0 for the player).
    pub index: u8,
    /// 1 alive, 2 dead, 3 posed (@0x2c; Sapphire's `status`).
    pub status: u8,
    /// 1 a player, 2 a battle NPC, 3 an event NPC (@0x30; the client's switch).
    pub kind: u8,
    pub main_weapon: u64,
    pub sub_weapon: u64,
    /// Radians, -pi..pi; sent as a u16 over the full turn (@0x60).
    pub rotation: f32,
    /// 0: built from `customize` and `models` (@0x62).
    pub model: u16,
    pub level: u8,
    pub class: u8,
    pub hp: u32,
    pub hp_max: u32,
    pub mp: u16,
    pub mp_max: u16,
    pub name: String,
    pub customize: [u8; 26],
    /// Equipment models, 10 slots (@0x244).
    pub models: [u32; 10],
    pub pos: [f32; 3],
}

impl Spawn {
    pub const SIZE: usize = 0x288;

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[0x2b] = self.index;
        b[0x2c] = self.status;
        b[0x30] = self.kind;
        b[0x48..0x50].copy_from_slice(&self.main_weapon.to_le_bytes());
        b[0x50..0x58].copy_from_slice(&self.sub_weapon.to_le_bytes());
        let turn = ((self.rotation + std::f32::consts::PI) / std::f32::consts::TAU * 65535.0).round();
        b[0x60..0x62].copy_from_slice(&(turn.clamp(0.0, 65535.0) as u16).to_le_bytes());
        b[0x62..0x64].copy_from_slice(&self.model.to_le_bytes());
        b[0x6a] = self.level;
        b[0x6b] = self.class;
        b[0x6c..0x70].copy_from_slice(&self.hp.to_le_bytes());
        b[0x70..0x72].copy_from_slice(&self.mp.to_le_bytes());
        b[0x74..0x78].copy_from_slice(&self.hp_max.to_le_bytes());
        b[0x78..0x7a].copy_from_slice(&self.mp_max.to_le_bytes());
        let n = self.name.len().min(0x1f);
        b[0x208..0x208 + n].copy_from_slice(&self.name.as_bytes()[..n]);
        b[0x228..0x228 + 26].copy_from_slice(&self.customize);
        for (i, m) in self.models.iter().enumerate() {
            b[0x244 + 4 * i..0x248 + 4 * i].copy_from_slice(&m.to_le_bytes());
        }
        for (i, v) in self.pos.iter().enumerate() {
            b[0x26c + 4 * i..0x270 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        b.freeze()
    }
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
    fn spawn_layout() {
        let s = Spawn {
            kind: 1,
            status: 1,
            level: 1,
            class: 3,
            name: "Null Crystal".into(),
            customize: [7; 26],
            pos: [-53.0, 18.0, 0.0],
            rotation: 0.0,
            ..Default::default()
        }
        .to_bytes();
        assert_eq!(s.len(), 0x288);
        assert_eq!((s[0x2c], s[0x30], s[0x6a], s[0x6b]), (1, 1, 1, 3));
        assert_eq!(&s[0x208..0x214], b"Null Crystal");
        assert_eq!(s[0x228], 7);
        assert_eq!(&s[0x26c..0x270], &(-53.0f32).to_le_bytes());
        // rotation 0 is the middle of the turn
        assert_eq!(u16::from_le_bytes([s[0x60], s[0x61]]), 32768);
    }

    #[test]
    fn position_update_from_client() {
        // as the client sent it, walking in Limsa Lominsa Lower Decks
        let body = hex::decode("4884a3be00000000000000008da57ec2af009041ffc6b140").unwrap();
        let p = PositionUpdate::parse(&body).unwrap();
        assert!((p.rotation + 0.3194).abs() < 1e-3);
        assert!((p.pos[0] + 63.66).abs() < 1e-2 && (p.pos[1] - 18.0).abs() < 1e-2 && (p.pos[2] - 5.55).abs() < 1e-2);
        assert_eq!(PositionUpdate::parse(&body[..0x10]), None);
    }

    #[test]
    fn ping_echoes_the_time() {
        let b = ping_reply(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(&b[..4], &[1, 2, 3, 4]);
        assert_eq!(b[4], 0);
    }
}
