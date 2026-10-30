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
    /// Down: the player's own setup (Sapphire's PlayerStatus); see [`super::PlayerStatus`]
    /// (case 0x49 → @0xa53550 → @0xa7f520, @0xa7f9c0).
    pub const PLAYER_STATUS: u16 = 0x1a0;
    /// Down: the current class and its level and experience (Sapphire's PlayerStatusUpdate); see
    /// [`super::class_info`] (case 0x48 → @0xa4ed60).
    pub const CLASS_INFO: u16 = 0x19f;
    /// Down: the player's attributes; see [`super::base_params`] (case 0x4a → @0xa4ecf0).
    pub const BASE_PARAM: u16 = 0x1a1;
    /// Down: an order about a character (sent as it; Sapphire's ActorControl); see
    /// [`super::order`] (cases 0x33-0x35 → @0xa55f80, which switches on the category through the
    /// byte table @0xa5bfb0, `tools/re/ordertable.py`).
    pub const ORDER: u16 = 0x142;
    /// Down: a character's HP, MP, TP and statuses (sent as that character); see
    /// [`super::hud_param`] (case 0x31 → @0xa555b0).
    pub const HUD_PARAM: u16 = 0x140;
    /// Down: the player's own statuses; see [`super::status_list`] (case 0x39 → @0xa4e4b0 →
    /// @0xb7ba00, on the player's own character).
    pub const STATUS: u16 = 0x148;
    /// Down: a character starts casting; see [`super::RequestCast`] (case 0x41 → @0xa52d10).
    pub const REQUEST_CAST: u16 = 0x196;
    /// Down: an action's result on one target (sent as whoever acted); see
    /// [`super::ActionResult`] (case 0x37 → @0xa4e520, which widens it to 0x147's layout for
    /// @0xac9b30).
    pub const ACTION_RESULT1: u16 = 0x146;
    /// Down: a character's gear changed (sent as that character); see [`super::equip`]
    /// (case 0x4e → @0xa4f1b0 → @0xaa9680).
    pub const EQUIP: u16 = 0x1a5;
    /// Down: one item of a container being sent; see [`super::ItemInfo`]. Queued under the
    /// sequence @0 (@0xa55990 → @0xa54d40) until [`ITEM_SIZE`] closes it.
    pub const NORMAL_ITEM: u16 = 0x1af;
    /// Down: a container is complete: the sequence @0, its item count @4, the container @8;
    /// see [`super::container_info`]. Once that many items arrived, the client takes them
    /// (@0xa55990 case 1 → @0xa49c10).
    pub const ITEM_SIZE: u16 = 0x1b0;
    /// Down: a currency or crystal "item" (gil is item 1 in container 2000), queued as
    /// [`NORMAL_ITEM`] is; sent with [`super::ItemInfo`]'s layout (not yet seen in a reader).
    pub const GIL_ITEM: u16 = 0x1b3;
    /// Up: an item operation; see [`super::ItemOperation`] (sent by @0xa51580 and its eight
    /// siblings, one per operation; the client keeps up to 128 pending by context id).
    pub const ITEM_OPERATION_REQUEST: u16 = 0x1ae;
    /// Down: answers an operation; see [`super::item_operation_batch`] (@0xa54be0: once the
    /// batch's packet count of [`ITEM_OPERATION`]s arrived, @0xa47b20 applies it).
    pub const ITEM_OPERATION_BATCH: u16 = 0x1b1;
    /// Down: one step of an answered operation, the request's layout.
    pub const ITEM_OPERATION: u16 = 0x1b2;
    /// Down: one slot changes (0x40 bytes, @0xa4e610): [`super::ItemInfo`]'s layout.
    pub const UPDATE_ITEM: u16 = 0x1b6;
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
    /// Up (0x18 bytes): the client asks to start an event it found for itself, the event id u32
    /// @0. Sent by @0x70dfa0 when the territory's handlers offer event kind 0xf (with the
    /// territory, `ReadInitZone` @0xab56a0 → @0x6ebf80, as its argument) without flag 2; the
    /// client then waits with the event pending (event, its own `GameObject::vf0` id, kind,
    /// argument) until an [`EVENT_START`] that matches all of them (@0x6b9110). Seen after an
    /// opening territory loads: 0x130001 in 181.
    pub const EVENT_REQUEST: u16 = 0x1c6;
    /// Up (0x28 bytes): the player walked into an event range a handler wants the server to run;
    /// see [`super::parse_range_event`]. Sent by @0x6f7db0, which holds the event pending (kind
    /// 10, the range's id as the argument, the player as the target) until a matching
    /// [`EVENT_START`]; the handler's script vets the range first (`OnPreWithinRange`).
    pub const RANGE_EVENT: u16 = 0x1c4;
    /// Up (0x28 bytes): the player left an event range a handler watches for leaving (the Limsa
    /// and Ul'dah openings' way out, `EVENT_OUTSIDE_RANGE`); sent by @0x6f8390, kind 11, the same
    /// layout as [`RANGE_EVENT`]. Seen: Ul'dah's ERANGE_SEQ_1_CLOSED_1 (4101587).
    pub const OUTSIDE_RANGE_EVENT: u16 = 0x1c5;
    /// Up: a scene of an event has ended; see [`super::parse_scene_result`]. Sent by @0xa50640:
    /// 0x1d7 with up to 2 values, 0x1d8 with 4, 0x1d9 with 8 (more through @0xa5d9c0..). Seen when
    /// the Gridania opening's scene 0 had played to its end: (0x130002, scene 0, one value 3).
    pub const SCENE_RESULT: u16 = 0x1d7;
    pub const SCENE_RESULT_4: u16 = 0x1d8;
    pub const SCENE_RESULT_8: u16 = 0x1d9;
    /// Down: the player's active quests, 30 entries of 10 bytes (case 0x69 → @0xa54690: quest id
    /// u16 @0, 8 bytes of its state @2). After all 30 the client marks its quests loaded
    /// (0x14a73d2) the first time; see [`super::quests`].
    pub const QUESTS: u16 = 0x1e0;
    /// Down: one quest slot (case 0x6a → @0xa50040: the slot u8 @0, its 10-byte entry @4, as in
    /// [`QUESTS`]); see [`super::quest`].
    pub const QUEST: u16 = 0x1e1;
    /// Down: the completed quests, 200 bytes of flags (case 0x6b → @0xa50160, copied to
    /// 0x14a875a). The first one marks them loaded (0x14a73d3) and, with the quests loaded too,
    /// has the event system refresh its objects (@0x6e5e50 → @0x6de320); until both have come,
    /// a quest-giver the client places from its layout stays unshown (Bertennant in the Gridania
    /// opening: in the client's object table, never drawn).
    pub const QUEST_COMPLETE_FLAGS: u16 = 0x1e2;
    /// Down: one quest's completed flag (case 0x6c → @0xa501b0: the quest's index u16 @0, set
    /// @2, refresh @3: with it the client refreshes its event objects, the quest's handler and its
    /// UI, @0x6e5ea0); see [`super::quest_complete_flag`].
    pub const QUEST_COMPLETE_FLAG: u16 = 0x1e3;
    /// Down: the player's conditions; see [`super::conditions`] (case 0x4c → @0xa4f150).
    pub const CONDITIONS: u16 = 0x1a3;
    /// Down: an event starts; see [`super::event_start`] (case 0x65 → @0x6f0be0).
    pub const EVENT_START: u16 = 0x1cc;
    /// Down: a scene of an event plays; see [`super::event_play`] (case 0x64 → @0xa50d10 →
    /// @0x6fcf60). 0x1c3 carries up to 2 parameters; 0x1c4-0x1ca 4, 8, 16, 32, 64, 128, 255.
    pub const EVENT_PLAY: u16 = 0x1c3;
    /// Down: an event ends and frees the player; see [`super::event_finish`] (case 0x66 →
    /// @0x6fd010).
    pub const EVENT_FINISH: u16 = 0x1cd;
    /// Up (0x30 bytes): the client's commands (Sapphire's ClientTrigger), built by
    /// `Global::SendClientTrigger` @0xa51200: the command @0, four u32 arguments @4; see
    /// [`super::trigger`] (`tools/re/triggers.py` lists every sender).
    pub const COMMAND: u16 = 0x191;
    pub const DISCOVERY: u16 = 0x194;
    /// Up (0x18 bytes): the player uses an action; see [`super::ActionRequest`] (sent by
    /// @0xa51de0).
    pub const ACTION_REQUEST: u16 = 0x196;
    /// Up (8 bytes, after Init): the linkshell list is wanted; not answered yet.
    pub const LINKSHELL_LIST: u16 = 0xf6;
    /// Down: a character goes away; see [`super::despawn`] (case 0x3d → @0xa64000).
    pub const DESPAWN: u16 = 0x191;
    /// Down: another character moves; see [`super::ActorMove`] (case 0x3e → @0xa55ae0, which
    /// queues the body under the segment's source; @0xa55030 plays the queue).
    pub const ACTOR_MOVE: u16 = 0x192;
    /// Down: an object that is not a character (a gathering point, an event object) appears in
    /// one of the client's 40 object slots; see [`super::create_object`] (case 0x46 →
    /// @0xa53340; the opcode's case read from the dispatcher's byte table @0xba6358).
    pub const CREATE_OBJECT: u16 = 0x19d;
    /// Down: the object in a slot goes away; see [`super::delete_object`] (case 0x47 → @0xa4ecc0).
    pub const DELETE_OBJECT: u16 = 0x19e;
}

/// The kinds of [`create_object`] (@0xa53340's switch on byte 1).
pub mod object {
    /// A gathering point: its id is its `GatheringPoint` row (@0xab2d80 looks it up at
    /// id - 30000); built by @0xae3230 (`GatheringPointObject`).
    pub const GATHERING_POINT: u8 = 6;
    /// An event object (@0xae3330).
    pub const EVENT_OBJECT: u8 = 7;
    /// How many object slots the client has (@0xae3230 takes 0-0x27).
    pub const SLOTS: u8 = 40;
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

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()))
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

/// [`opcode::PLAYER_STATUS`]'s body, as @0xa7f520 and @0xa7f9c0 read it (the fields this build
/// is known to read; the rest, unlock flags and the like from @0xd8 to @0x3d6, stay zero, as for a
/// new character):
///
/// ```text
/// 0x00  u64       content id
/// 0x08  u32       actor id
/// 0x0C  u8 x14    single bytes (race, tribe, sex, guardian, ... not yet all told apart):
///   0x0F  u8      the current class (@0xa7f520 → PlayerState +0x64, @0x14ab0d4)
///   0x10  u8      the class the character began as (→ PlayerState +0xde, @0x14ab14e): the
///                 quests' class condition compares it (@0x6d8b60: `Quest` col 16 = 3 needs col
///                 17 to equal it; Close to Home's three variants, 4/5/6, for Gridania's
///                 classes). Sapphire's PlayerStatus names the pair ClassJob, FirstClass.
/// 0x1A  u8        the current class (a change refreshes the class UI)
/// 0x34  char[64]  name
/// 0x66  u16 x19   level per class slot (ClassJob col 4)
/// 0x8C  u32 x19   experience per class slot
/// 0xD8  u8 x16    unlocks, bit (1 << id & 7) of byte id >> 3 (→ @0x14b4ec0; 1 Return, 4 Teleport)
/// 0xE8  u8 x12    aetherytes attuned, the same way by `Aetheryte` row (→ @0x14b4ed0, which
///                 the aethernet menu reads, @0x714f80)
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerStatus {
    pub content_id: u64,
    pub actor_id: u32,
    pub class: u8,
    /// The class the character was made with.
    pub first_class: u8,
    pub name: String,
    pub levels: [u16; 19],
    pub exp: [u32; 19],
    pub unlocks: [u8; 16],
    pub aetherytes: [u8; 12],
}

/// Ids as the client's bit arrays hold them: bit 1 << (id & 7) of byte id >> 3 (ids past the
/// array left out).
pub fn id_bits<const N: usize>(ids: &[u32]) -> [u8; N] {
    let mut bits = [0u8; N];
    for &id in ids {
        if let Some(b) = bits.get_mut((id >> 3) as usize) {
            *b |= 1 << (id & 7);
        }
    }
    bits
}

impl PlayerStatus {
    /// The reader goes as far as 0x3b2 + 0x24 (@0xa7f520's last loop).
    pub const SIZE: usize = 0x3e0;

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[0..8].copy_from_slice(&self.content_id.to_le_bytes());
        b[8..0xc].copy_from_slice(&self.actor_id.to_le_bytes());
        (b[0xf], b[0x10]) = (self.class, self.first_class);
        b[0x1a] = self.class;
        let n = self.name.len().min(0x3f);
        b[0x34..0x34 + n].copy_from_slice(&self.name.as_bytes()[..n]);
        for (i, l) in self.levels.iter().enumerate() {
            b[0x66 + 2 * i..0x68 + 2 * i].copy_from_slice(&l.to_le_bytes());
        }
        for (i, e) in self.exp.iter().enumerate() {
            b[0x8c + 4 * i..0x90 + 4 * i].copy_from_slice(&e.to_le_bytes());
        }
        b[0xd8..0xe8].copy_from_slice(&self.unlocks);
        b[0xe8..0xf4].copy_from_slice(&self.aetherytes);
        b.freeze()
    }
}

/// [`opcode::CLASS_INFO`] (0x10 bytes, as @0xa4ed60 reads it): the class @0, its level @2, the
/// level it plays at @4 (synced), its experience @8, and a u32 @0xc kept by the client.
pub fn class_info(class: u8, level: u16, synced: u16, exp: u32) -> Bytes {
    let mut b = BytesMut::zeroed(0x10);
    b[0] = class;
    b[2..4].copy_from_slice(&level.to_le_bytes());
    b[4..6].copy_from_slice(&synced.to_le_bytes());
    b[8..0xc].copy_from_slice(&exp.to_le_bytes());
    b.freeze()
}

/// The slots of [`base_params`]: each `BaseParam` row's column 0.
pub mod param {
    pub const STRENGTH: usize = 0;
    pub const PIETY: usize = 5;
    pub const HP: usize = 6;
    pub const MP: usize = 7;
    pub const TP: usize = 8;
    pub const GP: usize = 9;
    pub const CP: usize = 10;
}

/// [`opcode::BASE_PARAM`] (0xb8 bytes): a u32 per `BaseParam` slot (40), then six u32s @0xa0
/// the client keeps by number (1-6) (@0xa4ecf0).
pub fn base_params(values: &[u32; 40], extra: &[u32; 6]) -> Bytes {
    let mut b = BytesMut::zeroed(0xb8);
    for (i, v) in values.iter().chain(extra).enumerate() {
        b[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    }
    b.freeze()
}

/// An item in a container slot, as [`opcode::UPDATE_ITEM`] (@0xa4e610) and, queued,
/// [`opcode::NORMAL_ITEM`] carry it (0x40 bytes; Sapphire's ItemInfo):
///
/// ```text
/// 0x00  u32  sequence (the container batch it belongs to)
/// 0x08  u16  container        0x0A  u16  slot
/// 0x0C  u32  quantity         0x10  u32  item id (Item row)
/// 0x18  u64  signature (crafter's content id)
/// 0x20  u8   high quality     0x22  u16  condition (30000 is 100%)
/// 0x24  u16  spiritbond       0x26  u8   dye (Stain row)
/// 0x28  u32  glamour item     0x2C  u16 x5  materia, 0x36 u8 x5 their grades (not yet told apart)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ItemInfo {
    pub sequence: u32,
    pub container: u16,
    pub slot: u16,
    pub quantity: u32,
    pub item_id: u32,
    pub signature: u64,
    pub hq: bool,
    pub condition: u16,
    pub spiritbond: u16,
    pub stain: u8,
    pub glamour: u32,
}

impl ItemInfo {
    pub const SIZE: usize = 0x40;
    /// A new item's condition.
    pub const FULL_CONDITION: u16 = 30000;

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[0..4].copy_from_slice(&self.sequence.to_le_bytes());
        b[8..0xa].copy_from_slice(&self.container.to_le_bytes());
        b[0xa..0xc].copy_from_slice(&self.slot.to_le_bytes());
        b[0xc..0x10].copy_from_slice(&self.quantity.to_le_bytes());
        b[0x10..0x14].copy_from_slice(&self.item_id.to_le_bytes());
        b[0x18..0x20].copy_from_slice(&self.signature.to_le_bytes());
        b[0x20] = self.hq as u8;
        b[0x22..0x24].copy_from_slice(&self.condition.to_le_bytes());
        b[0x24..0x26].copy_from_slice(&self.spiritbond.to_le_bytes());
        b[0x26] = self.stain;
        b[0x28..0x2c].copy_from_slice(&self.glamour.to_le_bytes());
        b.freeze()
    }

    pub fn parse(b: &[u8]) -> Option<Self> {
        let b = b.get(..Self::SIZE)?;
        let u16_at = |at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
        Some(Self {
            sequence: u32_at(b, 0)?,
            container: u16_at(8),
            slot: u16_at(0xa),
            quantity: u32_at(b, 0xc)?,
            item_id: u32_at(b, 0x10)?,
            signature: u64::from_le_bytes(b[0x18..0x20].try_into().unwrap()),
            hq: b[0x20] != 0,
            condition: u16_at(0x22),
            spiritbond: u16_at(0x24),
            stain: b[0x26],
            glamour: u32_at(b, 0x28)?,
        })
    }
}

/// [`opcode::ITEM_OPERATION_REQUEST`]'s body (0x30 bytes, as @0xa51580 fills it; Sapphire's
/// ClientInventoryItemOperation): the context id @0, the operation @4, then the source and the
/// destination: actor id @8/@0x1c, container @0xc/@0x20, slot (i16) @0x10/@0x24, stack
/// @0x14/@0x28, item @0x18/@0x2c.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ItemOperation {
    pub context: u32,
    pub operation: u8,
    pub src_container: u32,
    pub src_slot: i16,
    pub src_stack: u32,
    pub src_item: u32,
    pub dst_container: u32,
    pub dst_slot: i16,
    pub dst_stack: u32,
    pub dst_item: u32,
}

impl ItemOperation {
    /// Moves (or swaps, or merges) the source onto the destination; the only operation with both
    /// (@0xa51580). 0x0e-0x12, 0x16, 0x17 and 0x2b name the source only.
    pub const MOVE: u8 = 8;

    pub fn parse(b: &[u8]) -> Option<Self> {
        let b = b.get(..0x30)?;
        let i16_at = |at: usize| i16::from_le_bytes([b[at], b[at + 1]]);
        Some(Self {
            context: u32_at(b, 0)?,
            operation: b[4],
            src_container: u32_at(b, 0xc)?,
            src_slot: i16_at(0x10),
            src_stack: u32_at(b, 0x14)?,
            src_item: u32_at(b, 0x18)?,
            dst_container: u32_at(b, 0x20)?,
            dst_slot: i16_at(0x24),
            dst_stack: u32_at(b, 0x28)?,
            dst_item: u32_at(b, 0x2c)?,
        })
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(0x30);
        b[0..4].copy_from_slice(&self.context.to_le_bytes());
        b[4] = self.operation;
        b[0xc..0x10].copy_from_slice(&self.src_container.to_le_bytes());
        b[0x10..0x12].copy_from_slice(&self.src_slot.to_le_bytes());
        b[0x14..0x18].copy_from_slice(&self.src_stack.to_le_bytes());
        b[0x18..0x1c].copy_from_slice(&self.src_item.to_le_bytes());
        b[0x20..0x24].copy_from_slice(&self.dst_container.to_le_bytes());
        b[0x24..0x26].copy_from_slice(&self.dst_slot.to_le_bytes());
        b[0x28..0x2c].copy_from_slice(&self.dst_stack.to_le_bytes());
        b[0x2c..0x30].copy_from_slice(&self.dst_item.to_le_bytes());
        b.freeze()
    }
}

/// [`opcode::ITEM_OPERATION_BATCH`] (0x10 bytes; Sapphire's ItemOperationBatch): the context id
/// @0, an operation id @4, the operation @8, an error @9 (0 none), and how many
/// [`opcode::ITEM_OPERATION`] packets belong to it @0xa.
pub fn item_operation_batch(context: u32, operation: u8, error: u8, packets: u8) -> Bytes {
    let mut b = BytesMut::zeroed(0x10);
    b[0..4].copy_from_slice(&context.to_le_bytes());
    (b[8], b[9], b[0xa]) = (operation, error, packets);
    b.freeze()
}

/// [`opcode::EQUIP`]'s body (0x40 bytes, as @0xaa9680 reads it): the main weapon's model u64 @0,
/// the off hand's @8, a flag @0x10 (kept by the character @+0x1680; 0 so far), and the ten gear
/// models @0x14, in the spawn's order.
pub fn equip(main_weapon: u64, sub_weapon: u64, models: &[u32; 10]) -> Bytes {
    let mut b = BytesMut::zeroed(0x40);
    b[0..8].copy_from_slice(&main_weapon.to_le_bytes());
    b[8..0x10].copy_from_slice(&sub_weapon.to_le_bytes());
    for (i, m) in models.iter().enumerate() {
        b[0x14 + 4 * i..0x18 + 4 * i].copy_from_slice(&m.to_le_bytes());
    }
    b.freeze()
}

/// The weapons and models of an [`equip`] body.
pub fn parse_equip(b: &[u8]) -> Option<(u64, u64, [u32; 10])> {
    let b = b.get(..0x3c)?;
    let mut models = [0u32; 10];
    for (i, m) in models.iter_mut().enumerate() {
        *m = u32_at(b, 0x14 + 4 * i)?;
    }
    Some((u64::from_le_bytes(b[0..8].try_into().ok()?), u64::from_le_bytes(b[8..0x10].try_into().ok()?), models))
}

/// [`opcode::HUD_PARAM`] (0x17c bytes, as @0xa555b0 reads it; the spawn's fields): class @0,
/// level @1, HP u32 @4, max HP @8, MP u16 @0xc and max @0xe (GP or CP for gatherers and crafters,
/// by ClassJobCategory), TP @0x10, then 30 statuses of 12 bytes @0x14 (id u16, param u16, time
/// f32, source u32). The client replaces all 30 of the character's with each (@0xb7b840 per slot;
/// its own character's through @0xb7b8e0, unless @0xa9f620 says otherwise), so each carries all
/// of them.
#[allow(clippy::too_many_arguments)]
pub fn hud_param(
    class: u8,
    level: u8,
    hp: u32,
    hp_max: u32,
    mp: u16,
    mp_max: u16,
    tp: u16,
    statuses: &[StatusEntry],
) -> Bytes {
    let mut b = BytesMut::zeroed(0x17c);
    (b[0], b[1]) = (class, level);
    b[4..8].copy_from_slice(&hp.to_le_bytes());
    b[8..0xc].copy_from_slice(&hp_max.to_le_bytes());
    b[0xc..0xe].copy_from_slice(&mp.to_le_bytes());
    b[0xe..0x10].copy_from_slice(&mp_max.to_le_bytes());
    b[0x10..0x12].copy_from_slice(&tp.to_le_bytes());
    write_statuses(&mut b[0x14..], statuses);
    b.freeze()
}

/// The most statuses a character has: the client's slots.
pub const STATUS_SLOTS: usize = 30;

/// One status slot of [`hud_param`] and [`status_list`] (12 bytes): the Status row u16 @0, its
/// parameter u16 @2, the seconds left f32 @4, who put it there u32 @8 (@0xa555b0 hands
/// @0xb7b840 them as id, time, param, source). An id of 0 is an empty slot.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StatusEntry {
    pub id: u16,
    pub param: u16,
    pub remaining: f32,
    pub source: u32,
}

impl StatusEntry {
    pub const SIZE: usize = 12;
}

/// `statuses` into the slots of `b` (the rest left empty; more than [`STATUS_SLOTS`] left out).
fn write_statuses(b: &mut [u8], statuses: &[StatusEntry]) {
    for (s, slot) in statuses.iter().take(STATUS_SLOTS).zip(b.as_chunks_mut::<{ StatusEntry::SIZE }>().0) {
        slot[0..2].copy_from_slice(&s.id.to_le_bytes());
        slot[2..4].copy_from_slice(&s.param.to_le_bytes());
        slot[4..8].copy_from_slice(&s.remaining.to_le_bytes());
        slot[8..12].copy_from_slice(&s.source.to_le_bytes());
    }
}

/// The statuses in the slots of `b`, empty slots left out.
fn read_statuses(b: &[u8]) -> Vec<StatusEntry> {
    b.as_chunks::<{ StatusEntry::SIZE }>()
        .0
        .iter()
        .take(STATUS_SLOTS)
        .map(|s| StatusEntry {
            id: u16::from_le_bytes([s[0], s[1]]),
            param: u16::from_le_bytes([s[2], s[3]]),
            remaining: f32::from_le_bytes([s[4], s[5], s[6], s[7]]),
            source: u32::from_le_bytes([s[8], s[9], s[10], s[11]]),
        })
        .filter(|s| s.id != 0)
        .collect()
}

/// [`opcode::STATUS`]'s body (0x168 bytes): the player's 30 status slots as [`hud_param`]'s.
/// @0xb7ba00 copies each into the character, and takes off the effects of the ids that are no
/// longer there.
pub fn status_list(statuses: &[StatusEntry]) -> Bytes {
    let mut b = BytesMut::zeroed(STATUS_SLOTS * StatusEntry::SIZE);
    write_statuses(&mut b, statuses);
    b.freeze()
}

/// The statuses of a [`hud_param`] body.
pub fn parse_hud_statuses(b: &[u8]) -> Vec<StatusEntry> {
    read_statuses(b.get(0x14..).unwrap_or_default())
}

/// The statuses of a [`status_list`] body.
pub fn parse_status_list(b: &[u8]) -> Vec<StatusEntry> {
    read_statuses(b)
}

/// The HP and max HP of a [`hud_param`] body.
pub fn parse_hud_param(b: &[u8]) -> Option<(u32, u32)> {
    Some((u32_at(b, 4)?, u32_at(b, 8)?))
}

/// [`opcode::ORDER`]'s categories.
pub mod order_kind {
    /// The character died: arg 1 the killer (0xe0000000: none), arg 2 kept with it (@0xa55f80
    /// case 6, which writes the defeat lines to the log).
    pub const DEATH: u16 = 6;
    /// A cast ended before it landed (@0xa55f80 case 0xf): arg 1 the LogMessage row it logs,
    /// arg 2 the action's kind (1: an Action row), arg 3 the action, arg 4 1 when it was
    /// interrupted. The player's own cast bar stops (@0xac2f90 on the cast @0x14bcf40, as
    /// RequestCast's), the character's cast too (@0xaa3360).
    pub const CAST_END: u16 = 0xf;
    /// An unlock set (arg 2 non-zero) or cleared: arg 1 its id, the player's bit @0x14b4ec0
    /// (@0xa55f80 case 0x29 → @0xa6e020; Sapphire's SetRewardFlag, 1 Return, 4 Teleport).
    pub const SET_UNLOCK: u16 = 0x29;
    /// Attuned (arg 2 non-zero) to the `Aetheryte` row arg 1: the player's bit @0x14b4ed0
    /// (@0xa55f80 case 0xbb → @0xa69ac0; Sapphire's LearnTeleport).
    pub const LEARN_AETHERYTE: u16 = 0x1fd;
}

/// LogMessage rows a cast's end logs.
pub mod log_message {
    /// "<you> cancel(s) <action>." (row 537, 0x219)
    pub const CAST_CANCELLED: u32 = 537;
    /// "<actor>'s <action> is interrupted." (row 540)
    pub const CAST_INTERRUPTED: u32 = 540;
}

/// [`opcode::ORDER`]'s body (0x18 bytes): the category u16 @0 and four u32 arguments @4 (the
/// dispatcher passes @0, @4, @8, @0xc, @0x10).
pub fn order(category: u16, args: [u32; 4]) -> Bytes {
    let mut b = BytesMut::zeroed(0x18);
    b[0..2].copy_from_slice(&category.to_le_bytes());
    for (i, a) in args.iter().enumerate() {
        b[4 + 4 * i..8 + 4 * i].copy_from_slice(&a.to_le_bytes());
    }
    b.freeze()
}

/// The category and arguments of an [`order`] body.
pub fn parse_order(b: &[u8]) -> Option<(u16, [u32; 4])> {
    let c = u16::from_le_bytes(b.get(..2)?.try_into().ok()?);
    Some((c, [u32_at(b, 4)?, u32_at(b, 8)?, u32_at(b, 0xc)?, u32_at(b, 0x10)?]))
}

/// One effect of an action on its target (8 bytes; Sapphire's older EffectEntry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Effect {
    pub kind: u8,
    pub severity: u8,
    pub param: u8,
    pub bonus: u8,
    pub multiplier: u8,
    pub flags: u8,
    pub value: u16,
}

impl Effect {
    pub const DAMAGE: u8 = 3;
    pub const HEAL: u8 = 4;

    pub fn damage(amount: u16) -> Self {
        Self { kind: Self::DAMAGE, value: amount, ..Default::default() }
    }

    /// HP restored (Sapphire's kind 4; not yet seen played by this client).
    pub fn heal(amount: u16) -> Self {
        Self { kind: Self::HEAL, value: amount, ..Default::default() }
    }
}

/// [`opcode::REQUEST_CAST`]'s body (0x20 bytes), as @0xa52d10 reads it, sent as the caster:
///
/// ```text
/// 0x00  u16  action (Action row)  0x02  u8   kind (1: an Action row; the log's line by its category)
/// 0x04  u32  the action again
/// 0x08  f32  cast time (s): the player's own cast bar (@0xac3e10), the others' (@0xac3dd0)
/// 0x0C  u32  target (0xE0000000: none)
/// 0x10  f32  rotation (@0xaa46b0)
/// 0x14  u32  and 0x18 3 x u16: a ground target's place (category 0xb only)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RequestCast {
    pub action: u16,
    pub kind: u8,
    pub cast_time: f32,
    pub target: u32,
    pub rotation: f32,
}

impl RequestCast {
    pub const SIZE: usize = 0x20;

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[0..2].copy_from_slice(&self.action.to_le_bytes());
        b[2] = self.kind;
        b[4..8].copy_from_slice(&(self.action as u32).to_le_bytes());
        b[8..0xc].copy_from_slice(&self.cast_time.to_le_bytes());
        b[0xc..0x10].copy_from_slice(&self.target.to_le_bytes());
        b[0x10..0x14].copy_from_slice(&self.rotation.to_le_bytes());
        b.freeze()
    }

    pub fn parse(b: &[u8]) -> Option<Self> {
        let b = b.get(..Self::SIZE)?;
        Some(Self {
            action: u16::from_le_bytes([b[0], b[1]]),
            kind: b[2],
            cast_time: f32::from_le_bytes(b[8..0xc].try_into().ok()?),
            target: u32_at(b, 0xc)?,
            rotation: f32::from_le_bytes(b[0x10..0x14].try_into().ok()?),
        })
    }
}

/// [`opcode::ACTION_RESULT1`]'s body (0x64 bytes), as @0xa4e520 widens it for @0xac9b30:
///
/// ```text
/// 0x00  u16  action (Action row; 7 is the auto-attack)
/// 0x02  u8   variation          0x03  u8   kind (1: an action)
/// 0x04  u32  the action again (its key)
/// 0x08  u32  the player's sequence for its own actions (0: the server's doing)
/// 0x0C  f32  animation lock (s)
/// 0x10  u32  target
/// 0x14  u16  rotation over the full turn
/// 0x18  u64  the target the animation plays on
/// 0x24  8 x 8 effects on it
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ActionResult {
    pub action: u16,
    pub kind: u8,
    pub sequence: u32,
    pub lock: f32,
    pub target: u32,
    pub rotation: f32,
    pub effects: [Effect; 8],
}

impl ActionResult {
    pub const SIZE: usize = 0x64;
    pub const AUTO_ATTACK: u16 = 7;

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[0..2].copy_from_slice(&self.action.to_le_bytes());
        b[3] = self.kind;
        b[4..8].copy_from_slice(&(self.action as u32).to_le_bytes());
        b[8..0xc].copy_from_slice(&self.sequence.to_le_bytes());
        b[0xc..0x10].copy_from_slice(&self.lock.to_le_bytes());
        b[0x10..0x14].copy_from_slice(&self.target.to_le_bytes());
        let turn = ((self.rotation + std::f32::consts::PI) / std::f32::consts::TAU * 65535.0).round();
        b[0x14..0x16].copy_from_slice(&(turn.clamp(0.0, 65535.0) as u16).to_le_bytes());
        b[0x18..0x20].copy_from_slice(&(self.target as u64).to_le_bytes());
        for (i, e) in self.effects.iter().enumerate() {
            let at = 0x24 + 8 * i;
            b[at..at + 6].copy_from_slice(&[e.kind, e.severity, e.param, e.bonus, e.multiplier, e.flags]);
            b[at + 6..at + 8].copy_from_slice(&e.value.to_le_bytes());
        }
        b.freeze()
    }

    /// The action, target and first effect of a body.
    pub fn parse(b: &[u8]) -> Option<(u16, u32, Effect)> {
        let b = b.get(..Self::SIZE)?;
        let e = &b[0x24..0x2c];
        let effect = Effect {
            kind: e[0],
            severity: e[1],
            param: e[2],
            bonus: e[3],
            multiplier: e[4],
            flags: e[5],
            value: u16::from_le_bytes([e[6], e[7]]),
        };
        Some((u16::from_le_bytes([b[0], b[1]]), u32_at(b, 0x10)?, effect))
    }
}

/// [`opcode::ACTION_REQUEST`]'s body (0x18 bytes, as @0xa51de0 fills it):
///
/// ```text
/// 0x00  u8   exec proc        0x01  u8   action kind (1: an Action row)
/// 0x04  u32  action
/// 0x08  u32  the request's sequence: the ActionResult echoes it, which ends the client's lock
/// 0x0C  u16  rotation         0x0E  u16  rotation to the target
/// 0x10  u64  target
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActionRequest {
    pub kind: u8,
    pub action: u32,
    pub sequence: u32,
    pub target: u64,
}

impl ActionRequest {
    pub const SIZE: usize = 0x18;

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::zeroed(Self::SIZE);
        b[1] = self.kind;
        b[4..8].copy_from_slice(&self.action.to_le_bytes());
        b[8..0xc].copy_from_slice(&self.sequence.to_le_bytes());
        b[0x10..0x18].copy_from_slice(&self.target.to_le_bytes());
        b.freeze()
    }

    pub fn parse(b: &[u8]) -> Option<Self> {
        let b = b.get(..Self::SIZE)?;
        Some(Self {
            kind: b[1],
            action: u32_at(b, 4)?,
            sequence: u32_at(b, 8)?,
            target: u64::from_le_bytes(b[0x10..0x18].try_into().ok()?),
        })
    }
}

/// [`opcode::ITEM_SIZE`] (0x10 bytes): closes the batch `sequence` of `count` items of `container`.
pub fn container_info(sequence: u32, count: u32, container: u16) -> Bytes {
    let mut b = BytesMut::zeroed(0x10);
    b[0..4].copy_from_slice(&sequence.to_le_bytes());
    b[4..8].copy_from_slice(&count.to_le_bytes());
    b[8..0xc].copy_from_slice(&(container as u32).to_le_bytes());
    b.freeze()
}

/// [`opcode::COMMAND`]'s commands, as the client's senders use them.
pub mod trigger {
    /// The weapon drawn (arg 1 = 1) or sheathed (0) (@0xa7ef20).
    pub const WEAPON: u32 = 0x00;
    /// Auto-attack on (arg 1 = 1) or off, at the target in arg 2 (@0xa7ede0; the client draws
    /// the weapon first if needed).
    pub const AUTO_ATTACK: u32 = 0x01;
    /// The target changed: its actor id in arg 1 (0xe0000000: none) (@0x5326b0).
    pub const TARGET: u32 = 0x02;
    /// Get up after dying (`Client::Game::UI::Revive` @0xa6d8d0 sends it with arg 1 = 8 or 5:
    /// the dialog's choices, not yet told apart).
    pub const REVIVE: u32 = 0x0d;
    /// The id the client uses for no target.
    pub const NO_TARGET: u32 = 0xe000_0000;

    /// The command and its four arguments.
    pub fn parse(b: &[u8]) -> Option<(u32, [u32; 4])> {
        let u = |at: usize| b.get(at..at + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap()));
        Some((u(0)?, [u(4)?, u(8)?, u(0xc)?, u(0x10)?]))
    }
}

/// Event handler types: an event id's top 16 bits.
pub mod event {
    pub const QUEST: u16 = 0x0001;
    pub const WARP: u16 = 0x0002;
    /// An aetheryte or aethernet shard (its `Aetheryte` row in the low 16 bits; the client's
    /// `game_script/transport/Aetheryte` runs its scenes).
    pub const AETHERYTE: u16 = 0x0005;
    /// The opening's handlers (the `Opening` sheet's rows, 0x130001-0x130003).
    pub const OPENING: u16 = 0x0013;
    /// The event kind `EVENT_START` and `EVENT_FINISH` carry for talking.
    pub const TALK: u8 = 1;
    /// The kind of an event started inside another: the client starts it while one runs
    /// (@0x6f0be0 lets kind 7 past its running-event check; Sapphire's Nest).
    pub const NEST: u8 = 7;
    /// The kind of an event an event range starts (@0x6f7db0 sets 10).
    pub const WITHIN_RANGE: u8 = 10;
    /// The kind of an event leaving an event range starts (@0x6f8390 sets 11).
    pub const OUTSIDE_RANGE: u8 = 11;
    /// The kind the client gives an event a territory's handlers offer on entering it
    /// (@0x70dfa0 sets 0xf before asking them).
    pub const ENTER_TERRITORY: u8 = 0xf;
}

/// [`opcode::RANGE_EVENT`]'s range id @0 (the range object's +0x14), event id @4 and the player's
/// position @8.
pub fn parse_range_event(b: &[u8]) -> Option<(u32, u32, [f32; 3])> {
    let f = |at: usize| u32_at(b, at).map(f32::from_bits);
    Some((u32_at(b, 0)?, u32_at(b, 4)?, [f(8)?, f(0xc)?, f(0x10)?]))
}

/// [`opcode::EVENT_REQUEST`]'s event id.
pub fn parse_event_request(b: &[u8]) -> Option<u32> {
    u32_at(b, 0)
}

/// [`opcode::CONDITIONS`]: one bit per condition (byte `id >> 3`, bit `id & 7`). The client sets
/// each of its conditions 9-0x2d from its bit (@0xa4f150 → @0xa4a760, a byte per condition at
/// 0x14a7360), except 0xf while an event runs, 0x20 and 0x29; conditions below 9 it keeps itself.
/// An event the client asks for on entering a territory leaves one set (0x10, read in the client
/// after the Gridania opening ended) that its event's end does not clear (@0x6f8f10 clears only
/// those below 9), so the server clears it with this.
pub fn conditions(set: &[u8]) -> Bytes {
    let mut b = BytesMut::zeroed(8);
    for &id in set {
        b[(id >> 3) as usize] |= 1 << (id & 7);
    }
    b.freeze()
}

/// One quest slot as the client keeps it (@0xa50040): its id, sequence and six variable bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuestSlot {
    pub slot: u8,
    pub quest_id: u32,
    pub sequence: u8,
    /// `UI8A`-`UI8F` (slot +8 - +0xd: the scripts' `GetQuestUI8A`.. @0x734ad0.., the `H`/`L`
    /// nibbles, `BitFlag8` in F).
    pub vars: [u8; 6],
}

/// One quest slot's 10 bytes as [`opcode::QUESTS`] and [`opcode::QUEST`] carry them: the id's low
/// 16 bits @0 (the client compares them as a short: 39 for quest 0x10027), its sequence @2 (slot
/// +6, `GetQuestSequence` @0x6cc000), a byte @3 (slot +7, left 0) and the variables @4-@9.
fn quest_entry(b: &mut [u8], q: &QuestSlot) {
    b[0..2].copy_from_slice(&(q.quest_id as u16).to_le_bytes());
    b[2] = q.sequence;
    b[4..10].copy_from_slice(&q.vars);
}

/// [`opcode::QUESTS`]: the 30 slots; the rest empty.
pub fn quests(active: &[QuestSlot]) -> Bytes {
    let mut b = BytesMut::zeroed(30 * 10);
    for q in active.iter().filter(|q| q.slot < 30) {
        let at = q.slot as usize * 10;
        quest_entry(&mut b[at..at + 10], q);
    }
    b.freeze()
}

/// [`opcode::QUEST`]: one slot's quest (quest id 0 empties the slot).
pub fn quest(q: &QuestSlot) -> Bytes {
    let mut b = BytesMut::zeroed(0x10);
    b[0] = q.slot;
    quest_entry(&mut b[4..0xe], q);
    b.freeze()
}

/// [`opcode::QUEST_COMPLETE_FLAGS`]: 200 bytes, a bit per completed quest's index (its id's low 16
/// bits): byte index >> 3, bit 0x80 >> (index & 7) (@0xa501b0 reads them so).
pub fn quest_complete_flags(completed: &[u32]) -> Bytes {
    let mut b = [0u8; 200];
    for &q in completed {
        let index = (q & 0xffff) as usize;
        if index >> 3 < 200 {
            b[index >> 3] |= 0x80 >> (index & 7);
        }
    }
    Bytes::copy_from_slice(&b)
}

/// [`opcode::QUEST_COMPLETE_FLAG`]: one quest's flag set or cleared, with the client's refresh.
pub fn quest_complete_flag(quest_id: u32, complete: bool) -> Bytes {
    let mut b = BytesMut::zeroed(8);
    b[0..2].copy_from_slice(&(quest_id as u16).to_le_bytes());
    (b[2], b[3]) = (complete as u8, 1);
    b.freeze()
}

/// A scene's end, as [`opcode::SCENE_RESULT`] carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneResult {
    pub event_id: u32,
    pub scene: u16,
    /// The byte @6 (@0xa50640's third argument): 0 so far; meaning unknown.
    pub byte_6: u8,
    /// The values the scene returned (count @7, u32 each from @8).
    pub values: Vec<u32>,
}

pub fn parse_scene_result(b: &[u8]) -> Option<SceneResult> {
    let count = *b.get(7)? as usize;
    let values = (0..count).map(|i| u32_at(b, 8 + i * 4)).collect::<Option<Vec<_>>>()?;
    Some(SceneResult { event_id: u32_at(b, 0)?, scene: u16_at(b, 4)?, byte_6: *b.get(6)?, values })
}

/// [`opcode::EVENT_PLAY`] with up to 2 parameters (the dispatcher passes @0, @4, @8, @0xc as
/// u16, @0x10, @0x18 as the parameters' address, @0x14 as their count): the target u64 @0, the
/// event id @8, the scene @0xc, flags @0x10, the parameter count @0x14, the parameters u32 @0x18.
pub fn event_play(target: u64, event_id: u32, scene: u16, flags: u32, params: &[u32]) -> Bytes {
    assert!(params.len() <= 2, "EVENT_PLAY (0x1c3) carries 2 parameters at most");
    let mut b = BytesMut::zeroed(0x20);
    b[0..8].copy_from_slice(&target.to_le_bytes());
    b[8..0xc].copy_from_slice(&event_id.to_le_bytes());
    b[0xc..0xe].copy_from_slice(&scene.to_le_bytes());
    b[0x10..0x14].copy_from_slice(&flags.to_le_bytes());
    b[0x14] = params.len() as u8;
    for (i, p) in params.iter().enumerate() {
        b[0x18 + i * 4..0x1c + i * 4].copy_from_slice(&p.to_le_bytes());
    }
    b.freeze()
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
/// its kind @4, the result @5 (1 frees the player), an argument @8. Event, kind and argument must
/// be the start's: @0x6fd010 finds the running event by them (entry +8, +0x18, +0x1c) and writes
/// the result to its +0x28, through a null entry when none matches (an access violation).
pub fn event_finish(event_id: u32, kind: u8, result: u8, arg: u32) -> Bytes {
    let mut b = BytesMut::zeroed(0x10);
    b[0..4].copy_from_slice(&event_id.to_le_bytes());
    (b[4], b[5]) = (kind, result);
    b[8..0xc].copy_from_slice(&arg.to_le_bytes());
    b.freeze()
}

/// [`opcode::CREATE_OBJECT`]'s body, 0x38 bytes, as @0xa53340 reads it for a gathering point:
///
/// ```text
/// 0x00  u8      slot (0-39)
/// 0x01  u8      kind (object::GATHERING_POINT, object::EVENT_OBJECT)
/// 0x04  u32     id (a gathering point's GatheringPoint row)
/// 0x08  u32     (to the object, @0xa5f450; left 0)
/// 0x0c  u32     (to the object, @0xa5f430; an event object's layout id; left 0)
/// 0x22  u16     rotation (u16 / 65535 * 2pi - pi)
/// 0x30  u16 x3  position (see coordinate_to_u16)
/// ```
pub fn create_object(slot: u8, kind: u8, id: u32, rotation: f32, pos: [f32; 3]) -> Bytes {
    let mut b = BytesMut::zeroed(0x38);
    (b[0], b[1]) = (slot, kind);
    b[4..8].copy_from_slice(&id.to_le_bytes());
    let turn = ((rotation + std::f32::consts::PI) / std::f32::consts::TAU * 65535.0).round();
    b[0x22..0x24].copy_from_slice(&(turn.clamp(0.0, 65535.0) as u16).to_le_bytes());
    for (i, v) in pos.iter().enumerate() {
        b[0x30 + 2 * i..0x32 + 2 * i].copy_from_slice(&coordinate_to_u16(*v).to_le_bytes());
    }
    b.freeze()
}

/// [`opcode::DELETE_OBJECT`]'s body: the slot @0.
pub fn delete_object(slot: u8) -> Bytes {
    let mut b = BytesMut::zeroed(8);
    b[0] = slot;
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
/// 0x04  u8      speed, tenths of a yalm a second (the client divides by 10.0: 60 runs at 6)
/// 0x06  u16 x3  position (see coordinate_to_u16)
/// ```
///
/// The client plays a move over the time since that actor's previous one (@0xae90c0) and snaps
/// instead of animating when that time is not positive, or when the step is 12 yalms or more at
/// over four times the speed (@0xafc4f0: 4.0 @0x1094788). A character whose +0x8e bit 0x40 is
/// set (`GameObject::vf21` @0xa5ff20) skips all that and is placed directly: players' bots on
/// this server do that (they snap), so something sets the bit for them; which caller does is
/// for a breakpoint trace (`tools/re/bptrace.py` on @0xa5ff20) with the client running.
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
    fn gathering_point_object_layout() {
        let b = create_object(3, object::GATHERING_POINT, 30005, 0.0, [13.1, 0.6, -35.9]);
        assert_eq!(b.len(), 0x38);
        assert_eq!((b[0], b[1]), (3, 6));
        assert_eq!(u32_at(&b, 4), Some(30005));
        // rotation 0 is the middle of the turn
        assert_eq!(u16_at(&b, 0x22), Some(32768));
        let x = coordinate_from_u16(u16_at(&b, 0x30).unwrap());
        let z = coordinate_from_u16(u16_at(&b, 0x34).unwrap());
        assert!((x - 13.1).abs() < 0.02 && (z + 35.9).abs() < 0.02, "{x} {z}");
        assert_eq!(&delete_object(3)[..1], &[3]);
    }

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
    fn player_status_layout() {
        let mut levels = [0; 19];
        levels[1] = 1;
        let s = PlayerStatus {
            content_id: 0x0040_0000_0000_0001,
            actor_id: 0x1000_0001,
            class: 1,
            first_class: 5,
            name: "Null Crystal".into(),
            levels,
            ..Default::default()
        }
        .to_bytes();
        assert_eq!((s[0xf], s[0x10]), (1, 5));
        // Gridania's aetheryte (2) and its first aethernet shard (25)
        let bits: [u8; 12] = id_bits(&[2, 25]);
        assert_eq!((bits[0], bits[3]), (0x04, 0x02));
        assert_eq!(s.len(), PlayerStatus::SIZE);
        assert_eq!(&s[8..0xc], &0x1000_0001u32.to_le_bytes());
        assert_eq!(s[0x1a], 1);
        assert_eq!(&s[0x34..0x40], b"Null Crystal");
        assert_eq!(&s[0x68..0x6a], &1u16.to_le_bytes(), "slot 1, the gladiator's");
    }

    #[test]
    fn item_layouts() {
        let item = ItemInfo {
            sequence: 7,
            container: 1000,
            slot: 0,
            quantity: 1,
            item_id: 1601,
            hq: true,
            condition: ItemInfo::FULL_CONDITION,
            stain: 3,
            ..Default::default()
        };
        let b = item.to_bytes();
        assert_eq!(b.len(), 0x40);
        // where @0xa4e610 reads them
        assert_eq!(&b[8..0xc], &[0xe8, 0x03, 0, 0]);
        assert_eq!(&b[0x10..0x14], &1601u32.to_le_bytes());
        assert_eq!(&b[0x22..0x24], &30000u16.to_le_bytes());
        assert_eq!((b[0x20], b[0x26]), (1, 3));
        assert_eq!(ItemInfo::parse(&b), Some(item));
        let c = container_info(7, 1, 1000);
        assert_eq!(&c[..], &[7, 0, 0, 0, 1, 0, 0, 0, 0xe8, 3, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn item_operations() {
        // as @0xa51580 fills its buffer: bag 0 slot 3 (a stack of 5) onto equipped slot 0
        let op = ItemOperation {
            context: 0x1000_0001,
            operation: ItemOperation::MOVE,
            src_container: 0,
            src_slot: 3,
            src_stack: 5,
            dst_container: 1000,
            dst_slot: 0,
            ..Default::default()
        };
        let b = op.to_bytes();
        assert_eq!((b.len(), b[4]), (0x30, 8));
        assert_eq!(&b[0x10..0x12], &3i16.to_le_bytes());
        assert_eq!(&b[0x20..0x24], &1000u32.to_le_bytes());
        assert_eq!(ItemOperation::parse(&b), Some(op));
        let batch = item_operation_batch(0x1000_0001, 8, 0, 0);
        assert_eq!(&batch[..], &[1, 0, 0, 0x10, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn combat_layouts() {
        let statuses = [
            StatusEntry { id: 50, param: 0, remaining: 10.0, source: 0x1000_0001 },
            StatusEntry { id: 3, param: 2, remaining: 4.5, source: 0x5020_4001 },
        ];
        let hud = hud_param(1, 10, 80, 114, 25, 25, 1000, &statuses);
        assert_eq!(hud.len(), 0x17c);
        assert_eq!((hud[0], hud[1]), (1, 10));
        assert_eq!(parse_hud_param(&hud), Some((80, 114)));
        // the second slot @0x20: id, param, time, source
        assert_eq!(&hud[0x20..0x24], &[3, 0, 2, 0]);
        assert_eq!(&hud[0x24..0x28], &4.5f32.to_le_bytes());
        assert_eq!(&hud[0x28..0x2c], &0x5020_4001u32.to_le_bytes());
        assert_eq!(parse_hud_statuses(&hud), statuses);
        let list = status_list(&statuses);
        assert_eq!(list.len(), 0x168);
        assert_eq!(&list[..], &hud[0x14..]);
        assert_eq!(parse_status_list(&list), statuses);
        // past the 30 slots: left out
        assert_eq!(parse_status_list(&status_list(&[statuses[0]; 31])).len(), 30);
        // a cast: the action twice, the time @8, the target @0xc
        let cast = RequestCast { action: 120, kind: 1, cast_time: 2.0, target: 0x1000_0001, rotation: 0.5 };
        let b = cast.to_bytes();
        assert_eq!(b.len(), 0x20);
        assert_eq!((&b[0..4], &b[4..8]), (&[120, 0, 1, 0][..], &120u32.to_le_bytes()[..]));
        assert_eq!(&b[8..0xc], &2.0f32.to_le_bytes());
        assert_eq!(RequestCast::parse(&b), Some(cast));
        assert_eq!(Effect::heal(40).kind, Effect::HEAL);
        let hit = ActionResult {
            action: ActionResult::AUTO_ATTACK,
            kind: 1,
            lock: 0.6,
            target: 0x5020_4001,
            effects: [
                Effect::damage(18),
                Effect::default(),
                Effect::default(),
                Effect::default(),
                Effect::default(),
                Effect::default(),
                Effect::default(),
                Effect::default(),
            ],
            ..Default::default()
        };
        let b = hit.to_bytes();
        // where @0xa4e520 takes them from
        assert_eq!(&b[0..2], &7u16.to_le_bytes());
        assert_eq!(&b[0x18..0x1c], &0x5020_4001u32.to_le_bytes());
        assert_eq!((b[0x24], &b[0x2a..0x2c]), (3, &18u16.to_le_bytes()[..]));
        assert_eq!(ActionResult::parse(&b), Some((7, 0x5020_4001, Effect::damage(18))));
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
    fn the_opening_asks_for_its_event() {
        // a new Limsa Lominsa character, once 181 loaded, as the client sent it
        let up = hex::decode("0100130000000000").unwrap();
        let event_id = parse_event_request(&up).unwrap();
        assert_eq!((event_id, (event_id >> 16) as u16), (0x0013_0001, event::OPENING));
        let play = event_play(0x1000_0008, event_id, 0, 0, &[7]);
        assert_eq!(play.len(), 0x20);
        assert_eq!(u64::from_le_bytes(play[..8].try_into().unwrap()), 0x1000_0008);
        assert_eq!((&play[8..0xc], &play[0xc..0xe], play[0x14]), (&up[..4], &[0u8, 0][..], 1));
        assert_eq!(&play[0x18..0x1c], &7u32.to_le_bytes());
        // the Gridania opening's scene 0 at its end, as the client sent it (the last 4 bytes are
        // past the one value)
        let up = hex::decode("0200130000000001030000007126ceca").unwrap();
        let r = parse_scene_result(&up).unwrap();
        assert_eq!(r, SceneResult { event_id: 0x0013_0002, scene: 0, byte_6: 0, values: vec![3] });
        // walking into the Gridania opening's ERANGE_SEQ_1_CLOSED_1, as the client sent it
        let up = hex::decode("2ee3230002001300cd35e542681942c1aa980b4300000000").unwrap();
        let (range, event_id, pos) = parse_range_event(&up).unwrap();
        assert_eq!((range, event_id), (2351918, 0x0013_0002), "ERANGE_SEQ_1_CLOSED_1, as the script names it");
        assert!((pos[0] - 114.6).abs() < 0.1 && (pos[2] - 139.6).abs() < 0.1);
        // a quest slot: the id's low 16 bits @0, the sequence @2 (the client's slot +4, +6)
        let s = QuestSlot { slot: 0, quest_id: 0x10027, sequence: 255, vars: [0x31, 0, 0, 0, 0, 0x80] };
        let q = quest(&s);
        assert_eq!((q.len(), q[0], &q[4..7]), (0x10, 0, &[0x27, 0, 255][..]));
        assert_eq!((q[8], q[13]), (0x31, 0x80), "UI8A @4 and UI8F @9 of the entry");
        let all = quests(&[QuestSlot { slot: 2, ..s }]);
        assert_eq!((all.len(), &all[20..23]), (300, &[0x27, 0, 255][..]));
        // quest 39 completed: byte 39 >> 3 = 4, bit 0x80 >> 7 = 1 (MSB first, @0xa501b0)
        let flags = quest_complete_flags(&[0x10027]);
        assert_eq!((flags.len(), flags[4]), (200, 0x01));
        assert_eq!(&quest_complete_flag(0x10027, true)[..4], &[0x27, 0, 1, 1]);
        // a condition's bit is byte id >> 3, bit id & 7 (@0xa4f150); none set clears 0x10
        assert_eq!(&conditions(&[0x10, 0x2d])[..], &[0, 0, 1, 0, 0, 0x20, 0, 0]);
        assert!(conditions(&[]).iter().all(|&b| b == 0));
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
