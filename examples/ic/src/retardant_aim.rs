//! Retardant aim mode. The player anchors at a cell (via the action
//! wheel), then rotates a fixed-length preview line around the anchor
//! by moving the cursor. Confirm paints a salmon-pink retardant strip
//! along that line on the ground; Cancel discards. Unlike the
//! firetruck's line drafting there are only ever two points — the
//! anchor and a directional endpoint — and the length is locked, so
//! the cursor only chooses the angle.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics;

use crate::mathlib::sqrt;
use crate::terrain::{terrain_height, FOOT_MAX};
use crate::{
    M_EMBER, M_FIRE, M_PINE_LEAVES, M_PINE_WOOD, M_PLANNED_RETARDANT, M_RETARDANT,
};

/// Strip length in cells. The cursor only chooses direction — the
/// strip is always this long, clipped only by the world edge.
pub(crate) const RETARDANT_LENGTH: u32 = 30;
/// Half-width in cells. 1 → 3 cells wide, matching the firetruck's
/// firebreak strip so the two play in similar units.
const RETARDANT_HALF_WIDTH: i32 = 1;
/// Vertical offset above terrain where preview voxels float — same
/// height as the fire-line preview so the two read consistently.
const PREVIEW_Y_OFFSET: u32 = 2;
/// Upper bound on preview cells we'll track for clearing. The strip
/// is 30 cells long; budget twice that for diagonal walks.
const PREVIEW_CAP: usize = 64;

pub(crate) struct RetardantAim {
    pub anchor: UVec3,
    pub active: bool,
    /// Last painted direction in *reduced integer cell* form. Two
    /// cursor positions on the same ray from the anchor reduce to
    /// the same key, so we don't repaint as the player slides the
    /// cursor along the chosen direction.
    last_dir_q: Option<(i32, i32)>,
    preview:    [Option<UVec3>; PREVIEW_CAP],
    preview_count: u16,
}

impl RetardantAim {
    pub(crate) const fn new() -> Self {
        Self {
            anchor: UVec3 { x: 0, y: 0, z: 0 },
            active: false,
            last_dir_q: None,
            preview: [None; PREVIEW_CAP],
            preview_count: 0,
        }
    }

    /// Player picked RETARDANT from the wheel — enter aim mode with
    /// `anchor` as the strip's start point.
    pub(crate) fn begin(&mut self, anchor: UVec3) {
        self.clear_preview();
        self.anchor = anchor;
        self.active = true;
        self.last_dir_q = None;
    }

    /// Called every frame while aiming. Computes the reduced
    /// direction from `anchor` to the cursor and re-paints the
    /// preview line only when that key changes.
    pub(crate) fn aim_at(&mut self, cursor: UVec3) {
        if !self.active { return; }
        let dx = cursor.x as i32 - self.anchor.x as i32;
        let dz = cursor.z as i32 - self.anchor.z as i32;
        if dx == 0 && dz == 0 {
            if self.last_dir_q.is_some() {
                self.clear_preview();
                self.last_dir_q = None;
            }
            return;
        }
        let g = gcd(dx.unsigned_abs(), dz.unsigned_abs()).max(1) as i32;
        let qdir = (dx / g, dz / g);
        if self.last_dir_q == Some(qdir) { return; }
        self.last_dir_q = Some(qdir);
        self.clear_preview();
        let mag = sqrt((dx * dx + dz * dz) as f32);
        let dir = (dx as f32 / mag, dz as f32 / mag);
        self.paint_preview(dir);
    }

    fn paint_preview(&mut self, dir: (f32, f32)) {
        for i in 0..RETARDANT_LENGTH {
            let xf = self.anchor.x as f32 + dir.0 * i as f32;
            let zf = self.anchor.z as f32 + dir.1 * i as f32;
            if xf < 0.0 || zf < 0.0 { continue; }
            let xu = xf as u32;
            let zu = zf as u32;
            if xu >= FOOT_MAX || zu >= FOOT_MAX { continue; }
            let y = terrain_height(xu, zu) + PREVIEW_Y_OFFSET;
            if physics::material_at(xu, y, zu) != 0 { continue; }
            set_voxel(UVec3::new(xu, y, zu), M_PLANNED_RETARDANT);
            if (self.preview_count as usize) < PREVIEW_CAP {
                self.preview[self.preview_count as usize] = Some(UVec3::new(xu, y, zu));
                self.preview_count += 1;
            }
        }
    }

    fn clear_preview(&mut self) {
        for i in 0..self.preview_count as usize {
            if let Some(p) = self.preview[i] {
                if physics::material_at(p.x, p.y, p.z) == M_PLANNED_RETARDANT {
                    set_voxel(p, 0);
                }
            }
            self.preview[i] = None;
        }
        self.preview_count = 0;
    }

    /// Cancel without painting the real strip.
    pub(crate) fn discard(&mut self) {
        self.active = false;
        self.last_dir_q = None;
        self.clear_preview();
    }

    /// Commit. Returns `Some(anchor, dir)` for the caller to hand
    /// off to the retardant tanker. The preview voxels are left in
    /// place — the dispatched tanker will overwrite them cell by
    /// cell as it crosses the strip — but our own tracking handles
    /// are cleared so a future aim doesn't try to wipe voxels the
    /// tanker has since rewritten.
    ///
    /// Returns `None` if the player never aimed at a cell distinct
    /// from the anchor, in which case there's nothing painted to
    /// either commit or clean up.
    pub(crate) fn commit(&mut self) -> Option<(UVec3, (f32, f32))> {
        if !self.active { return None; }
        self.active = false;
        let qd = self.last_dir_q.take();
        for slot in &mut self.preview { *slot = None; }
        self.preview_count = 0;
        let (dx, dz) = qd?;
        let mag = sqrt((dx * dx + dz * dz) as f32);
        Some((self.anchor, (dx as f32 / mag, dz as f32 / mag)))
    }
}

/// Clear lingering `M_PLANNED_RETARDANT` voxels along `(anchor, dir)`.
/// Called by the cart on the failure path of `dispatch_retardant_strip`
/// (tanker pool full) so the preview doesn't permanently haunt the
/// airspace when no plane is coming to overwrite it.
pub(crate) fn clear_planned_voxels_along(anchor: UVec3, dir: (f32, f32)) {
    for i in 0..RETARDANT_LENGTH {
        let xf = anchor.x as f32 + dir.0 * i as f32;
        let zf = anchor.z as f32 + dir.1 * i as f32;
        if xf < 0.0 || zf < 0.0 { continue; }
        let xu = xf as u32;
        let zu = zf as u32;
        if xu >= FOOT_MAX || zu >= FOOT_MAX { continue; }
        let y = terrain_height(xu, zu) + PREVIEW_Y_OFFSET;
        if physics::material_at(xu, y, zu) == M_PLANNED_RETARDANT {
            set_voxel(UVec3::new(xu, y, zu), 0);
        }
    }
}

/// Paint the committed retardant strip on the ground instantly:
/// 3 cells wide, `RETARDANT_LENGTH` long, starting at `anchor` and
/// extending along `dir`. Replaces the terrain cap with `M_RETARDANT`
/// and clears flammables (and the planning marker) in the column
/// above so the strip reads as a real fire-block.
///
/// Used as the fallback when `dispatch_retardant_strip` can't find a
/// free tanker slot — the player's order isn't silently dropped, it
/// just lands without the plane animation.
pub(crate) fn paint_strip(anchor: UVec3, dir: (f32, f32)) {
    // Perpendicular to (dir.0, dir.1) is (-dir.1, dir.0) — points 90°
    // CCW. Width is symmetric around the line so the sign of the
    // perpendicular doesn't matter.
    let pdx = -dir.1;
    let pdz = dir.0;
    for i in 0..RETARDANT_LENGTH {
        let cxf = anchor.x as f32 + dir.0 * i as f32;
        let czf = anchor.z as f32 + dir.1 * i as f32;
        for w in -RETARDANT_HALF_WIDTH..=RETARDANT_HALF_WIDTH {
            let xf = cxf + pdx * w as f32;
            let zf = czf + pdz * w as f32;
            if xf < 0.0 || zf < 0.0 { continue; }
            let xu = xf as u32;
            let zu = zf as u32;
            if xu >= FOOT_MAX || zu >= FOOT_MAX { continue; }
            let h = terrain_height(xu, zu);
            if h == 0 { continue; }
            set_voxel(UVec3::new(xu, h - 1, zu), M_RETARDANT);
            for y in h..h + 6 {
                let m = physics::material_at(xu, y, zu);
                if m == M_PINE_WOOD || m == M_PINE_LEAVES
                    || m == M_FIRE || m == M_EMBER
                {
                    set_voxel(UVec3::new(xu, y, zu), 0);
                }
            }
            // Also clear the planning marker so the floating preview
            // doesn't linger above the now-painted strip.
            let preview_y = h + PREVIEW_Y_OFFSET;
            if physics::material_at(xu, preview_y, zu) == M_PLANNED_RETARDANT {
                set_voxel(UVec3::new(xu, preview_y, zu), 0);
            }
        }
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}
