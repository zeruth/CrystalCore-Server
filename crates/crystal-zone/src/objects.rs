//! Objects that are not characters, kept in the client's 40 object slots (down 0x19d creates
//! one in a slot, 0x19e deletes it): so far the territory's gathering points
//! ([`crystal_common::gathering`]), the nearest ones in range.

use bytes::Bytes;
use crystal_common::gathering::Placement;

use crate::ipc::{self, opcode};
use crate::{Session, Zone};

/// How far away an object is shown (the server's choice: the client takes what it is sent).
const RANGE: f32 = 80.0;
/// How far the player moves before the set is worked out again.
const REFRESH_STEP: f32 = 15.0;

impl Zone {
    /// The player entered a territory: the client starts with no objects.
    pub(crate) fn objects_entered(&self, sess: &mut Session) -> Vec<(u16, Bytes)> {
        sess.objects = [None; ipc::object::SLOTS as usize];
        sess.objects_at = None;
        self.refresh_objects(sess)
    }

    /// The player moved: past [`REFRESH_STEP`] since the last time, the objects in range are
    /// worked out again; those gone are deleted, those new created in free slots, nearest first.
    pub(crate) fn objects_moved(&self, sess: &mut Session) -> Vec<(u16, Bytes)> {
        let here = sess.pos();
        match sess.objects_at {
            Some(at) if distance(at, here) < REFRESH_STEP => vec![],
            _ => self.refresh_objects(sess),
        }
    }

    fn refresh_objects(&self, sess: &mut Session) -> Vec<(u16, Bytes)> {
        let here = sess.pos();
        sess.objects_at = Some(here);
        let territory = sess.position.zone_id as u16;
        let mut near: Vec<&Placement> = self
            .gathering
            .get(&territory)
            .map(|points| points.iter().filter(|p| distance(p.pos, here) <= RANGE).collect())
            .unwrap_or_default();
        near.sort_by(|a, b| distance(a.pos, here).total_cmp(&distance(b.pos, here)));
        near.truncate(ipc::object::SLOTS as usize);

        let mut out = Vec::new();
        for (slot, held) in sess.objects.iter_mut().enumerate() {
            if let Some(point) = *held
                && !near.iter().any(|p| p.point == point)
            {
                *held = None;
                out.push((opcode::DELETE_OBJECT, ipc::delete_object(slot as u8)));
            }
        }
        for p in near {
            if sess.objects.contains(&Some(p.point)) {
                continue;
            }
            let Some(slot) = sess.objects.iter().position(Option::is_none) else { break };
            sess.objects[slot] = Some(p.point);
            out.push((
                opcode::CREATE_OBJECT,
                ipc::create_object(slot as u8, ipc::object::GATHERING_POINT, p.point, p.rotation, p.pos),
            ));
        }
        out
    }
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}
