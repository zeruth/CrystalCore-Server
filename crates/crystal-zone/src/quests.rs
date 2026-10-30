//! Quests run from their definitions ([`crystal_common::quest`]): a talk picks a scene, a scene's
//! end runs its rule's actions. The client plays the scenes (the quest's own script); the server
//! keeps the state: each active quest's slot, sequence and six variable bytes, sent to the client
//! as it keeps them (down 0x1e1), and the completed ones (0x1e3).

use bytes::Bytes;
use crystal_common::quest::{self, Action};
use crystal_db::QuestRow;
use tracing::{info, warn};

use crate::ipc::{self, opcode};
use crate::{Session, Zone, container, finish_event, play_scene, slot_update, start_event, system_message};

impl Zone {
    /// The scene a talk to a quest's event plays: the definition's first talk for the talked-to
    /// actor whose conditions hold. A quest neither active nor completed is only offered, by its
    /// start NPC (`Quest` col 27); a completed one plays nothing. The talked-to NPC is the target's
    /// `Level` row (a layout NPC's id is its layout id with high half 1, and its `Level` row's
    /// col 6 is its ENpc: Bertennant 1985150 → 1001148).
    pub(crate) fn quest_talk(&self, sess: &Session, quest_id: u32, target: u64) -> Option<u16> {
        if sess.completed.contains(&quest_id) {
            return None;
        }
        let Some(def) = self.quests.get(&quest_id) else {
            warn!(quest = format_args!("{quest_id:#x}"), "  quest: no definition");
            return None;
        };
        let data = self.data.as_deref()?;
        let enpc = (target >> 32 == 1)
            .then(|| data.sheet("Level").ok()?.row(target as u32)?.get(6)?.as_u64())
            .flatten()? as u32;
        let (seq, vars) = match sess.quests.iter().find(|q| q.quest_id == quest_id) {
            Some(q) => (q.sequence, q.vars),
            None => {
                let start = data.sheet("Quest").ok()?.row(quest_id)?.get(27)?.as_u64()?;
                if start != enpc as u64 {
                    return None;
                }
                (0, [0; 6])
            }
        };
        let talk = quest::talk_for(def, enpc, seq, &vars)?;
        if let Some(action) = talk.action {
            // the event action's cast bar (down packet not yet known): the scene plays at once
            info!(action, "  quest talk: event action skipped");
        }
        Some(talk.scene)
    }

    /// A quest's scene ended: the definition's first rule for the scene whose conditions hold
    /// runs. When it plays nothing further and does not end the event itself, the event ends.
    pub(crate) async fn quest_scene_result(&self, sess: &mut Session, r: &ipc::SceneResult) -> Vec<(u16, Bytes)> {
        let quest_id = r.event_id;
        let Some(def) = self.quests.get(&quest_id).cloned() else {
            warn!(quest = format_args!("{quest_id:#x}"), "  quest: no definition");
            return finish_event(sess);
        };
        let (seq, vars) =
            sess.quests.iter().find(|q| q.quest_id == quest_id).map_or((0, [0; 6]), |q| (q.sequence, q.vars));
        let Some(rule) = quest::rule_for(&def, r.scene, seq, &vars, &r.values) else {
            return finish_event(sess);
        };
        let mut out = Vec::new();
        let mut playing = false;
        let mut ended = false;
        let mut changed = false;
        let mut accepted = false;
        let mut warp = None;
        for action in &rule.then {
            match *action {
                Action::Play { scene } => {
                    out.extend(play_scene(sess, scene, &[]));
                    playing = true;
                }
                Action::SetSeq { seq } => match sess.quests.iter_mut().find(|q| q.quest_id == quest_id) {
                    Some(q) => {
                        q.sequence = seq;
                        changed = true;
                    }
                    None => {
                        out.extend(self.accept_quest(sess, quest_id, seq).await);
                        accepted = true;
                    }
                },
                Action::SetVar { var, value } => {
                    if let Some(q) = sess.quests.iter_mut().find(|q| q.quest_id == quest_id) {
                        var.set(&mut q.vars, value);
                        changed = true;
                    }
                }
                Action::AddVar { var, delta } => {
                    if let Some(q) = sess.quests.iter_mut().find(|q| q.quest_id == quest_id) {
                        let value = var.get(&q.vars).wrapping_add_signed(delta);
                        var.set(&mut q.vars, value);
                        changed = true;
                    }
                }
                Action::SetFlag { index, set } => {
                    if let Some(q) = sess.quests.iter_mut().find(|q| q.quest_id == quest_id) {
                        quest::set_flag(&mut q.vars, index, set);
                        changed = true;
                    }
                }
                Action::Notice { index, ref values } => {
                    info!(quest = format_args!("{quest_id:#x}"), index, ?values, "  quest notice (not sent yet)");
                }
                Action::Complete { choice } => {
                    let choice = choice.and_then(|i| r.values.get(i as usize).copied());
                    out.extend(self.complete_quest(sess, quest_id, choice).await);
                    changed = false;
                }
                Action::OpeningSeq { seq } => self.set_opening(sess, seq).await,
                Action::OpeningScene { scene } => {
                    out.extend(self.opening_scene(sess, quest_id, scene));
                    playing = true;
                }
                Action::Finish => {
                    out.extend(finish_event(sess));
                    ended = true;
                }
                Action::Warp { territory, pos, rotation } => warp = Some((territory, pos, rotation)),
            }
        }
        // an opening's first quest, accepted by a definition that does not say so: the opening
        // moves on and its onQuestAccepted (scene 30: the way-out ranges off) plays nested, as
        // Sapphire's ManFst001 does it
        if accepted && !playing && let Some(opening) = self.opening_of(quest_id) {
            self.set_opening(sess, 2).await;
            out.extend(self.opening_scene(sess, opening, 30));
            playing = true;
        }
        if changed {
            out.extend(self.save_quest(sess, quest_id).await);
        }
        if !playing && !ended {
            out.extend(finish_event(sess));
        }
        if let Some((territory, pos, rotation)) = warp {
            out.extend(self.move_to(sess, territory, pos, rotation).await);
        }
        out
    }

    /// The opening whose first quest (`Opening` col 1) `quest_id` is.
    fn opening_of(&self, quest_id: u32) -> Option<u32> {
        (0x130001..=0x130003).find(|&event| {
            self.data.as_deref().and_then(|d| d.sheet("Opening").ok()?.row(event)?.get(1)?.as_u64())
                == Some(quest_id as u64)
        })
    }

    /// Plays an opening's scene in an event nested in the quest's (kind 7, on the same target);
    /// its end ends both (`opening_scene_result`). The opening is the one whose first quest this
    /// is.
    fn opening_scene(&self, sess: &mut Session, quest_id: u32, scene: u16) -> Vec<(u16, Bytes)> {
        let opening = if (quest_id >> 16) as u16 == ipc::event::OPENING { Some(quest_id) } else { self.opening_of(quest_id) };
        let Some(opening) = opening else {
            warn!(quest = format_args!("{quest_id:#x}"), scene, "  quest: opening scene, but no opening has this quest");
            return vec![];
        };
        let target = sess.events.last().map_or(sess.actor_id as u64, |e| e.target);
        let mut out = start_event(sess, target, opening, ipc::event::NEST, 0);
        out.extend(play_scene(sess, scene, &[]));
        out
    }

    /// Moves the character's opening on to `seq` (never back).
    async fn set_opening(&self, sess: &mut Session, seq: u8) {
        let Some(chara) = sess.chara.as_mut() else { return };
        if chara.opening >= seq {
            return;
        }
        chara.opening = seq;
        if let Err(e) = self.db.characters.save_opening(chara.content_id, seq).await {
            warn!(character = %chara.name, "opening not saved: {e}");
        }
    }

    /// Makes `quest_id` the character's in its first free slot at `seq`.
    async fn accept_quest(&self, sess: &mut Session, quest_id: u32, seq: u8) -> Vec<(u16, Bytes)> {
        let Some(slot) = (0..30u8).find(|s| !sess.quests.iter().any(|q| q.slot == *s)) else {
            warn!(quest = format_args!("{quest_id:#x}"), "  quest: no free slot");
            return vec![];
        };
        sess.quests.push(ipc::QuestSlot { slot, quest_id, sequence: seq, vars: [0; 6] });
        info!(quest = format_args!("{quest_id:#x}"), slot, seq, "  quest accepted");
        self.save_quest(sess, quest_id).await
    }

    /// Tells the client an active quest's slot as the session has it, and saves it.
    async fn save_quest(&self, sess: &Session, quest_id: u32) -> Vec<(u16, Bytes)> {
        let Some(q) = sess.quests.iter().find(|q| q.quest_id == quest_id) else { return vec![] };
        if let Some(chara) = &sess.chara {
            let row = QuestRow { slot: q.slot, quest_id: q.quest_id, sequence: q.sequence, vars: q.vars };
            if let Err(e) = self.db.characters.set_quest(chara.id, &row).await {
                warn!(character = %chara.name, "quest not saved: {e}");
            }
        }
        vec![(opcode::QUEST, ipc::quest(q))]
    }

    /// Completes an active quest: its slot emptied (down 0x1e1 with id 0: the client clears the
    /// slot and counts one quest fewer, @0xa50040), its completed flag set with the client's
    /// refresh (0x1e3), its gil paid (`Quest` col 1334, 103 for Coming to Gridania, as
    /// Sapphire's script has it). Experience (col 1333, a factor) waits for the β's formula, the
    /// item rewards (and so `choice`) for their columns.
    async fn complete_quest(&self, sess: &mut Session, quest_id: u32, choice: Option<u32>) -> Vec<(u16, Bytes)> {
        let Some(i) = sess.quests.iter().position(|q| q.quest_id == quest_id) else { return vec![] };
        let slot = sess.quests.remove(i).slot;
        sess.completed.push(quest_id);
        let gil = self
            .data
            .as_deref()
            .and_then(|d| d.sheet("Quest").ok()?.row(quest_id)?.get(1334)?.as_u64())
            .unwrap_or(0);
        info!(quest = format_args!("{quest_id:#x}"), slot, gil, ?choice, "  quest completed");
        let mut out = vec![
            (opcode::QUEST, ipc::quest(&ipc::QuestSlot { slot, ..Default::default() })),
            (opcode::QUEST_COMPLETE_FLAG, ipc::quest_complete_flag(quest_id, true)),
        ];
        if gil > 0 && sess.inventory.add_gil(gil as i64).is_ok() {
            out.push((opcode::UPDATE_ITEM, slot_update(&sess.inventory, container::CURRENCY, 0)));
            out.push((opcode::CHAT_DOWN, system_message(&format!("You obtain {gil} gil."))));
            self.save_inventory(sess).await;
        }
        if let Some(chara) = &sess.chara
            && let Err(e) = self.db.characters.complete_quest(chara.id, slot, quest_id).await
        {
            warn!(character = %chara.name, "quest completion not saved: {e}");
        }
        out
    }
}
