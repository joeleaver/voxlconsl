//! Fire-line drafting mode. The player toggles in with SECONDARY (K),
//! drops points with PRIMARY (J), and commits the draft with CONFIRM
//! (Enter). Each drafted point gets a Billboard marker so the polyline
//! is visible while it's being assembled. On commit, the points are
//! handed to the Roster as a single FireLine command — the queue takes
//! it from there.
//!
//! Cancelling the draft (toggling out via SECONDARY again before
//! committing) discards the points without enqueueing — the draft
//! never reached the queue, so it isn't a "cancellation" of any
//! committed order.

use voxlconsl_sdk::*;
use voxlconsl_sdk::physics;

use crate::terrain::terrain_height;
use crate::{M_PLANNED_LINE, M_SELECT_MARKER};

/// Max points in a single fire-line draft. Must equal
/// `units::CREW_PATH_CAP` so the crew can walk the full polyline
/// without truncation.
pub(crate) const LINE_CAP: usize = 8;

/// Max preview voxels painted while a fire-line is being drafted.
/// Sized for 8 points (≤ 7 segments) × ~32 cells per segment.
const PREVIEW_VOXELS_CAP: usize = 256;

/// Vertical offset above terrain for the preview voxels — keeps
/// them floating in air so the cart never overwrites grass / trees
/// while the player is drafting.
const PREVIEW_Y_OFFSET: u32 = 2;

const MARKER_W: u8 = 3;
const MARKER_H: u8 = 3;
const MARKER_PREFAB: PrefabId = PrefabId(67);
const MARKER_VOL_BYTES: usize = (MARKER_W as usize) * (MARKER_H as usize);
const MARKER_BITMAP: [[u8; MARKER_W as usize]; MARKER_H as usize] = [
    [0, 1, 0],
    [1, 1, 1],
    [0, 1, 0],
];
static mut MARKER_DENSE: [u8; MARKER_VOL_BYTES] = [0; MARKER_VOL_BYTES];

pub(crate) struct LineMode {
    pub active: bool,
    pub count:  u8,
    points:     [UVec3; LINE_CAP],
    markers:    [Option<ActorId>; LINE_CAP],
    /// Voxel cells painted with `M_PLANNED_LINE` for the preview
    /// segments. We track every painted cell so `clear()` can reset
    /// it back to air without leaving stray magenta voxels in the
    /// world.
    preview:    [Option<UVec3>; PREVIEW_VOXELS_CAP],
    preview_count: u16,
}

impl LineMode {
    pub(crate) const fn new() -> Self {
        Self {
            active:  false,
            count:   0,
            points:  [UVec3 { x: 0, y: 0, z: 0 }; LINE_CAP],
            markers: [None; LINE_CAP],
            preview: [None; PREVIEW_VOXELS_CAP],
            preview_count: 0,
        }
    }

    /// Register the marker prefab + spawn the actor pool once at
    /// boot. Each marker starts hidden; `push_point` re-positions
    /// and shows them as the player drafts.
    pub(crate) fn init(&mut self) {
        unsafe {
            let dense = &mut *(&raw mut MARKER_DENSE);
            for (row_idx, row) in MARKER_BITMAP.iter().enumerate() {
                for (col_idx, &on) in row.iter().enumerate() {
                    if on == 0 { continue; }
                    let lx = col_idx;
                    let ly = (MARKER_H as usize - 1) - row_idx;
                    let i = ly * MARKER_W as usize + lx;
                    dense[i] = M_SELECT_MARKER;
                }
            }
            prefab_define(
                MARKER_PREFAB,
                &*(&raw const MARKER_DENSE),
                U8Vec3::new(MARKER_W, MARKER_H, 1),
            );
        }
        for slot in &mut self.markers {
            let id = actor_spawn_from(MARKER_PREFAB, Orientation::Up)
                .expect("ic line marker actor spawn");
            actor_set_render_mode(id, ActorRenderMode::Billboard);
            actor_set_visible(id, false);
            *slot = Some(id);
        }
    }

    /// Append a point to the draft. Beyond `LINE_CAP` points the
    /// click is silently ignored (the player has more than they can
    /// queue — they can commit and start a new line).
    ///
    /// When this is the 2nd+ point, also paint a preview segment of
    /// `M_PLANNED_LINE` voxels along the cells between the previous
    /// point and the new one. The voxels float at
    /// `terrain_height + PREVIEW_Y_OFFSET` so they never overwrite
    /// terrain or trees during drafting.
    pub(crate) fn push_point(&mut self, p: UVec3) {
        if (self.count as usize) >= LINE_CAP { return; }
        if self.count > 0 {
            let prev = self.points[(self.count - 1) as usize];
            self.paint_segment(prev, p);
        }
        self.points[self.count as usize] = p;
        if let Some(actor) = self.markers[self.count as usize] {
            // Anchor the marker just above the cell so it floats
            // visibly over the terrain.
            actor_set_position(
                actor,
                Vec3::new(p.x as f32 + 0.5, p.y as f32 + 2.0, p.z as f32 + 0.5),
            );
            actor_set_visible(actor, true);
        }
        self.count += 1;
    }

    /// Paint floating magenta voxels along the cells between two
    /// drafted points. Skips the endpoints (they already have
    /// Billboard markers) and only writes into air cells so the
    /// preview can't blast over terrain or trees the player will
    /// eventually want their crew to clear.
    fn paint_segment(&mut self, a: UVec3, b: UVec3) {
        let dx = b.x as i32 - a.x as i32;
        let dz = b.z as i32 - a.z as i32;
        let dx_abs = if dx < 0 { -dx } else { dx };
        let dz_abs = if dz < 0 { -dz } else { dz };
        let steps = dx_abs.max(dz_abs).max(1);
        for i in 1..steps {
            let t = i as f32 / steps as f32;
            let x = (a.x as f32 + dx as f32 * t) as u32;
            let z = (a.z as f32 + dz as f32 * t) as u32;
            let y = terrain_height(x, z) + PREVIEW_Y_OFFSET;
            if physics::material_at(x, y, z) != 0 { continue; }
            set_voxel(UVec3::new(x, y, z), M_PLANNED_LINE);
            if (self.preview_count as usize) < PREVIEW_VOXELS_CAP {
                self.preview[self.preview_count as usize] = Some(UVec3::new(x, y, z));
                self.preview_count += 1;
            }
        }
    }

    /// Hide every draft marker, clear any painted preview voxels,
    /// and reset the count. Called when the draft is committed or
    /// cancelled.
    pub(crate) fn clear(&mut self) {
        for i in 0..self.count as usize {
            if let Some(actor) = self.markers[i] {
                actor_set_visible(actor, false);
            }
        }
        self.count = 0;
        for i in 0..self.preview_count as usize {
            if let Some(p) = self.preview[i] {
                // Only clear if it's still our preview voxel; some
                // other system may have overwritten it in the meantime
                // (a stray ember, etc).
                if physics::material_at(p.x, p.y, p.z) == M_PLANNED_LINE {
                    set_voxel(p, 0);
                }
            }
            self.preview[i] = None;
        }
        self.preview_count = 0;
    }

    /// Copy the current draft into `dst`, returning the number of
    /// points written. Used by the cart to hand the polyline off to
    /// `Roster::dispatch_fire_line`.
    pub(crate) fn copy_points_into(&self, dst: &mut [UVec3; LINE_CAP]) -> usize {
        let n = self.count as usize;
        for i in 0..n { dst[i] = self.points[i]; }
        n
    }
}
