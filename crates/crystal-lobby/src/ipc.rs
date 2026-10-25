//! Lobby IPC: the segments of type 3 after the encryption handshake. Each is a 0x10-byte IPC header
//! and a body, both encrypted (the leading multiple of 32 bytes; see [`crate::crypt`]).
//!
//! Opcodes, from ffxiv.exe 672261:
//! - up (client to lobby), written by `LobbyLoginOperation` @0xe46940 per login step: 0x1f5 (0x38
//!   bytes), 0x05 (0x100; the client's version), 0x06 (0x240), 0x1f6 (0x98).
//! - down (lobby to client), dispatched by `LobbyProtoDownCallbackInterface` slot 1 @0xe3b8f0:
//!   0x01, 0x02, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x15, 0x16, 0x17, 0x1f4, 0x1f5.
//!   `LobbyLoginOperation` answers 0x05 and 0x06 with 0x0c and 0x1f5/0x1f6 with 0x1f5; 0x02 is an
//!   error for any operation (`LobbyOperation` slot 2 @0xe4fb00).

use bytes::{BufMut, Bytes, BytesMut};

/// ```text
/// 0x00  u16  0x14
/// 0x02  u16  opcode
/// 0x04  u16  0
/// 0x06  u16  server id
/// 0x08  u32  timestamp (unix seconds)
/// 0x0C  u32  0
/// ```
/// As the client writes it; it reads only the opcode back (@0xe3b8f0).
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

pub mod opcode {
    /// Up: the client's version (0x100 bytes): sequence @0, 0x6e0000 @8, 0x1347 @0xc, the session
    /// id (0x40) @0x10, "ffxiv.exe/<size>/<sha1>" (0x80) @0x50, 0x20 bytes @0xd0.
    pub const CLIENT_VERSION: u16 = 0x05;
    /// Down: an error for the pending operation: its sequence @0, the code @8, text @0x14.
    pub const ERROR: u16 = 0x02;
    /// Down: the answer to [`CLIENT_VERSION`] (read @0xe3fc70): page @8 (0 or 1 starts the list
    /// over), count @9, two bytes kept @0xa/@0xb, then `count` entries of 0x50 bytes @0x10.
    pub const SERVICE_ACCOUNT_LIST: u16 = 0x0c;
    /// Up: log in to a service account (0x28 bytes, `ServiceLoginOperation` @0xe46e80): sequence
    /// @0, a byte @8, flags @9, the account's id (from [`SERVICE_ACCOUNT_LIST`]) @0x10, a u32 @0x14.
    /// Answered with [`SERVER_LIST`], 0x16, 0x17 (each read, then still waiting) and
    /// [`CHARA_LIST`], which ends it on its last page (@0xe47070).
    pub const SERVICE_LOGIN: u16 = 0x03;
    /// Down: the worlds (read @0xe410c0): last page in bit 0 @8, the list starts over when the u16
    /// @0xa is 0, count (u16) @0xc, entries of 0x54 bytes @0x18, told apart by their u16 @2.
    pub const SERVER_LIST: u16 = 0x15;
    /// Down: the characters (read @0xe3fdf0): bit 0 @8 is the last page, the list starts over when
    /// the other bits are 0; count @9; a u32 @0xc and u16s @0x10, @0x12 kept; entries of 0x450
    /// bytes @0x18, two to a packet (0x10 + 0x18 + 2 * 0x450 is the client's 0x8c8 IPC buffer).
    /// The u16 @0x10 is the most characters a world takes: character select (@0x882980) offers a
    /// new character on a world only while it has fewer.
    pub const CHARA_LIST: u16 = 0x0d;
    /// Up: one step of making a character (0x1f0 bytes, `CharaMakeOperation` @0xe47160): sequence
    /// @0, content id @8, the step @0x19 (0x80: more chunks follow), world id @0x1a, name @0x1c,
    /// a chunk of the appearance text @0x3c. See [`super::CharaMakeRequest`].
    pub const CHARA_MAKE: u16 = 0x0b;
    /// Down: the answer to each [`CHARA_MAKE`] step; see [`super::chara_make_reply`]. Step 1
    /// carries the world and name, step 2 the content id given in the answer to step 1 and the
    /// appearance as JSON (`{"classid":118,"classname":"CharaMake","content":[...]}`).
    pub const CHARA_MAKE_REPLY: u16 = 0x0e;
    /// Up: log in to the game with a character (0x20 bytes, `GameLoginOperation`): sequence @0,
    /// content id @8. Sent after a character is made or picked; the client then shows the loading
    /// screen and waits for 0x0f (read @0xe408c0; it carries the zone server, Phase 2).
    pub const GAME_LOGIN: u16 = 0x04;
}

/// An [`opcode::ERROR`] body for the operation whose request carried `sequence`: the code @8 (the
/// client sorts 1001.., 3000.., 5000.. into classes; 1007 is handled apart), a u32 @0xc, a u16 @0x10
/// and the text @0x14 (`LobbyOperation::ReadError` @0xe4fb00).
pub fn error(sequence: u64, code: u32, text: &str) -> Bytes {
    let mut body = BytesMut::zeroed(0x218);
    body[..8].copy_from_slice(&sequence.to_le_bytes());
    body[8..0xc].copy_from_slice(&code.to_le_bytes());
    let n = text.len().min(0x200);
    body[0x14..0x14 + n].copy_from_slice(&text.as_bytes()[..n]);
    body.freeze()
}

/// The request in [`opcode::CHARA_MAKE`], as `CharaMakeOperation` @0xe47160 writes it.
#[derive(Debug, Clone)]
pub struct CharaMakeRequest {
    pub sequence: u64,
    pub content_id: u64,
    /// 1 reserves the name, 2 makes the character (with its appearance); 3, 4, 6, 7 are the
    /// other steps the reply handler @0xe45f40 knows.
    pub step: u8,
    /// More chunks of [`Self::appearance`] follow this one.
    pub more: bool,
    pub world_id: u16,
    pub name: String,
    /// This chunk of the appearance text @0x3c (400 bytes a chunk).
    pub appearance: Vec<u8>,
}

impl CharaMakeRequest {
    pub fn parse(body: &[u8]) -> Option<Self> {
        let body = body.get(..0x1cc)?;
        let cstr = |b: &[u8]| b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())].to_vec();
        Some(Self {
            sequence: sequence(body),
            content_id: u64::from_le_bytes(body[8..0x10].try_into().unwrap()),
            step: body[0x19] & 0x7f,
            more: body[0x19] & 0x80 != 0,
            world_id: u16::from_le_bytes([body[0x1a], body[0x1b]]),
            name: String::from_utf8_lossy(&cstr(&body[0x1c..0x3c])).into_owned(),
            appearance: cstr(&body[0x3c..0x1cc]),
        })
    }
}

/// A character entry of [`opcode::CHARA_LIST`] and [`opcode::CHARA_MAKE_REPLY`], 0x450 bytes (built
/// by @0xe3cb10): content id @8, a byte @0x10 (its low 6 bits matched across pages), a byte @0x11,
/// the world id @0x18 (what character select counts per world), the name @0x1a (0x20), the
/// appearance text @0x4a, parsed into `ClientSelectData`.
#[derive(Debug, Clone, Default)]
pub struct CharaEntry {
    pub content_id: u64,
    pub world_id: u16,
    pub name: String,
    pub appearance: Vec<u8>,
}

impl CharaEntry {
    pub const SIZE: usize = 0x450;

    pub fn write(&self, out: &mut [u8]) {
        let out = &mut out[..Self::SIZE];
        out.fill(0);
        out[8..0x10].copy_from_slice(&self.content_id.to_le_bytes());
        out[0x18..0x1a].copy_from_slice(&self.world_id.to_le_bytes());
        let n = self.name.len().min(0x1f);
        out[0x1a..0x1a + n].copy_from_slice(&self.name.as_bytes()[..n]);
        let n = self.appearance.len().min(Self::SIZE - 0x4a - 1);
        out[0x4a..0x4a + n].copy_from_slice(&self.appearance[..n]);
    }
}

/// The answer to one step of [`opcode::CHARA_MAKE`] (read @0xe40080): a non-zero byte @9 (the
/// entry is there), the step @0xa, the character @0x10.
pub fn chara_make_reply(sequence: u64, step: u8, chara: &CharaEntry) -> Bytes {
    let mut body = BytesMut::zeroed(0x10 + CharaEntry::SIZE);
    body[..8].copy_from_slice(&sequence.to_le_bytes());
    body[9] = 1;
    body[0xa..0xc].copy_from_slice(&(step as u16).to_le_bytes());
    chara.write(&mut body[0x10..]);
    body.freeze()
}

/// A world in [`opcode::SERVER_LIST`], 0x54 bytes. Character select (@0x882980) matches characters
/// to it by the id @0, shows the name @0x14, reads a byte @8 and tests the flags byte @4 (masks
/// @0x13598ee..): see [`world_flags`]. The index @2 (what the list is deduplicated on) and the icon
/// @0xc are the layout later ARR servers send.
#[derive(Debug, Clone)]
pub struct World {
    pub id: u16,
    pub index: u16,
    pub flags: u32,
    pub name: String,
}

/// [`World::flags`]. Character select tests 0x01 first, then 0x02 (each a state of its own, the
/// world unavailable for new characters), then whether the world is full, then 0x04.
pub mod world_flags {
    pub const STATE_1: u32 = 0x01;
    pub const STATE_2: u32 = 0x02;
    /// New characters may be made here (while the world is not full).
    pub const OPEN: u32 = 0x04;
}

/// The most characters one world takes, sent in [`opcode::CHARA_LIST`] @0x10.
pub const MAX_CHARAS_PER_WORLD: u16 = 8;
/// The account's character slots, sent in [`opcode::CHARA_LIST`] @0x12. "New Character" (@0x8a2540)
/// says the account has no slots left unless a list holds fewer characters than this, and that
/// every world is full unless one holds fewer than [`MAX_CHARAS_PER_WORLD`].
pub const CHARA_SLOTS: u16 = 8;

pub fn server_list(sequence: u64, worlds: &[World]) -> Bytes {
    const ENTRY: usize = 0x54;
    const MAX: usize = 6;
    let mut body = BytesMut::with_capacity(0x18 + MAX * ENTRY);
    body.put_u64_le(sequence);
    body.put_u16_le(1); // the last page
    body.put_u16_le(0); // from the start
    body.put_u32_le(worlds.len().min(MAX) as u32);
    body.put_u64_le(0);
    for w in worlds.iter().take(MAX) {
        body.put_u16_le(w.id);
        body.put_u16_le(w.index);
        body.put_u32_le(w.flags);
        body.put_u32_le(0);
        body.put_u32_le(0); // icon
        body.put_u32_le(0);
        let mut name = [0u8; 0x40];
        let n = w.name.len().min(name.len() - 1);
        name[..n].copy_from_slice(&w.name.as_bytes()[..n]);
        body.put_slice(&name);
    }
    body.resize(0x18 + MAX * ENTRY, 0);
    body.freeze()
}

/// A character list with no characters: one page, the first and the last, holding one empty entry
/// (content id 0). It has to hold something: character select waits for the game data of every
/// entry before it shows (`LobbyUIClient` slot 17 @0x905c90 starts a `CallbackSheetWaiter` over
/// them @0x904e60), and the sheet fires a waiter only from its loop over the entries (@0xcb8140),
/// so with none it never fires and the screen stays empty. An entry whose content id (@8) is 0 is
/// waited on as row 0.
pub fn empty_chara_list(sequence: u64) -> Bytes {
    let mut body = BytesMut::zeroed(0x18 + 2 * 0x450);
    body[..8].copy_from_slice(&sequence.to_le_bytes());
    body[8] = 1;
    body[9] = 1;
    body[0x10..0x12].copy_from_slice(&MAX_CHARAS_PER_WORLD.to_le_bytes());
    body[0x12..0x14].copy_from_slice(&CHARA_SLOTS.to_le_bytes());
    body.freeze()
}

/// An IPC segment payload: the header, then `body`.
pub fn packet(opcode: u16, timestamp: u32, body: &[u8]) -> BytesMut {
    let mut out = BytesMut::with_capacity(IpcHeader::SIZE + body.len());
    IpcHeader { magic: IpcHeader::MAGIC, opcode, server_id: 0, timestamp }.write(&mut out);
    out.extend_from_slice(body);
    out
}

/// The sequence number a request carries @0, echoed in its answer.
pub fn sequence(body: &[u8]) -> u64 {
    body.get(..8).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
}

/// One service account in [`opcode::SERVICE_ACCOUNT_LIST`]. The entry is 0x50 bytes (the client
/// copies each whole); the fields inside are the layout later ARR servers send and not yet confirmed
/// against this client: id @0, unknown @4, index @8, name (0x44) @0xc.
#[derive(Debug, Clone)]
pub struct ServiceAccount {
    pub id: u32,
    pub index: u32,
    pub name: String,
}

pub fn service_account_list(sequence: u64, accounts: &[ServiceAccount]) -> Bytes {
    const ENTRY: usize = 0x50;
    const MAX: usize = 8;
    let mut body = BytesMut::with_capacity(0x10 + MAX * ENTRY);
    body.put_u64_le(sequence);
    body.put_u8(0); // page: the first, so the client's list starts over
    body.put_u8(accounts.len().min(MAX) as u8);
    body.put_u8(0);
    body.put_u8(0);
    body.put_u32_le(0);
    for a in accounts.iter().take(MAX) {
        body.put_u32_le(a.id);
        body.put_u32_le(0);
        body.put_u32_le(a.index);
        let mut name = [0u8; ENTRY - 0xc];
        let n = a.name.len().min(name.len() - 1);
        name[..n].copy_from_slice(&a.name.as_bytes()[..n]);
        body.put_slice(&name);
    }
    body.resize(0x10 + MAX * ENTRY, 0);
    body.freeze()
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

    #[test]
    fn chara_make_request_from_client() {
        // the body of the client's first 0x0b, as captured and decrypted (the rest is zeros)
        let mut body = hex::decode("03000000000000000000000000000000000000000000000000010100").unwrap();
        body.extend_from_slice(b"Null Crystal");
        body.resize(0x1e0, 0);
        let req = CharaMakeRequest::parse(&body).unwrap();
        assert_eq!((req.sequence, req.content_id, req.step, req.more), (3, 0, 1, false));
        assert_eq!((req.world_id, req.name.as_str()), (1, "Null Crystal"));
        assert!(req.appearance.is_empty());

        let chara = CharaEntry { content_id: 7, world_id: 1, name: req.name, appearance: b"{}".to_vec() };
        let reply = chara_make_reply(3, 1, &chara);
        assert_eq!(reply.len(), 0x460);
        assert_eq!((reply[9], reply[0xa]), (1, 1));
        assert_eq!(&reply[0x18..0x20], &7u64.to_le_bytes());
        assert_eq!(&reply[0x28..0x2a], &1u16.to_le_bytes());
        assert_eq!(&reply[0x2a..0x36], b"Null Crystal");
        assert_eq!(&reply[0x5a..0x5c], b"{}");
    }

    #[test]
    fn service_account_list_layout() {
        let body = service_account_list(1, &[ServiceAccount { id: 1, index: 0, name: "FINAL FANTASY XIV".into() }]);
        assert_eq!(body.len(), 0x290);
        assert_eq!(sequence(&body), 1);
        assert_eq!((body[8], body[9]), (0, 1));
        assert_eq!(&body[0x10..0x14], &1u32.to_le_bytes());
        assert_eq!(&body[0x1c..0x2d], b"FINAL FANTASY XIV");
    }
}
