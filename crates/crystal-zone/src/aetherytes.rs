//! Aetherytes and aethernet shards (event handler type 5; the client's
//! `game_script/transport/Aetheryte` plays their scenes, Sapphire's `Aetheryte.cpp` as a
//! cross-check for the results):
//!
//! - a shard not yet attuned: attuned (order 0x1fd), scene 3 (the attunement; the script's lines
//!   for the first and the last of a town's shards);
//! - an attuned shard: scene 2, the aethernet menu (`AethernetMenu`), which returns the shard to
//!   travel to;
//! - an aetheryte not yet attuned: attuned, Teleport unlocked (order 0x29, 4) the first time;
//!   the β plays its message in `OnNotice`, whose packet is not known yet;
//! - an attuned aetheryte: scene 0, its menu (`AetheryteMenu`): (1) a home point, (4, shard) the
//!   aethernet.
//!
//! The client keeps what is attuned in the bits the player's status fills (@0xe8 → @0x14b4ed0)
//! and order 0x1fd sets; the aethernet menu lists only those.
//!
//! Attuning has a cast bar (the event action, 0x13, Sapphire's `eventActionStart`) whose packet is
//! not known yet: it is attuned at once.

use bytes::Bytes;
use tracing::{info, warn};

use crate::ipc::{self, opcode, order_kind};
use crate::{Session, Zone, finish_event, play_scene};

/// Teleport's unlock (Sapphire's `UnlockEntry::Teleport`; the β client tests unlock byte 0's
/// 0x10 bit).
const UNLOCK_TELEPORT: u32 = 4;

impl Zone {
    /// A talk to an aetheryte or a shard (the event already started).
    pub(crate) async fn aetheryte_talk(&self, sess: &mut Session, event_id: u32) -> Vec<(u16, Bytes)> {
        let id = event_id & 0xffff;
        let Some(info) = self.data.as_deref().and_then(|d| d.aetheryte(id)) else {
            warn!(id, "  aetheryte: no Aetheryte row");
            return finish_event(sess);
        };
        let attuned = sess.aetherytes.contains(&id);
        info!(id, main = info.main, attuned, "  aetheryte");
        match (info.main, attuned) {
            (false, false) => {
                let mut out = self.attune(sess, id).await;
                out.extend(play_scene(sess, 3, &[]));
                out
            }
            (false, true) => play_scene(sess, 2, &[]),
            (true, false) => {
                let mut out = self.attune(sess, id).await;
                if !sess.unlocks.contains(&UNLOCK_TELEPORT) {
                    out.extend(self.unlock(sess, UNLOCK_TELEPORT).await);
                }
                out.extend(finish_event(sess));
                out
            }
            (true, true) => play_scene(sess, 0, &[]),
        }
    }

    /// An aetheryte's scene ended: travel where its menu said, else just end.
    pub(crate) async fn aetheryte_scene_result(&self, sess: &mut Session, r: &ipc::SceneResult) -> Vec<(u16, Bytes)> {
        let destination = match (r.scene, r.values.as_slice()) {
            // the aethernet menu: the shard chosen (0: none)
            (2, [d]) if *d != 0 => Some(*d),
            // the aetheryte's menu: (4, shard) for the aethernet
            (0, [4, d]) if *d != 0 => Some(*d),
            (0, [1]) => {
                info!(aetheryte = r.event_id & 0xffff, "  home point chosen (not kept yet)");
                None
            }
            _ => None,
        };
        let mut out = finish_event(sess);
        if let Some(d) = destination {
            match self.data.as_deref().and_then(|data| data.aetheryte(d)).and_then(|a| a.arrival) {
                Some(to) if sess.aetherytes.contains(&d) => {
                    info!(to = d, territory = to.territory, "  aethernet travel");
                    // the departure: order 0xcf (case 0x69 → @0xab78d0: warp type 0xe plays the
                    // teleport effect, 0x70, on the character arg 2, fades out and holds the
                    // player; arg 4 the destination territory), then the move a second later,
                    // as Sapphire's move task waits 1000 ms
                    let warp = [ipc::warp_type::TOWN_TRANSLATE, sess.actor_id, 0, to.territory as u32];
                    out.push((opcode::ORDER, ipc::order(order_kind::WARP_START, warp)));
                    let at = tokio::time::Instant::now() + std::time::Duration::from_millis(1000);
                    sess.pending_move = Some((at, to.territory, to.pos, to.rotation));
                }
                Some(_) => warn!(to = d, "  aethernet: not attuned to it"),
                None => warn!(to = d, "  aethernet: no arrival for it"),
            }
        }
        out
    }

    /// Attunes the character to an aetheryte: kept, saved and told to the client (order 0x1fd).
    pub(crate) async fn attune(&self, sess: &mut Session, id: u32) -> Vec<(u16, Bytes)> {
        if !sess.aetherytes.contains(&id) {
            sess.aetherytes.push(id);
            if let Some(chara) = &sess.chara
                && let Err(e) = self.db.characters.add_aetheryte(chara.id, id).await
            {
                warn!(character = %chara.name, "aetheryte not saved: {e}");
            }
        }
        info!(id, "  attuned");
        vec![(opcode::ORDER, ipc::order(order_kind::LEARN_AETHERYTE, [id, 1, 0, 0]))]
    }

    /// Unlocks something for the character (1 Return, 4 Teleport): kept, saved and told to the
    /// client (order 0x29).
    pub(crate) async fn unlock(&self, sess: &mut Session, id: u32) -> Vec<(u16, Bytes)> {
        if !sess.unlocks.contains(&id) {
            sess.unlocks.push(id);
            if let Some(chara) = &sess.chara
                && let Err(e) = self.db.characters.add_unlock(chara.id, id).await
            {
                warn!(character = %chara.name, "unlock not saved: {e}");
            }
        }
        info!(id, "  unlocked");
        vec![(opcode::ORDER, ipc::order(order_kind::SET_UNLOCK, [id, 1, 0, 0]))]
    }
}
