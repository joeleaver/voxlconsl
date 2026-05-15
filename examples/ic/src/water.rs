//! Cart-side TTL tracking for player-dropped water.
//!
//! Dropped water uses `M_WATER` (liquid, flows naturally via the
//! §10.3 CA). The lake uses `M_LAKE_WATER` (slot 45, no liquid
//! flag, same visual) — that's how the cart's clearing pass stays
//! intrinsically lake-safe: it only looks at `M_WATER` cells.
//!
//! Per cell: register at drop time with a TTL randomised in
//! `[WATER_TTL_MIN, WATER_TTL_MAX]`. The spread means a single
//! drop's ~25 cells expire spread across ~2 seconds instead of
//! popping out in one frame. On each expiry we clear a small box
//! of `M_WATER` around the tracked cell — catches the cell itself
//! plus any water that flowed a few cells off the spawn point.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics::material_at;

use crate::M_WATER;
use crate::rng::Rng;

/// Lower / upper bound on per-cell evaporation time in frames @ 60 fps.
/// 240..360 ⇒ 4..6 s.  Each `mark` picks a random TTL in this range so
/// the cells of a single drop disappear over the 2 s spread rather
/// than vanishing all at once.
const WATER_TTL_MIN: u32 = 240;
const WATER_TTL_MAX: u32 = 360;

/// Cap on simultaneously-tracked water cells. With 5 s TTL the
/// rolling population is small; 1024 is plenty even for
/// multi-tanker bursts.
const CAP: usize = 1024;

pub(crate) struct WaterTracker {
    cells: [Option<(UVec3, u32)>; CAP],
    rng:   Rng,
}

impl WaterTracker {
    pub(crate) const fn new() -> Self {
        Self {
            cells: [None; CAP],
            rng:   Rng(0x57A7_E51E),
        }
    }

    /// Track a cell the cart just placed. Re-marking refreshes the
    /// TTL with a fresh random pick. Cap-evict replaces the
    /// lowest-TTL slot when full.
    pub(crate) fn mark(&mut self, pos: UVec3) {
        let ttl = self.rand_ttl();
        for slot in self.cells.iter_mut() {
            if let Some((p, t)) = slot {
                if *p == pos { *t = ttl; return; }
            }
        }
        let mut worst_idx = 0usize;
        let mut worst_ttl = u32::MAX;
        for (i, slot) in self.cells.iter_mut().enumerate() {
            match slot {
                None => { *slot = Some((pos, ttl)); return; }
                Some((_, t)) => {
                    if *t < worst_ttl { worst_ttl = *t; worst_idx = i; }
                }
            }
        }
        self.cells[worst_idx] = Some((pos, ttl));
    }

    /// Decrement every entry's TTL; on expiry, clear water near the
    /// tracked cell.
    pub(crate) fn tick(&mut self) {
        for slot in self.cells.iter_mut() {
            if let Some((pos, ttl)) = *slot {
                if ttl == 0 {
                    evaporate_local(pos);
                    *slot = None;
                    continue;
                }
                *slot = Some((pos, ttl - 1));
            }
        }
    }

    fn rand_ttl(&mut self) -> u32 {
        let span = WATER_TTL_MAX - WATER_TTL_MIN;
        WATER_TTL_MIN + (self.rng.next_u32() % span)
    }
}

/// Clear any `M_WATER` cells in a box around `centre`. Sized
/// generously: a heli drop spreads 5×5 spawn cells to roughly
/// 9-11 cells across after settling, and on slopes water can run
/// several cells downhill before pooling. R=5 lateral covers the
/// typical spread; DOWN=5 catches downhill runs. Per-expiry the
/// box opens a hole; combined with the randomised per-cell TTLs
/// the pool dissolves gradually instead of vanishing in one frame.
/// The lake is `M_LAKE_WATER` (separate slot) so it's never touched.
fn evaporate_local(centre: UVec3) {
    const R: i32 = 5;
    const DOWN: i32 = 5;
    const UP: i32 = 2;
    let cx = centre.x as i32;
    let cy = centre.y as i32;
    let cz = centre.z as i32;
    for dy in -DOWN..=UP {
        for dz in -R..=R {
            for dx in -R..=R {
                let x = cx + dx;
                let y = cy + dy;
                let z = cz + dz;
                if x < 0 || y < 0 || z < 0 { continue; }
                let xu = x as u32;
                let yu = y as u32;
                let zu = z as u32;
                if material_at(xu, yu, zu) == M_WATER {
                    set_voxel(UVec3::new(xu, yu, zu), 0);
                }
            }
        }
    }
}
