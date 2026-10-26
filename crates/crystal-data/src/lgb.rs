//! LGB layouts (`bg/<territory path>/level/*.lgb`): layers of instance objects, each with a type,
//! an instance id, a transform and type-specific data.
//!
//! ```text
//! "LGB1" size chunks  "LGP1" size  group id, name, layers offset, layer count
//! layer:   id, name, objects offset, object count           (offsets from the table holding them)
//! object:  type u32, instance id u32, name, translation, rotation, scale (f32 x3 each), data...
//! ```

use crate::{Result, format};

/// Object types the server reads (the layout's type numbers).
pub mod kind {
    pub const EVENT_NPC: u32 = 8;
    pub const AETHERYTE: u32 = 12;
    pub const POP_RANGE: u32 = 40;
    pub const EXIT_RANGE: u32 = 41;
    pub const MAP_RANGE: u32 = 43;
    pub const EVENT_OBJECT: u32 = 45;
}

#[derive(Debug, Clone)]
pub struct Object {
    pub kind: u32,
    pub id: u32,
    pub name: String,
    pub pos: [f32; 3],
    pub rot: [f32; 3],
    /// The type-specific bytes after the 0x30-byte header (to the end of the file; each type
    /// reads what it needs).
    data: Vec<u8>,
}

/// An exit range's data (0x24 bytes after the header): a trigger box (shape @0, priority @4,
/// enabled @6), exit type @0xc, destination territory @0x12, destination pop range @0x18, the
/// pop range players arriving through it use @0x1c, the facing on arrival @0x20.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExitRange {
    pub exit_type: i32,
    pub territory: u16,
    pub destination: u32,
    pub return_point: u32,
    pub direction: f32,
}

impl Object {
    pub fn exit_range(&self) -> Option<ExitRange> {
        if self.kind != kind::EXIT_RANGE || self.data.len() < 0x24 {
            return None;
        }
        let d = &self.data;
        let u32_at = |i: usize| u32::from_le_bytes(d[i..i + 4].try_into().unwrap());
        Some(ExitRange {
            exit_type: u32_at(0xc) as i32,
            territory: u16::from_le_bytes([d[0x12], d[0x13]]),
            destination: u32_at(0x18),
            return_point: u32_at(0x1c),
            direction: f32::from_bits(u32_at(0x20)),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub id: u32,
    pub name: String,
    pub objects: Vec<Object>,
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub layers: Vec<Layer>,
}

impl Layout {
    pub fn parse(path: &str, d: &[u8]) -> Result<Self> {
        let bad = |what: &str| format(path, what);
        if d.get(..4) != Some(b"LGB1") || d.get(0xc..0x10) != Some(b"LGP1") {
            return Err(bad("not an LGB1/LGP1 layout"));
        }
        let i32_at = |at: usize| -> Result<i32> {
            d.get(at..at + 4)
                .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
                .ok_or_else(|| bad("offset out of range"))
        };
        let f32_at = |at: usize| -> Result<f32> { i32_at(at).map(|v| f32::from_bits(v as u32)) };
        let name_at = |at: usize| -> String {
            let end = d[at.min(d.len())..].iter().position(|&b| b == 0).map_or(d.len(), |p| at + p);
            String::from_utf8_lossy(&d[at.min(end)..end]).into_owned()
        };
        let base = 0x14;
        let (layers_off, layer_count) = (i32_at(base + 8)?, i32_at(base + 12)?);
        let table = (base as i64 + layers_off as i64) as usize;
        let mut layers = Vec::with_capacity(layer_count.max(0) as usize);
        for i in 0..layer_count.max(0) as usize {
            let lo = (table as i64 + i32_at(table + 4 * i)? as i64) as usize;
            let (id, name_off, objs_off, obj_count) =
                (i32_at(lo)? as u32, i32_at(lo + 4)?, i32_at(lo + 8)?, i32_at(lo + 12)?);
            let ot = (lo as i64 + objs_off as i64) as usize;
            let mut objects = Vec::with_capacity(obj_count.max(0) as usize);
            for j in 0..obj_count.max(0) as usize {
                let oo = (ot as i64 + i32_at(ot + 4 * j)? as i64) as usize;
                let (kind, inst, oname) = (i32_at(oo)? as u32, i32_at(oo + 4)? as u32, i32_at(oo + 8)?);
                let v = |k: usize| f32_at(oo + 12 + 4 * k);
                objects.push(Object {
                    kind,
                    id: inst,
                    name: if oname != 0 { name_at((oo as i64 + oname as i64) as usize) } else { String::new() },
                    pos: [v(0)?, v(1)?, v(2)?],
                    rot: [v(3)?, v(4)?, v(5)?],
                    data: d.get(oo + 0x30..(oo + 0x30 + 0x40).min(d.len())).unwrap_or_default().to_vec(),
                });
            }
            layers.push(Layer { id, name: name_at((lo as i64 + name_off as i64) as usize), objects });
        }
        Ok(Self { layers })
    }

    pub fn objects(&self) -> impl Iterator<Item = &Object> {
        self.layers.iter().flat_map(|l| l.objects.iter())
    }

    pub fn find(&self, id: u32) -> Option<&Object> {
        self.objects().find(|o| o.id == id)
    }
}
