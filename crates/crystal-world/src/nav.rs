//! Where battle NPCs can walk: a territory's collision triangles (the client's PCB files) turned
//! into a grid of walkable surfaces, and paths over it.
//!
//! Each cell (`CELL` yalms square) keeps up to [`LAYERS`] heights where a walkable surface (one
//! no steeper than [`MAX_SLOPE`]) covers its centre, and the height range of the steep surfaces
//! (walls, cliffs, trunks) that cross it. A surface is standable when nothing steep stands on it
//! within a body's height and no other surface hangs over it that low. A step from cell to
//! neighbouring cell climbs or drops at most [`CLIMB`]. Paths are A* over the cells, eight ways,
//! not cutting corners, then straightened where a straight line stays walkable.
//!
//! This is the server's own approximation of a navigation mesh (Sapphire builds Recast meshes
//! from the same files): what it gets wrong, play shows.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// Cell size, yalms.
pub const CELL: f32 = 1.0;
/// Walkable surfaces a cell keeps (ground, a bridge over it, a floor above that).
pub const LAYERS: usize = 3;
/// The steepest walkable surface: its normal's y at least this (about 50 degrees).
pub const MAX_SLOPE: f32 = 0.64;
/// The most a step from a cell to the next climbs or drops, yalms.
pub const CLIMB: f32 = 1.0;
/// A body's height: what stands on or hangs over a surface lower than this blocks it, yalms.
pub const HEIGHT: f32 = 1.8;
/// The most cells a path search looks at (about 80 yalms around, open ground).
pub const MAX_SEARCH: usize = 40_000;

const NONE: i16 = i16::MIN;

/// Heights are kept in tenths of a yalm.
fn dm(y: f32) -> i16 {
    (y * 10.0).round().clamp(-32000.0, 32000.0) as i16
}

fn yalms(h: i16) -> f32 {
    h as f32 / 10.0
}

#[derive(Debug, Clone)]
pub struct NavGrid {
    min: [f32; 2],
    w: usize,
    h: usize,
    /// Walkable heights per cell, ascending, [`NONE`] for unused.
    layers: Vec<[i16; LAYERS]>,
    /// The lowest and highest point of the steep surfaces crossing each cell (lo > hi: none).
    walls: Vec<(i16, i16)>,
}

impl NavGrid {
    /// The grid of `triangles` (world space).
    pub fn build(triangles: &[[[f32; 3]; 3]]) -> Self {
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for v in triangles.iter().flatten() {
            lo = [lo[0].min(v[0]), lo[1].min(v[2])];
            hi = [hi[0].max(v[0]), hi[1].max(v[2])];
        }
        if lo[0] > hi[0] {
            return Self { min: [0.0; 2], w: 0, h: 0, layers: vec![], walls: vec![] };
        }
        let w = ((hi[0] - lo[0]) / CELL).ceil() as usize + 1;
        let h = ((hi[1] - lo[1]) / CELL).ceil() as usize + 1;
        let mut grid = Self { min: lo, w, h, layers: vec![[NONE; LAYERS]; w * h], walls: vec![(i16::MAX, i16::MIN); w * h] };
        for t in triangles {
            grid.rasterize(t);
        }
        grid
    }

    fn rasterize(&mut self, t: &[[f32; 3]; 3]) {
        let [a, b, c] = *t;
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len < 1e-6 {
            return;
        }
        if (n[1] / len).abs() >= MAX_SLOPE {
            // walkable: the height at every cell centre it covers
            let (x0, x1) = (a[0].min(b[0]).min(c[0]), a[0].max(b[0]).max(c[0]));
            let (z0, z1) = (a[2].min(b[2]).min(c[2]), a[2].max(b[2]).max(c[2]));
            let (Some((i0, j0)), Some((i1, j1))) = (self.cell_xz(x0, z0), self.cell_xz(x1, z1)) else { return };
            for j in j0..=j1 {
                for i in i0..=i1 {
                    let (cx, cz) = self.centre(i, j);
                    if let Some(y) = height_in(a, b, c, cx, cz) {
                        self.add_layer(j * self.w + i, dm(y));
                    }
                }
            }
        } else {
            // steep: every cell it crosses, sampled over its face
            let longest = [dist(a, b), dist(b, c), dist(c, a)].into_iter().fold(0.0f32, f32::max);
            let steps = ((longest / (CELL * 0.5)).ceil() as usize).clamp(1, 96);
            for p in 0..=steps {
                for q in 0..=(steps - p) {
                    let (s, r) = (p as f32 / steps as f32, q as f32 / steps as f32);
                    let x = a[0] + s * u[0] + r * v[0];
                    let y = a[1] + s * u[1] + r * v[1];
                    let z = a[2] + s * u[2] + r * v[2];
                    if let Some((i, j)) = self.cell_xz(x, z) {
                        let wall = &mut self.walls[j * self.w + i];
                        let y = dm(y);
                        *wall = (wall.0.min(y), wall.1.max(y));
                    }
                }
            }
        }
    }

    fn add_layer(&mut self, idx: usize, y: i16) {
        let cell = &mut self.layers[idx];
        // a surface within half a yalm of one kept is the same one (the higher kept)
        for l in cell.iter_mut() {
            if *l != NONE && (*l - y).abs() <= 5 {
                *l = (*l).max(y);
                return;
            }
        }
        if let Some(slot) = cell.iter_mut().find(|l| **l == NONE) {
            *slot = y;
        } else if y < cell[LAYERS - 1] {
            // full: the highest gives way
            cell[LAYERS - 1] = y;
        }
        cell.sort_by(|p, q| match (*p == NONE, *q == NONE) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => p.cmp(q),
        });
    }

    fn cell_xz(&self, x: f32, z: f32) -> Option<(usize, usize)> {
        let i = ((x - self.min[0]) / CELL).floor();
        let j = ((z - self.min[1]) / CELL).floor();
        (i >= 0.0 && j >= 0.0 && (i as usize) < self.w && (j as usize) < self.h).then_some((i as usize, j as usize))
    }

    fn centre(&self, i: usize, j: usize) -> (f32, f32) {
        (self.min[0] + (i as f32 + 0.5) * CELL, self.min[1] + (j as f32 + 0.5) * CELL)
    }

    /// Whether the surface at `y` in cell `idx` can be stood on.
    fn standable(&self, idx: usize, y: i16) -> bool {
        let (wall_lo, wall_hi) = self.walls[idx];
        let (low, high) = (y + 4, y + dm(HEIGHT));
        if wall_lo <= wall_hi && wall_hi > low && wall_lo < high {
            return false;
        }
        // another surface overhead, too low to stand under
        !self.layers[idx].iter().any(|&l| l != NONE && l > y + 2 && l < high)
    }

    /// The standable surface in cell `idx` nearest `y`, within `reach`.
    fn layer_near(&self, idx: usize, y: i16, reach: i16) -> Option<i16> {
        self.layers[idx]
            .iter()
            .copied()
            .filter(|&l| l != NONE && (l - y).abs() <= reach && self.standable(idx, l))
            .min_by_key(|&l| (l - y).abs())
    }

    /// The ground under or near `pos`: the standable surface of its cell nearest its height,
    /// within `reach` yalms.
    pub fn ground(&self, pos: [f32; 3], reach: f32) -> Option<f32> {
        let (i, j) = self.cell_xz(pos[0], pos[2])?;
        self.layer_near(j * self.w + i, dm(pos[1]), dm(reach)).map(yalms)
    }

    /// A walkable path from `from` to `to`: its points after `from` (the last as near `to` as
    /// the search got, when it could not reach it). `None` when `from` stands nowhere walkable.
    pub fn path(&self, from: [f32; 3], to: [f32; 3]) -> Option<Vec<[f32; 3]>> {
        let (si, sj) = self.cell_xz(from[0], from[2])?;
        let start = (sj * self.w + si, self.layer_near(sj * self.w + si, dm(from[1]), dm(4.0))?);
        let goal_cell = self.cell_xz(to[0], to[2]).map(|(i, j)| j * self.w + i);
        let goal_y = dm(to[1]);
        let (gi, gj) = self.cell_xz(to[0], to[2]).unwrap_or((si, sj));
        let h = |idx: usize| {
            let (i, j) = (idx % self.w, idx / self.w);
            (((i as f32 - gi as f32).powi(2) + (j as f32 - gj as f32).powi(2)).sqrt() * CELL * 10.0) as u32
        };
        let mut open = BinaryHeap::new();
        let mut came: HashMap<(usize, i16), ((usize, i16), u32)> = HashMap::new();
        came.insert(start, (start, 0));
        open.push(std::cmp::Reverse((h(start.0), 0u32, start)));
        let mut best = (h(start.0), start);
        let mut seen = 0;
        let mut reached = None;
        while let Some(std::cmp::Reverse((_, g, node))) = open.pop() {
            if came.get(&node).is_some_and(|(_, cost)| *cost < g) {
                continue;
            }
            seen += 1;
            if Some(node.0) == goal_cell && (node.1 - goal_y).abs() <= dm(4.0) {
                reached = Some(node);
                break;
            }
            if h(node.0) < best.0 {
                best = (h(node.0), node);
            }
            if seen >= MAX_SEARCH {
                break;
            }
            let (i, j) = ((node.0 % self.w) as i64, (node.0 / self.w) as i64);
            for (di, dj) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let Some(next) = self.step(i, j, node.1, di, dj) else { continue };
                let cost = g + if di != 0 && dj != 0 { 14 } else { 10 } + ((next.1 - node.1).unsigned_abs() as u32);
                if came.get(&next).is_none_or(|(_, c)| cost < *c) {
                    came.insert(next, (node, cost));
                    open.push(std::cmp::Reverse((cost + h(next.0), cost, next)));
                }
            }
        }
        let end = reached.unwrap_or(best.1);
        let mut cells = vec![end];
        let mut at = end;
        while at != start {
            at = came[&at].0;
            cells.push(at);
        }
        cells.reverse();
        let points: Vec<[f32; 3]> = cells
            .iter()
            .map(|&(idx, y)| {
                let (x, z) = self.centre(idx % self.w, idx / self.w);
                [x, yalms(y), z]
            })
            .collect();
        Some(self.straighten(from, &points))
    }

    /// The neighbour of cell (i, j) at surface `y` one step `(di, dj)` away: its surface within a
    /// climb, standable; a diagonal only when both sides it passes are passable too.
    fn step(&self, i: i64, j: i64, y: i16, di: i64, dj: i64) -> Option<(usize, i16)> {
        let at = |i: i64, j: i64| -> Option<usize> {
            (i >= 0 && j >= 0 && (i as usize) < self.w && (j as usize) < self.h).then(|| j as usize * self.w + i as usize)
        };
        let idx = at(i + di, j + dj)?;
        let ny = self.layer_near(idx, y, dm(CLIMB))?;
        if di != 0 && dj != 0 {
            self.layer_near(at(i + di, j)?, y, dm(CLIMB))?;
            self.layer_near(at(i, j + dj)?, y, dm(CLIMB))?;
        }
        Some((idx, ny))
    }

    /// Whether a straight walk from `a` to `b` stays on walkable cells, climbing no more than a
    /// step at a time.
    fn line_clear(&self, a: [f32; 3], b: [f32; 3]) -> bool {
        let d = ((b[0] - a[0]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
        let n = (d / (CELL * 0.5)).ceil().max(1.0) as usize;
        let mut y = dm(a[1]);
        for k in 1..=n {
            let t = k as f32 / n as f32;
            let (x, z) = (a[0] + (b[0] - a[0]) * t, a[2] + (b[2] - a[2]) * t);
            let Some((i, j)) = self.cell_xz(x, z) else { return false };
            match self.layer_near(j * self.w + i, y, dm(CLIMB)) {
                Some(ny) => y = ny,
                None => return false,
            }
        }
        true
    }

    /// The path's corners only: each point kept is the farthest one a straight line reaches.
    fn straighten(&self, from: [f32; 3], points: &[[f32; 3]]) -> Vec<[f32; 3]> {
        let mut out = Vec::new();
        let mut at = from;
        let mut k = 0;
        while k < points.len() {
            let mut far = k;
            for m in (k + 1..points.len()).rev().take(64) {
                if self.line_clear(at, points[m]) {
                    far = m;
                    break;
                }
            }
            out.push(points[far]);
            at = points[far];
            k = far + 1;
        }
        out
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The triangle's height at (x, z), when (x, z) is within its projection.
fn height_in(a: [f32; 3], b: [f32; 3], c: [f32; 3], x: f32, z: f32) -> Option<f32> {
    let d = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
    if d.abs() < 1e-9 {
        return None;
    }
    let l1 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / d;
    let l2 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / d;
    let l3 = 1.0 - l1 - l2;
    let e = -1e-4;
    (l1 >= e && l2 >= e && l3 >= e).then(|| l1 * a[1] + l2 * b[1] + l3 * c[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat square floor from (0,0) to (size,size) at height `y`, two triangles.
    fn floor(size: f32, y: f32) -> Vec<[[f32; 3]; 3]> {
        vec![
            [[0.0, y, 0.0], [size, y, 0.0], [size, y, size]],
            [[0.0, y, 0.0], [size, y, size], [0.0, y, size]],
        ]
    }

    /// A wall along x = `x` from z = `z0` to `z1`, `height` tall, standing on y = 0.
    fn wall(x: f32, z0: f32, z1: f32, height: f32) -> Vec<[[f32; 3]; 3]> {
        vec![
            [[x, 0.0, z0], [x, height, z0], [x, height, z1]],
            [[x, 0.0, z0], [x, height, z1], [x, 0.0, z1]],
        ]
    }

    #[test]
    fn ground_is_found_on_a_floor() {
        let g = NavGrid::build(&floor(20.0, 3.0));
        assert_eq!(g.ground([5.0, 10.0, 5.0], 10.0), Some(3.0));
        assert_eq!(g.ground([5.0, 30.0, 5.0], 10.0), None, "too far above");
    }

    #[test]
    fn paths_go_around_a_wall() {
        let mut tris = floor(30.0, 0.0);
        // a wall across the middle, with a gap near z = 28
        tris.extend(wall(15.0, 0.0, 25.0, 5.0));
        let g = NavGrid::build(&tris);
        let path = g.path([5.0, 0.0, 5.0], [25.0, 0.0, 5.0]).unwrap();
        let end = *path.last().unwrap();
        assert!((end[0] - 25.0).abs() < 1.5 && (end[2] - 5.0).abs() < 1.5, "reached: {end:?}");
        // it went through the gap
        assert!(path.iter().any(|p| p[2] > 24.0), "{path:?}");
        // and never through the wall
        let mut at = [5.0, 0.0, 5.0];
        for p in &path {
            assert!(g.line_clear(at, *p), "{at:?} -> {p:?}");
            at = *p;
        }
    }

    #[test]
    fn open_ground_is_one_straight_leg() {
        let g = NavGrid::build(&floor(40.0, 0.0));
        let path = g.path([2.0, 0.0, 2.0], [30.0, 0.0, 20.0]).unwrap();
        assert_eq!(path.len(), 1, "{path:?}");
    }
}
