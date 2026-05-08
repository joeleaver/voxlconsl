//! Macro-grid actor binning — see SPEC.md §11.6.
//!
//! A coarse 16³ grid of cells (32 world voxels per cell) covering the
//! full 512³ world. Each frame the host bins every visible actor's
//! world-space AABB into the cells it overlaps; per-ray traversal then
//! walks the macro-grid via Amanatides–Woo, gathering candidate actor
//! IDs only from cells the ray enters.
//!
//! ## Storage
//!
//! Packed-offset CSR-style layout:
//!   - `offsets[k]` = first index into `packed` for cell `k` (length
//!     `N_CELLS + 1`, sentinel at the end).
//!   - `packed[i]` = actor slot index.
//! Rebuild is a 3-pass count / prefix-sum / place; per-frame allocation
//! is bounded and the offsets vec is reused across frames.

use voxlconsl_types::Vec3;

use crate::actors::ActorTable;

/// Cells per axis. `MACRO_SIDE * CELL_VOXELS = 512` (the world side).
pub const MACRO_SIDE: u32 = 16;
/// World voxels per macro-cell along one axis.
pub const CELL_VOXELS: f32 = 32.0;
/// Total world side in voxels (covered by the macro-grid).
pub const WORLD_SIDE: f32 = MACRO_SIDE as f32 * CELL_VOXELS;

const N_CELLS: usize = (MACRO_SIDE * MACRO_SIDE * MACRO_SIDE) as usize;

/// Linear cell key (cz * MACRO_SIDE + cy) * MACRO_SIDE + cx.
#[inline]
pub fn cell_key(cx: u32, cy: u32, cz: u32) -> u32 {
    ((cz * MACRO_SIDE) + cy) * MACRO_SIDE + cx
}

/// Packed-offset macro-grid. Build once per frame; query per ray.
pub struct MacroGrid {
    /// `offsets[k] .. offsets[k+1]` is the slice of `packed` containing
    /// the slot indices of actors whose AABB overlaps cell `k`.
    offsets: Vec<u32>,
    packed: Vec<u32>,
}

impl MacroGrid {
    pub fn new() -> Self {
        Self {
            offsets: vec![0; N_CELLS + 1],
            packed: Vec::new(),
        }
    }

    /// Repopulate from the actor table. Call once per frame *after*
    /// `actors.flush_all()` so actor world AABBs reflect the current
    /// transform/orientation.
    pub fn rebuild(&mut self, actors: &ActorTable) {
        // Pass 1 — count: offsets[k] gets the per-cell occupancy count.
        for o in self.offsets.iter_mut() {
            *o = 0;
        }
        let mut total = 0u32;
        actors.for_each_visible_with_index(|_idx, actor| {
            let (cmin, cmax) = aabb_to_cell_range(actor.world_aabb());
            for cz in cmin.2..=cmax.2 {
                for cy in cmin.1..=cmax.1 {
                    for cx in cmin.0..=cmax.0 {
                        self.offsets[cell_key(cx, cy, cz) as usize] += 1;
                        total += 1;
                    }
                }
            }
        });

        // Pass 2 — exclusive prefix sum: turn counts into start offsets.
        let mut acc = 0u32;
        for o in self.offsets.iter_mut() {
            let c = *o;
            *o = acc;
            acc += c;
        }

        // Pass 3 — place: write slot indices into packed at each cell's
        // running cursor. We bump offsets[k] as we go, then fix up.
        self.packed.clear();
        self.packed.resize(total as usize, u32::MAX);
        actors.for_each_visible_with_index(|idx, actor| {
            let (cmin, cmax) = aabb_to_cell_range(actor.world_aabb());
            for cz in cmin.2..=cmax.2 {
                for cy in cmin.1..=cmax.1 {
                    for cx in cmin.0..=cmax.0 {
                        let k = cell_key(cx, cy, cz) as usize;
                        let pos = self.offsets[k];
                        self.packed[pos as usize] = idx;
                        self.offsets[k] = pos + 1;
                    }
                }
            }
        });

        // Pass 4 — restore offsets. After pass 3, offsets[k] points one
        // past the last entry for cell k. Shift back by one position so
        // offsets[k] is again the start of cell k.
        for k in (1..=N_CELLS).rev() {
            self.offsets[k] = self.offsets[k - 1];
        }
        self.offsets[0] = 0;
    }

    /// Slot indices of actors whose AABB overlaps cell `(cx, cy, cz)`.
    /// Returns an empty slice if the coords are out of range.
    pub fn cell_actors(&self, cx: u32, cy: u32, cz: u32) -> &[u32] {
        if cx >= MACRO_SIDE || cy >= MACRO_SIDE || cz >= MACRO_SIDE {
            return &[];
        }
        let k = cell_key(cx, cy, cz) as usize;
        let start = self.offsets[k] as usize;
        let end = self.offsets[k + 1] as usize;
        &self.packed[start..end]
    }

    /// Iterator over `(cx, cy, cz)` of cells along a ray, in front-to-back
    /// order, clipped to the world AABB and `max_t`.
    pub fn ray_iter(&self, origin: Vec3, dir: Vec3, max_t: f32) -> RayCellIter {
        RayCellIter::new(origin, dir, max_t)
    }

    /// Total number of actor entries currently stored across all cells.
    /// Useful for tests and diagnostics.
    pub fn entry_count(&self) -> usize {
        self.packed.len()
    }
}

impl Default for MacroGrid {
    fn default() -> Self { Self::new() }
}

/// Convert a world-space AABB into the inclusive macro-cell range it
/// overlaps. Coords outside the world are clamped into [0, MACRO_SIDE-1].
fn aabb_to_cell_range(aabb: (Vec3, Vec3)) -> ((u32, u32, u32), (u32, u32, u32)) {
    let (mn, mx) = aabb;
    let to_cell = |v: f32| -> u32 {
        let c = (v / CELL_VOXELS).floor();
        if c < 0.0 { 0 } else if c >= MACRO_SIDE as f32 { MACRO_SIDE - 1 } else { c as u32 }
    };
    let cmin = (to_cell(mn.x), to_cell(mn.y), to_cell(mn.z));
    let cmax = (to_cell(mx.x), to_cell(mx.y), to_cell(mx.z));
    (cmin, cmax)
}

/// Amanatides–Woo macro-cell DDA. Yields `(cx, cy, cz)` of each cell
/// the ray enters, in front-to-back order, until either the ray exits
/// the world AABB or `t` exceeds `max_t`.
pub struct RayCellIter {
    cx: i32, cy: i32, cz: i32,
    step_x: i32, step_y: i32, step_z: i32,
    t_max_x: f32, t_max_y: f32, t_max_z: f32,
    t_delta_x: f32, t_delta_y: f32, t_delta_z: f32,
    t_end: f32,
    started: bool,
    done: bool,
}

impl RayCellIter {
    fn new(origin: Vec3, dir: Vec3, max_t: f32) -> Self {
        // Clip the ray to [0, WORLD_SIDE]³.
        let (t_in, t_out) = match ray_aabb_t(origin, dir, Vec3::ZERO, Vec3::splat(WORLD_SIDE)) {
            Some(t) => t,
            None => return Self::dead(),
        };
        let t_in = t_in.max(0.0);
        let t_end = t_out.min(max_t);
        if t_in > t_end {
            return Self::dead();
        }

        // Entry point in world coords.
        let entry = Vec3::new(
            origin.x + dir.x * t_in,
            origin.y + dir.y * t_in,
            origin.z + dir.z * t_in,
        );

        // Initial cell. Clamp to [0, MACRO_SIDE-1] since floating-point
        // entry points can land exactly on a cell boundary.
        let clamp_cell = |v: f32| -> i32 {
            let c = (v / CELL_VOXELS).floor() as i32;
            c.max(0).min(MACRO_SIDE as i32 - 1)
        };
        let cx = clamp_cell(entry.x);
        let cy = clamp_cell(entry.y);
        let cz = clamp_cell(entry.z);

        let make_axis = |o: f32, d: f32, c: i32| -> (i32, f32, f32) {
            if d > 0.0 {
                let next_boundary = (c + 1) as f32 * CELL_VOXELS;
                (1, t_in + (next_boundary - (o + d * t_in)) / d, CELL_VOXELS / d)
            } else if d < 0.0 {
                let next_boundary = c as f32 * CELL_VOXELS;
                (-1, t_in + (next_boundary - (o + d * t_in)) / d, -CELL_VOXELS / d)
            } else {
                // Component is zero — this axis never crosses a boundary.
                (0, f32::INFINITY, f32::INFINITY)
            }
        };
        let (sx, tmx, tdx) = make_axis(origin.x, dir.x, cx);
        let (sy, tmy, tdy) = make_axis(origin.y, dir.y, cy);
        let (sz, tmz, tdz) = make_axis(origin.z, dir.z, cz);

        Self {
            cx, cy, cz,
            step_x: sx, step_y: sy, step_z: sz,
            t_max_x: tmx, t_max_y: tmy, t_max_z: tmz,
            t_delta_x: tdx, t_delta_y: tdy, t_delta_z: tdz,
            t_end,
            started: false,
            done: false,
        }
    }

    fn dead() -> Self {
        Self {
            cx: 0, cy: 0, cz: 0,
            step_x: 0, step_y: 0, step_z: 0,
            t_max_x: 0.0, t_max_y: 0.0, t_max_z: 0.0,
            t_delta_x: 0.0, t_delta_y: 0.0, t_delta_z: 0.0,
            t_end: 0.0,
            started: false,
            done: true,
        }
    }
}

impl Iterator for RayCellIter {
    type Item = (u32, u32, u32);

    fn next(&mut self) -> Option<(u32, u32, u32)> {
        if self.done { return None; }

        if !self.started {
            self.started = true;
            return Some((self.cx as u32, self.cy as u32, self.cz as u32));
        }

        // Advance to the next cell along the smallest t_max axis.
        let (next_t, axis) = if self.t_max_x <= self.t_max_y && self.t_max_x <= self.t_max_z {
            (self.t_max_x, 0)
        } else if self.t_max_y <= self.t_max_z {
            (self.t_max_y, 1)
        } else {
            (self.t_max_z, 2)
        };

        if next_t > self.t_end {
            self.done = true;
            return None;
        }

        match axis {
            0 => { self.cx += self.step_x; self.t_max_x += self.t_delta_x; }
            1 => { self.cy += self.step_y; self.t_max_y += self.t_delta_y; }
            _ => { self.cz += self.step_z; self.t_max_z += self.t_delta_z; }
        }

        // Out of world bounds → done.
        if self.cx < 0 || self.cy < 0 || self.cz < 0
           || self.cx >= MACRO_SIDE as i32
           || self.cy >= MACRO_SIDE as i32
           || self.cz >= MACRO_SIDE as i32 {
            self.done = true;
            return None;
        }

        Some((self.cx as u32, self.cy as u32, self.cz as u32))
    }
}

/// Ray–AABB slab intersection returning `(t_enter, t_exit)`. None if
/// the ray misses the box (or only touches behind the origin).
fn ray_aabb_t(origin: Vec3, dir: Vec3, mn: Vec3, mx: Vec3) -> Option<(f32, f32)> {
    let inv = (
        if dir.x != 0.0 { 1.0 / dir.x } else { f32::INFINITY },
        if dir.y != 0.0 { 1.0 / dir.y } else { f32::INFINITY },
        if dir.z != 0.0 { 1.0 / dir.z } else { f32::INFINITY },
    );
    let t1 = ((mn.x - origin.x) * inv.0, (mn.y - origin.y) * inv.1, (mn.z - origin.z) * inv.2);
    let t2 = ((mx.x - origin.x) * inv.0, (mx.y - origin.y) * inv.1, (mx.z - origin.z) * inv.2);
    let t_enter = t1.0.min(t2.0).max(t1.1.min(t2.1)).max(t1.2.min(t2.2));
    let t_exit  = t1.0.max(t2.0).min(t1.1.max(t2.1)).min(t1.2.max(t2.2));
    if t_enter <= t_exit && t_exit >= 0.0 {
        Some((t_enter, t_exit))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxlconsl_types::{U8Vec3, Vec3};

    use crate::actors::ActorTable;

    #[test]
    fn empty_grid_returns_empty_cell_lists() {
        let g = MacroGrid::new();
        for cz in 0..MACRO_SIDE {
            for cy in 0..MACRO_SIDE {
                for cx in 0..MACRO_SIDE {
                    assert!(g.cell_actors(cx, cy, cz).is_empty());
                }
            }
        }
        assert_eq!(g.entry_count(), 0);
    }

    #[test]
    fn single_actor_appears_in_its_cell() {
        let mut t = ActorTable::new();
        let id = t.spawn().unwrap();
        let a = t.get_mut(id).unwrap();
        a.position = Vec3::new(40.0, 40.0, 40.0);
        // Default 16³ volume → AABB extends to (40+16, 40+16, 40+16).
        // Cell range: floor(40/32)=1 to floor(56/32)=1. So only cell (1,1,1).

        let mut g = MacroGrid::new();
        g.rebuild(&t);

        assert_eq!(g.cell_actors(1, 1, 1), &[id.0]);
        assert!(g.cell_actors(0, 0, 0).is_empty());
        assert!(g.cell_actors(2, 2, 2).is_empty());
    }

    #[test]
    fn actor_spanning_two_cells_appears_in_both() {
        let mut t = ActorTable::new();
        let id = t.spawn().unwrap();
        let a = t.get_mut(id).unwrap();
        // 16³ default volume placed straddling cell boundary at x=32.
        a.position = Vec3::new(28.0, 0.0, 0.0);
        // AABB.x: [28, 44] → spans cells 0 and 1.
        // AABB.y: [0, 16] → cell 0.
        // AABB.z: [0, 16] → cell 0.

        let mut g = MacroGrid::new();
        g.rebuild(&t);

        assert_eq!(g.cell_actors(0, 0, 0), &[id.0]);
        assert_eq!(g.cell_actors(1, 0, 0), &[id.0]);
        assert!(g.cell_actors(2, 0, 0).is_empty());
    }

    #[test]
    fn invisible_actor_is_excluded() {
        let mut t = ActorTable::new();
        let id = t.spawn().unwrap();
        t.get_mut(id).unwrap().visible = false;

        let mut g = MacroGrid::new();
        g.rebuild(&t);
        assert_eq!(g.entry_count(), 0);
    }

    #[test]
    fn axial_ray_visits_cells_in_order() {
        let g = MacroGrid::new();
        // Ray straight along +X starting at (0.5, 16.0, 16.0).
        let cells: Vec<_> = g.ray_iter(
            Vec3::new(0.5, 16.0, 16.0),
            Vec3::new(1.0, 0.0, 0.0),
            10_000.0,
        ).collect();
        assert_eq!(cells.first().copied(), Some((0, 0, 0)));
        // Should walk every X cell from 0..32 along (cy=0, cz=0).
        assert_eq!(cells.len(), MACRO_SIDE as usize);
        for (i, &(cx, cy, cz)) in cells.iter().enumerate() {
            assert_eq!((cx, cy, cz), (i as u32, 0, 0));
        }
    }

    #[test]
    fn ray_starting_outside_world_clips_correctly() {
        let g = MacroGrid::new();
        let cells: Vec<_> = g.ray_iter(
            Vec3::new(-100.0, 16.0, 16.0),
            Vec3::new(1.0, 0.0, 0.0),
            10_000.0,
        ).collect();
        // Should still visit all 32 X cells once the ray enters the world.
        assert_eq!(cells.len(), MACRO_SIDE as usize);
        assert_eq!(cells[0], (0, 0, 0));
    }

    #[test]
    fn ray_missing_world_yields_no_cells() {
        let g = MacroGrid::new();
        // Origin above the world, direction +X (parallel to the world).
        let cells: Vec<_> = g.ray_iter(
            Vec3::new(0.0, 2000.0, 16.0),
            Vec3::new(1.0, 0.0, 0.0),
            10_000.0,
        ).collect();
        assert!(cells.is_empty());
    }

    #[test]
    fn diagonal_ray_visits_at_least_world_side_cells() {
        // A ray from (0,0,0) along (1,1,1) crosses the cube; AW yields
        // up to 3*MACRO_SIDE - 2 cells (each axis takes MACRO_SIDE-1
        // steps and they interleave).
        let g = MacroGrid::new();
        let cells: Vec<_> = g.ray_iter(
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(1.0, 1.0, 1.0),
            10_000.0,
        ).collect();
        assert!(cells.len() >= MACRO_SIDE as usize);
        assert_eq!(cells[0], (0, 0, 0));
        // Last cell should be near the far corner.
        let &(lx, ly, lz) = cells.last().unwrap();
        assert!(lx == MACRO_SIDE - 1 || ly == MACRO_SIDE - 1 || lz == MACRO_SIDE - 1);
    }

    #[test]
    fn rebuild_is_idempotent() {
        let mut t = ActorTable::new();
        let id = t.spawn().unwrap();
        t.get_mut(id).unwrap().position = Vec3::new(50.0, 50.0, 50.0);

        let mut g = MacroGrid::new();
        g.rebuild(&t);
        let before = g.entry_count();
        g.rebuild(&t);
        let after = g.entry_count();
        assert_eq!(before, after);
        assert_eq!(g.cell_actors(1, 1, 1), &[id.0]);
    }

    #[test]
    fn rebuild_after_actor_move_updates_cells() {
        let mut t = ActorTable::new();
        let id = t.spawn().unwrap();
        t.get_mut(id).unwrap().position = Vec3::new(0.0, 0.0, 0.0);
        let mut g = MacroGrid::new();
        g.rebuild(&t);
        assert_eq!(g.cell_actors(0, 0, 0), &[id.0]);
        assert!(g.cell_actors(2, 2, 2).is_empty());

        t.get_mut(id).unwrap().position = Vec3::new(64.0, 64.0, 64.0);
        g.rebuild(&t);
        assert!(g.cell_actors(0, 0, 0).is_empty());
        // 16³ volume at (64,64,64): AABB → cells 2..2.
        assert_eq!(g.cell_actors(2, 2, 2), &[id.0]);
    }
}

// Bytemuck helper trait used by the actors module to walk slots with
// their indices. Lives here to avoid cluttering ActorTable's public API
// with a function that's only used by macro_grid rebuild.
//
// Implemented in actors.rs as `impl ActorTable { pub fn for_each_visible_with_index... }`.
