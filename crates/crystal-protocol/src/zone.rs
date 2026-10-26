//! Zone IPC bodies, as ffxiv.exe 672261 reads and writes them.
//!
//! Down opcodes go through `PacketDispatcher::vf1` (@0xba51d0), which switches on a byte table
//! (`tools/re/zonetable.py`); the handlers named below are the cases it calls with the body. The
//! segment's source is the actor a packet is about, where the handler takes it.

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
    /// Up: chat; see [`super::ChatUp`]. The sender's client shows its own message.
    pub const CHAT: u16 = 0x67;
    /// Down (the same number): someone says something; see [`super::ChatDown`]. Case 0 of the
    /// dispatcher hands the packet to the chat log (`UIModule` slot 31 kind 1 → @0x45b210), then
    /// plays the speaker's talking motion (@0xa4f220 → @0xaa5d80, louder for 0x0b and 0x1e).
    pub const CHAT_DOWN: u16 = 0x67;
    /// Up (0x18 bytes, a flag @0): log out (sent by @0xc2bba0, after the menu's Logout set the
    /// client's logout-pending flag). Answered with [`LOGOUT_REPLY`]; the client then closes its
    /// connections itself. A server-side close is shown as "connection with the server lost".
    pub const LOGOUT: u16 = 0x263;
    /// Down: go ahead and log out. The game dispatcher passes it to `UIModule` slot 31 as kind 7
    /// (@0xba51d0 case 0x92 → @0x46af90 → @0x433ab0), which, with logout pending, starts the logout
    /// (@0x7e4b80: fade, a 60-frame countdown, then @0xc2e910 tears down the zone and chat
    /// connections and reports logout code 0, no error).
    pub const LOGOUT_REPLY: u16 = 0x2d6;
    /// Up (0x10 bytes): the player walked into a zone exit: the exit range's instance id @0 (in the
    /// territory's layout), the player's position @4. Answered with [`INIT_ZONE`] for where the exit
    /// leads.
    pub const ZONE_LINE: u16 = 0x190;
    /// Down: a character appears; see [`super::Spawn`] (case 0x3c, `HandleSpawn`). Sent as the
    /// client's own actor id (the segment's source), it becomes the player (@0xa64860 compares it
    /// with the id game login gave); any other id is another character.
    pub const SPAWN: u16 = 0x190;
    /// Up and down: the block list; see [`super::blacklist`]. Up (0x18 bytes, from
    /// `InfoProxyBlacklist::vf4` @0x43fb60 and vf7): the page index wanted @0x10, the request key
    /// @0x11. Down (case 0x11 → @0xb037e0 → info proxy 4, `InfoProxyBlacklist::vf7` @0x43fa50).
    /// Until a last page arrives the client treats every speaker as blocked (@0x4355d0: the
    /// loaded flag @+0x660, set by vf5) and shows no one's chat.
    pub const BLACKLIST: u16 = 0xe3;
    /// Up (0x10 bytes): the player talked to something with an event: its id u64 @0 (the
    /// client's own for NPCs it placed from its layouts), the event (handler type << 16 | row)
    /// @8. The client waits for the event to start and end ([`EVENT_START`], [`EVENT_FINISH`]);
    /// answered with neither, it hangs.
    pub const TALK_EVENT: u16 = 0x1c2;
    /// Down: an event starts; see [`super::event_start`] (case 0x65 → @0x6f0be0).
    pub const EVENT_START: u16 = 0x1cc;
    /// Down: an event ends and frees the player; see [`super::event_finish`] (case 0x66 →
    /// @0x6fd010).
    pub const EVENT_FINISH: u16 = 0x1cd;
    /// Up: the client's command / target changes (Sapphire's ClientTrigger family), and the
    /// discovery of a map area; not acted on yet.
    pub const COMMAND: u16 = 0x191;
    pub const DISCOVERY: u16 = 0x194;
    /// Up (8 bytes, after Init): the linkshell list is wanted; not answered yet.
    pub const LINKSHELL_LIST: u16 = 0xf6;
    /// Down: a character goes away; see [`super::despawn`] (case 0x3d → @0xa64000).
    pub const DESPAWN: u16 = 0x191;
    /// Down: another character moves; see [`super::ActorMove`] (case 0x3e → @0xa55ae0, which
    /// queues the body under the segment's source; @0xa55030 plays the queue).
    pub const ACTOR_MOVE: u16 = 0x192;
}

/// Chat types: `LogKind` rows. The chat log drops a type every tab filters (@0x43b6e0), and the
/// tabs' filters are `LogFilter` rows: a type that sheet does not list, like 1 (Debug), is never
/// shown.
pub mod chat {
    /// "System Messages" (`LogKind` 57, `LogFilter` 21): the text alone, shown by default.
    pub const SYSTEM: u16 = 0x39;
    pub const SAY: u16 = 0x0a;
    pub const SHOUT: u16 = 0x0b;
    pub const YELL: u16 = 0x1e;
}

fn f32_at(b: &[u8], at: usize) -> Option<f32> {
    b.get(at..at + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

/// Text up to its first zero.
fn c_str(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

/// The body of [`opcode::UPDATE_POSITION`] (0x18 bytes, as the client sends it):
///
/// ```text
/// 0x00  f32     rotation
/// 0x04  f32     rotation before slipping (Sapphire's dirBeforeSlip)
/// 0x08  u8      movement flags (Sapphire: 2 walking, 4 strafing, 0x10 jumping)
/// 0x09  u8      collision flags (1 leaving the ground, 2 landing, 4 falling)
/// 0x0A  u8      head rotation
/// 0x0C  f32 x3  where the player is
/// ```
/// Only rotation and position are known from this client so far (the rest was zero walking).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PositionUpdate {
    pub rotation: f32,
    pub rotation_before_slip: f32,
    pub flags: u8,
    pub collision: u8,
    pub head: u8,
    pub pos: [f32; 3],
}

impl PositionUpdate {
    pub const SIZE: usize = 0x18;

    pub fn parse(body: &[u8]) -> Option<Self> {
        if body.len() < Self::SIZE {
            return None;
        }
        let f = |at| f32_at(body, at);
        Some(Self {
            rotation: f(0)?,
            rotation_before_slip: f(4)?,
            flags: body[8],
            collision: body[9],
            head: body[0xa],
            pos: [f(0xc)?, f(0x10)?, f(0x14)?],
        })
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[0..4].copy_from_slice(&self.rotation.to_le_bytes());
        b[4..8].copy_from_slice(&self.rotation_before_slip.to_le_bytes());
        (b[8], b[9], b[0xa]) = (self.flags, self.collision, self.head);
        for (i, v) in self.pos.iter().enumerate() {
            b[0xc + 4 * i..0x10 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        b.freeze()
    }
}

/// [`Spawn::kind`] values (the client's switch on @0x30, @0xaac200).
pub mod kind {
    pub const PLAYER: u8 = 1;
    pub const BATTLE_NPC: u8 = 2;
    pub const EVENT_NPC: u8 = 3;
}

/// [`opcode::SPAWN`]'s body, 0x288 bytes. The client queues it whole (@0xa5d350) and reads it when
/// the character is made (@0xaac200). The offsets below are the client's; where Sapphire's 4.0
/// ActorSpawn agrees (weapons @0x48 on), its names are used.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spawn {
    /// What the character is made from (@4): for an event NPC (kind 3) its `ENpcBase` row, from
    /// which the client builds its look itself (@0xaac200 → @0xaaa870).
    pub base_id: u32,
    /// Which of the client's 100 character slots (0 for the player).
    pub index: u8,
    /// 1 alive, 2 dead, 3 posed (@0x2c; Sapphire's `status`).
    pub status: u8,
    /// 1 a player, 2 a battle NPC, 3 an event NPC (@0x30; the client's switch).
    pub kind: u8,
    /// For kinds 2 and 3, where the name comes from (@0x38): a `BNpcName` / `ENpcResident` row,
    /// looked up by @0x463610 (`ReadSpawn` @0xaac200; the text @0x208 is used instead when the
    /// byte @0x31 is 3).
    pub name_id: u32,
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
        b[4..8].copy_from_slice(&self.base_id.to_le_bytes());
        b[0x2b] = self.index;
        b[0x2c] = self.status;
        b[0x30] = self.kind;
        b[0x38..0x3c].copy_from_slice(&self.name_id.to_le_bytes());
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

    /// The fields [`Self::to_bytes`] writes (rotation to the u16's precision).
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        let u64_at = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let u16_at = |at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
        let turn = u16_at(0x60) as f32 / 65535.0 * std::f32::consts::TAU - std::f32::consts::PI;
        let mut models = [0; 10];
        for (i, m) in models.iter_mut().enumerate() {
            *m = u32_at(b, 0x244 + 4 * i)?;
        }
        Some(Self {
            base_id: u32_at(b, 4)?,
            index: b[0x2b],
            status: b[0x2c],
            kind: b[0x30],
            name_id: u32_at(b, 0x38)?,
            main_weapon: u64_at(0x48),
            sub_weapon: u64_at(0x50),
            rotation: turn,
            model: u16_at(0x62),
            level: b[0x6a],
            class: b[0x6b],
            hp: u32_at(b, 0x6c)?,
            mp: u16_at(0x70),
            hp_max: u32_at(b, 0x74)?,
            mp_max: u16_at(0x78),
            name: String::from_utf8_lossy(c_str(&b[0x208..0x228])).into_owned(),
            customize: b[0x228..0x228 + 26].try_into().unwrap(),
            models,
            pos: [f32_at(b, 0x26c)?, f32_at(b, 0x270)?, f32_at(b, 0x274)?],
        })
    }
}

/// [`opcode::DESPAWN`]'s body (8 bytes): the character's slot @0 and its actor id @4. The client
/// frees the slot only if the character there has that id (@0xa64000 compares +0x74).
pub fn despawn(slot: u8, actor: u32) -> Bytes {
    let mut b = BytesMut::zeroed(8);
    b[0] = slot;
    b[4..8].copy_from_slice(&actor.to_le_bytes());
    b.freeze()
}

/// An empty page of [`opcode::BLACKLIST`] down (0x328 bytes): 20 entries of 0x28 (content id u64,
/// name char[32]; a zero id ends the list), the next page's index @0x320 (0: this is the last),
/// this page's index @0x321 (0 clears the list first), the request key @0x322. The client takes
/// a page only when the key is that of its request (`InfoProxyBlacklist` +0xd; 0 before it made
/// one); the last page marks the list loaded.
pub fn blacklist(index: u8, key: u8) -> Bytes {
    let mut b = BytesMut::zeroed(0x328);
    b[0x321] = index;
    b[0x322] = key;
    b.freeze()
}

/// The page index and request key of an up [`opcode::BLACKLIST`].
pub fn parse_blacklist_request(b: &[u8]) -> Option<(u8, u8)> {
    Some((*b.get(0x10)?, *b.get(0x11)?))
}

/// Event handler types: an event id's top 16 bits.
pub mod event {
    pub const QUEST: u16 = 0x0001;
    pub const WARP: u16 = 0x0002;
    /// The event kind `EVENT_START` and `EVENT_FINISH` carry for talking.
    pub const TALK: u8 = 1;
}

/// [`opcode::TALK_EVENT`]'s target and event id.
pub fn parse_talk_event(b: &[u8]) -> Option<(u64, u32)> {
    Some((u64::from_le_bytes(b.get(..8)?.try_into().ok()?), u32_at(b, 8)?))
}

/// [`opcode::EVENT_START`] (0x18 bytes; the dispatcher passes @8, @0, @4, @0xc, @0x10, @0xd):
/// the target u64 @0, the event id @8, the event kind @0xc, flags @0xd, an argument @0x10.
pub fn event_start(target: u64, event_id: u32, kind: u8, flags: u8, arg: u32) -> Bytes {
    let mut b = BytesMut::zeroed(0x18);
    b[0..8].copy_from_slice(&target.to_le_bytes());
    b[8..0xc].copy_from_slice(&event_id.to_le_bytes());
    (b[0xc], b[0xd]) = (kind, flags);
    b[0x10..0x14].copy_from_slice(&arg.to_le_bytes());
    b.freeze()
}

/// [`opcode::EVENT_FINISH`] (0x10 bytes; the dispatcher passes @0, @4, @8, @5): the event id @0,
/// its kind @4, the result @5 (1 frees the player), an argument @8.
pub fn event_finish(event_id: u32, kind: u8, result: u8, arg: u32) -> Bytes {
    let mut b = BytesMut::zeroed(0x10);
    b[0..4].copy_from_slice(&event_id.to_le_bytes());
    (b[4], b[5]) = (kind, result);
    b[8..0xc].copy_from_slice(&arg.to_le_bytes());
    b.freeze()
}

/// The slot and actor id of a [`despawn`] body.
pub fn parse_despawn(b: &[u8]) -> Option<(u8, u32)> {
    Some((*b.first()?, u32_at(b, 4)?))
}

/// Radians (-pi..pi) as a byte over the full turn, as [`ActorMove`] carries them: the client
/// turns `b` back into `b * (2pi * 100 / 255) * 0.01 - pi` (@0xa55030).
pub fn rotation_to_u8(r: f32) -> u8 {
    ((r + std::f32::consts::PI) / std::f32::consts::TAU * 255.0).round().clamp(0.0, 255.0) as u8
}

pub fn rotation_from_u8(b: u8) -> f32 {
    b as f32 / 255.0 * std::f32::consts::TAU - std::f32::consts::PI
}

/// A coordinate as [`ActorMove`] carries it: -1000..1000 over a u16; the client turns `v` back
/// into `v * 3.0518043 * 0.01 - 1000` (@0xa55030).
pub fn coordinate_to_u16(v: f32) -> u16 {
    ((v + 1000.0) / 0.030_518_043).round().clamp(0.0, 65535.0) as u16
}

pub fn coordinate_from_u16(v: u16) -> f32 {
    v as f32 * 0.030_518_043 - 1000.0
}

/// [`opcode::ACTOR_MOVE`]'s body, 0x10 bytes, as @0xa55030 plays it (Sapphire's `ActorMove`
/// has the same fields):
///
/// ```text
/// 0x00  u8      rotation (see rotation_to_u8)
/// 0x01  u8      rotation before slipping (the head, for Sapphire)
/// 0x02  u8      movement flags (as the mover's PositionUpdate @8)
/// 0x03  u8      collision flags (as @9)
/// 0x04  u8      speed (Sapphire: 24 walking, 60 running)
/// 0x06  u16 x3  position (see coordinate_to_u16)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ActorMove {
    pub rotation: f32,
    pub rotation_before_slip: f32,
    pub flags: u8,
    pub collision: u8,
    pub speed: u8,
    pub pos: [f32; 3],
}

impl ActorMove {
    pub const SIZE: usize = 0x10;
    pub const SPEED_WALK: u8 = 24;
    pub const SPEED_RUN: u8 = 60;
    /// [`PositionUpdate::flags`]: walking.
    pub const FLAG_WALKING: u8 = 2;

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[0] = rotation_to_u8(self.rotation);
        b[1] = rotation_to_u8(self.rotation_before_slip);
        (b[2], b[3], b[4]) = (self.flags, self.collision, self.speed);
        for (i, v) in self.pos.iter().enumerate() {
            b[6 + 2 * i..8 + 2 * i].copy_from_slice(&coordinate_to_u16(*v).to_le_bytes());
        }
        b.freeze()
    }

    pub fn parse(b: &[u8]) -> Option<Self> {
        let b = b.get(..Self::SIZE)?;
        let c = |at: usize| coordinate_from_u16(u16::from_le_bytes([b[at], b[at + 1]]));
        Some(Self {
            rotation: rotation_from_u8(b[0]),
            rotation_before_slip: rotation_from_u8(b[1]),
            flags: b[2],
            collision: b[3],
            speed: b[4],
            pos: [c(6), c(8), c(10)],
        })
    }
}

/// [`opcode::CHAT`] up (0x420 bytes): the sender's actor id @4, position @8, facing @0x14, the
/// chat type @0x18 (see [`chat`]), the text @0x1a, zero-terminated.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatUp {
    pub actor: u32,
    pub pos: [f32; 3],
    pub rotation: f32,
    pub kind: u16,
    pub text: String,
}

impl ChatUp {
    pub const SIZE: usize = 0x420;

    pub fn parse(body: &[u8]) -> Option<Self> {
        let kind = body.get(0x18..0x1a).map(|b| u16::from_le_bytes([b[0], b[1]]))?;
        Some(Self {
            actor: u32_at(body, 4).unwrap_or(0),
            pos: [f32_at(body, 8).unwrap_or(0.0), f32_at(body, 0xc).unwrap_or(0.0), f32_at(body, 0x10).unwrap_or(0.0)],
            rotation: f32_at(body, 0x14).unwrap_or(0.0),
            kind,
            text: String::from_utf8_lossy(c_str(body.get(0x1a..).unwrap_or_default())).into_owned(),
        })
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[4..8].copy_from_slice(&self.actor.to_le_bytes());
        for (i, v) in self.pos.iter().enumerate() {
            b[8 + 4 * i..0xc + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        b[0x14..0x18].copy_from_slice(&self.rotation.to_le_bytes());
        b[0x18..0x1a].copy_from_slice(&self.kind.to_le_bytes());
        let n = self.text.len().min(Self::SIZE - 0x1a - 1);
        b[0x1a..0x1a + n].copy_from_slice(&self.text.as_bytes()[..n]);
        b.freeze()
    }
}

/// [`opcode::CHAT_DOWN`]'s body (0x434 bytes), as the chat log (@0x45b210, which gets the whole
/// packet: its offsets are these plus 0x10) and the talking motion (@0xa4f220) read it:
///
/// ```text
/// 0x00  u16       chat type (see `chat`)
/// 0x04  u32       the speaker's actor id (where Sapphire has it; this client reads @0x10)
/// 0x08  u64       the speaker's content id (checked against the block list)
/// 0x10  u32       the speaker's actor id (whose character plays the talking motion)
/// 0x14  char[32]  the speaker's name
/// 0x34  char[]    the text, zero-terminated
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatDown {
    pub kind: u16,
    pub actor: u32,
    pub content_id: u64,
    pub name: String,
    pub text: String,
}

impl ChatDown {
    pub const SIZE: usize = 0x434;

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[0..2].copy_from_slice(&self.kind.to_le_bytes());
        b[4..8].copy_from_slice(&self.actor.to_le_bytes());
        b[8..0x10].copy_from_slice(&self.content_id.to_le_bytes());
        b[0x10..0x14].copy_from_slice(&self.actor.to_le_bytes());
        let n = self.name.len().min(0x1f);
        b[0x14..0x14 + n].copy_from_slice(&self.name.as_bytes()[..n]);
        let n = self.text.len().min(Self::SIZE - 0x34 - 1);
        b[0x34..0x34 + n].copy_from_slice(&self.text.as_bytes()[..n]);
        b.freeze()
    }

    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 0x34 {
            return None;
        }
        Some(Self {
            kind: u16::from_le_bytes([b[0], b[1]]),
            actor: u32_at(b, 0x10)?,
            content_id: u64::from_le_bytes(b[8..0x10].try_into().unwrap()),
            name: String::from_utf8_lossy(c_str(&b[0x14..0x34])).into_owned(),
            text: String::from_utf8_lossy(c_str(&b[0x34..])).into_owned(),
        })
    }
}

/// The payload of a session init segment (type 1, 0x38 bytes with its header), as the client
/// sends it on its zone and chat connections: the actor id game login gave, as decimal text @4.
pub fn session_init(actor: u32) -> Bytes {
    let mut b = BytesMut::zeroed(0x38 - 0x10);
    let text = actor.to_string();
    b[4..4 + text.len()].copy_from_slice(text.as_bytes());
    b.freeze()
}

/// The actor id in a session init's payload: decimal text @4, zero-terminated.
pub fn session_actor(payload: &[u8]) -> Option<u32> {
    std::str::from_utf8(c_str(payload.get(4..)?)).ok()?.parse().ok()
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

/// The territory and position of an [`init_zone`] body.
pub fn parse_init_zone(b: &[u8]) -> Option<(u16, [f32; 3])> {
    let t = u16::from_le_bytes(b.get(2..4)?.try_into().unwrap());
    Some((t, [f32_at(b, 0x18)?, f32_at(b, 0x1c)?, f32_at(b, 0x20)?]))
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
        assert_eq!(parse_init_zone(&b), Some((181, [-53.0, 18.0, 0.0])));
    }

    #[test]
    fn spawn_layout() {
        let spawn = Spawn {
            kind: 1,
            status: 1,
            level: 1,
            class: 3,
            name: "Null Crystal".into(),
            customize: [7; 26],
            pos: [-53.0, 18.0, 0.0],
            rotation: 0.0,
            ..Default::default()
        };
        let s = spawn.to_bytes();
        assert_eq!(s.len(), 0x288);
        assert_eq!((s[0x2c], s[0x30], s[0x6a], s[0x6b]), (1, 1, 1, 3));
        assert_eq!(&s[0x208..0x214], b"Null Crystal");
        assert_eq!(s[0x228], 7);
        assert_eq!(&s[0x26c..0x270], &(-53.0f32).to_le_bytes());
        // rotation 0 is the middle of the turn
        assert_eq!(u16::from_le_bytes([s[0x60], s[0x61]]), 32768);
        let back = Spawn::parse(&s).unwrap();
        assert!(back.rotation.abs() < 1e-3);
        assert_eq!(Spawn { rotation: 0.0, ..back }, spawn);
    }

    #[test]
    fn position_update_from_client() {
        // as the client sent it, walking in Limsa Lominsa Lower Decks
        let body = hex::decode("4884a3be00000000000000008da57ec2af009041ffc6b140").unwrap();
        let p = PositionUpdate::parse(&body).unwrap();
        assert!((p.rotation + 0.3194).abs() < 1e-3);
        assert!((p.pos[0] + 63.66).abs() < 1e-2 && (p.pos[1] - 18.0).abs() < 1e-2 && (p.pos[2] - 5.55).abs() < 1e-2);
        assert_eq!((p.flags, p.collision, p.head), (0, 0, 0));
        assert_eq!(PositionUpdate::parse(&body[..0x10]), None);
        assert_eq!(&p.to_bytes()[..], &body[..]);
    }

    #[test]
    fn actor_move_matches_the_clients_decoding() {
        let m = ActorMove {
            rotation: -0.3194,
            rotation_before_slip: 0.0,
            flags: 0,
            collision: 0,
            speed: ActorMove::SPEED_RUN,
            pos: [-63.66, 18.0, 5.55],
        };
        let b = m.to_bytes();
        assert_eq!(b.len(), 0x10);
        // what @0xa55030 computes from the bytes
        let rot = b[0] as f64 * (std::f64::consts::TAU * 100.0 / 255.0) * 0.01 - std::f64::consts::PI;
        assert!((rot + 0.3194).abs() < std::f64::consts::TAU / 255.0);
        let x = u16::from_le_bytes([b[6], b[7]]) as f64 * 3.051804304122925 * 0.01 - 1000.0;
        assert!((x + 63.66).abs() < 0.02, "{x}");
        assert_eq!(b[1], 128);
        assert_eq!(b[4], 60);
        let back = ActorMove::parse(&b).unwrap();
        assert!((back.pos[2] - 5.55).abs() < 0.02);
    }

    #[test]
    fn chat_round_trips() {
        let up = ChatUp { actor: 0x1000_0001, pos: [1.0, 2.0, 3.0], rotation: 0.5, kind: chat::SAY, text: "hi".into() };
        let b = up.to_bytes();
        assert_eq!(b.len(), 0x420);
        assert_eq!(&b[0x1a..0x1d], b"hi\0");
        assert_eq!(ChatUp::parse(&b), Some(up));

        let down = ChatDown {
            kind: chat::SHOUT,
            actor: 0x1000_0002,
            content_id: 0x1234,
            name: "Null Crystal".into(),
            text: "hello".into(),
        };
        let b = down.to_bytes();
        assert_eq!(b.len(), 0x434);
        assert_eq!(&b[0x10..0x14], &0x1000_0002u32.to_le_bytes());
        assert_eq!(&b[0x14..0x20], b"Null Crystal");
        assert_eq!(&b[0x34..0x3a], b"hello\0");
        assert_eq!(ChatDown::parse(&b), Some(down));
    }

    #[test]
    fn session_init_round_trips() {
        // the client's session init payload, as captured on both connections
        let payload =
            hex::decode("00000000323638343335343537000000000000000000000000000000000000000000000000000000").unwrap();
        assert_eq!(session_actor(&payload), Some(0x1000_0001));
        assert_eq!(&session_init(0x1000_0001)[..], &payload[..]);
        assert_eq!(session_actor(&[0; 8]), None);
    }

    #[test]
    fn events_from_client() {
        // talking to the Limsa lift attendant, as the client sent it
        let up = hex::decode("b3ab3e00010000001400020048f99b01").unwrap();
        let (target, event_id) = parse_talk_event(&up).unwrap();
        assert_eq!((target, event_id), (0x0000_0001_003e_abb3, 0x0002_0014));
        assert_eq!((event_id >> 16) as u16, event::WARP);
        let start = event_start(target, event_id, event::TALK, 0, 0);
        assert_eq!((start.len(), &start[..8], &start[8..0xc], start[0xc]), (0x18, &up[..8], &up[8..0xc], 1));
        let finish = event_finish(event_id, event::TALK, 1, 0);
        assert_eq!((&finish[..4], finish[4], finish[5]), (&up[8..0xc], 1, 1));
    }

    #[test]
    fn despawn_layout() {
        let b = despawn(5, 0x4000_0001);
        assert_eq!(&b[..], &[5, 0, 0, 0, 1, 0, 0, 0x40]);
        assert_eq!(parse_despawn(&b), Some((5, 0x4000_0001)));
    }

    #[test]
    fn ping_echoes_the_time() {
        let b = ping_reply(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(&b[..4], &[1, 2, 3, 4]);
        assert_eq!(b[4], 0);
    }
}
