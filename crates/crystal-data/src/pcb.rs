//! PCB collision meshes (`bg/.../collision/*.pcb`): a territory's terrain pieces
//! (`collision/list.pcb` names them, `tr%04d.pcb`) and its background parts' collision. The format
//! as Sapphire's reader has it (`deps/datReader/DatCategories/bg/pcb.h`), checked on the β's own
//! files:
//!
//! ```text
//! header  (0x18)  u32, u32, u32 entries, u32 indices, u64
//! block   (0x30)  u32 type (0x30: a group of blocks, else 0), u32 group size,
//!                 f32 x6 bounding box (min, max), u16 packed vertex count, u16 index count,
//!                 u32 float vertex count
//!   then          float vertices (f32 x3), packed vertices (u16 x3: min + v / 0xffff * size),
//!                 indices (u8 x3 and 3 more bytes each)
//! ```

use crate::{Result, format};

/// A triangle, three points.
pub type Triangle = [[f32; 3]; 3];

/// Every triangle of a PCB file, in its own space.
pub fn triangles(path: &str, d: &[u8]) -> Result<Vec<Triangle>> {
    let bad = |what: &str| format(path, what);
    if d.len() < 0x18 {
        return Err(bad("shorter than its header"));
    }
    let mut out = Vec::new();
    blocks(d, 0x18, d.len(), &mut out).ok_or_else(|| bad("a block out of range"))?;
    Ok(out)
}

/// The blocks from `at` on to `end`: groups (type 0x30) hold more blocks after their 0x30-byte
/// header, the rest hold vertices and indices. Sapphire's reader stops at the first block that is
/// not a group, at each level.
fn blocks(d: &[u8], mut at: usize, end: usize, out: &mut Vec<Triangle>) -> Option<()> {
    let u32_at = |i: usize| -> Option<u32> { Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?)) };
    let u16_at = |i: usize| -> Option<u16> { Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?)) };
    let f32_at = |i: usize| -> Option<f32> { u32_at(i).map(f32::from_bits) };
    while at + 0x28 <= end {
        let kind = u32_at(at)?;
        if kind == 0x30 {
            let size = u32_at(at + 4)? as usize;
            if size < 0x30 {
                return None;
            }
            blocks(d, at + 0x30, (at + size).min(end), out)?;
            at += size;
            continue;
        }
        let min = [f32_at(at + 8)?, f32_at(at + 12)?, f32_at(at + 16)?];
        let max = [f32_at(at + 20)?, f32_at(at + 24)?, f32_at(at + 28)?];
        let (packed, indices, floats) = (u16_at(at + 32)? as usize, u16_at(at + 34)? as usize, u32_at(at + 36)? as usize);
        let mut verts = Vec::with_capacity(floats + packed);
        let mut p = at + 0x28;
        for _ in 0..floats {
            verts.push([f32_at(p)?, f32_at(p + 4)?, f32_at(p + 8)?]);
            p += 12;
        }
        let size = [(max[0] - min[0]).abs(), (max[1] - min[1]).abs(), (max[2] - min[2]).abs()];
        for _ in 0..packed {
            let c = |k: usize| -> Option<f32> { Some(u16_at(p + 2 * k)? as f32 / 65535.0 * size[k] + min[k]) };
            verts.push([c(0)?, c(1)?, c(2)?]);
            p += 6;
        }
        for _ in 0..indices {
            let i = d.get(p..p + 3)?;
            let (a, b, c) = (verts.get(i[0] as usize)?, verts.get(i[1] as usize)?, verts.get(i[2] as usize)?);
            out.push([*a, *b, *c]);
            p += 6;
        }
        // a leaf ends its level (Sapphire: `isgroup` false stops the loop)
        return Some(());
    }
    Some(())
}

/// A background part's placement applied to a point: scale, rotation about x, then y, then z,
/// then translation (Sapphire's `nav_export`, its matrices applied to the point as a column).
pub fn place(v: [f32; 3], pos: [f32; 3], rot: [f32; 3], scale: [f32; 3]) -> [f32; 3] {
    let [mut x, mut y, mut z] = [v[0] * scale[0], v[1] * scale[1], v[2] * scale[2]];
    let (s, c) = rot[0].sin_cos();
    (y, z) = (c * y - s * z, s * y + c * z);
    let (s, c) = rot[1].sin_cos();
    (x, z) = (c * x + s * z, -s * x + c * z);
    let (s, c) = rot[2].sin_cos();
    (x, y) = (c * x - s * y, s * x + c * y);
    [x + pos[0], y + pos[1], z + pos[2]]
}

/// The terrain pieces `collision/list.pcb` names: from 0x20 on, every 0x20 bytes a u16 id
/// (Sapphire's `nav_export` reads them so; the file starts with the count and the whole
/// territory's box).
pub fn terrain_pieces(list: &[u8]) -> Vec<u16> {
    (0x20..list.len()).step_by(0x20).filter_map(|i| Some(u16::from_le_bytes(list.get(i..i + 2)?.try_into().ok()?))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_rotates_as_sapphire_does() {
        let quarter = std::f32::consts::FRAC_PI_2;
        let p = place([1.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, quarter, 0.0], [2.0, 2.0, 2.0]);
        // scaled to 2, turned a quarter about y (x → -z), moved by 10 on x
        assert!((p[0] - 10.0).abs() < 1e-5 && (p[2] + 2.0).abs() < 1e-5, "{p:?}");
    }

    #[test]
    fn a_leaf_block_reads() {
        let mut d = vec![0u8; 0x18];
        let mut block = vec![0u8; 0x28];
        block[24..28].copy_from_slice(&1f32.to_le_bytes());
        block[34..36].copy_from_slice(&1u16.to_le_bytes()); // one triangle
        block[36..40].copy_from_slice(&3u32.to_le_bytes()); // three float vertices
        d.extend_from_slice(&block);
        for v in [[0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]] {
            for c in v {
                d.extend_from_slice(&c.to_le_bytes());
            }
        }
        d.extend_from_slice(&[0, 1, 2, 0, 0, 0]);
        let t = triangles("t.pcb", &d).unwrap();
        assert_eq!(t, vec![[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]]);
    }
}
