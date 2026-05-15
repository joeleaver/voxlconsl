//! Cart-owned 3D cursor — the tile the player is currently pointing
//! at. Driven by mouse-delta (Aim hint); rendered as a Billboard
//! actor so the reticle stays pixel-crisp 1:1 regardless of camera
//! zoom or tilt.

use voxlconsl_sdk::*;

use crate::terrain::{terrain_height, FOOT_MAX, FOOT_MIN};
use crate::{M_CURSOR_MARKER};

// ── Reticle prefab ────────────────────────────────────────────────

const CURSOR_W: u8 = 7;
const CURSOR_H: u8 = 7;
const CURSOR_VOL_BYTES: usize = (CURSOR_W as usize) * (CURSOR_H as usize) * 1;
const CURSOR_PREFAB: PrefabId = PrefabId(65);

// 7×7 crosshair: four "+" arms with a 1-px gap in the middle so the
// thing the cursor is pointing at stays visible.
// Reading top-to-bottom (row 0 = top of glyph = highest local Y).
const CURSOR_BITMAP: [[u8; CURSOR_W as usize]; CURSOR_H as usize] = [
    [0, 0, 0, 1, 0, 0, 0],
    [0, 0, 0, 1, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0],
    [1, 1, 0, 0, 0, 1, 1],
    [0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 1, 0, 0, 0],
    [0, 0, 0, 1, 0, 0, 0],
];

static mut CURSOR_DENSE: [u8; CURSOR_VOL_BYTES] = [0; CURSOR_VOL_BYTES];

pub(crate) struct Cursor {
    pub x: f32,
    pub z: f32,
    actor: Option<ActorId>,
}

impl Cursor {
    pub(crate) const fn new(x: f32, z: f32) -> Self {
        Self { x, z, actor: None }
    }

    /// Reposition the cursor without disturbing its host actor.
    /// Used by `lib::restart_season()` so the actor allocated on
    /// first boot is reused across endless restarts.
    pub(crate) fn set_focus(&mut self, x: f32, z: f32) {
        self.x = x;
        self.z = z;
    }

    /// Register the reticle prefab and spawn a Billboard actor for
    /// the cursor. Called once after the world is built. Idempotent
    /// — endless-mode restarts call this again, but the actor stays.
    pub(crate) fn init(&mut self) {
        if self.actor.is_some() { return; }
        unsafe {
            // Convert the row-major bitmap into the prefab's
            // (x, y, z) layout. Bitmap row 0 (top of glyph) should
            // end up at the highest local Y so `Axis::XY` painters
            // stay convention-consistent — the engine's Billboard
            // blit flips Y back so the top row paints at the top of
            // the rect on screen.
            let dense = &mut *(&raw mut CURSOR_DENSE);
            for (row_idx, row) in CURSOR_BITMAP.iter().enumerate() {
                for (col_idx, &on) in row.iter().enumerate() {
                    if on == 0 { continue; }
                    let lx = col_idx;
                    let ly = (CURSOR_H as usize - 1) - row_idx;
                    let i = ((0 * CURSOR_H as usize) + ly) * CURSOR_W as usize + lx;
                    dense[i] = M_CURSOR_MARKER;
                }
            }
            prefab_define(
                CURSOR_PREFAB,
                &*(&raw const CURSOR_DENSE),
                U8Vec3::new(CURSOR_W, CURSOR_H, 1),
            );
        }
        let id = actor_spawn_from(CURSOR_PREFAB, Orientation::Up)
            .expect("ic cursor actor spawn");
        actor_set_render_mode(id, ActorRenderMode::Billboard);
        self.actor = Some(id);
    }

    /// `(ax, ay)` is mouse delta (Aim hint). `speed` comes from the
    /// camera so cursor sensitivity scales with zoom. Positive `ay`
    /// = mouse moved down → cursor pans south (+Z).
    pub(crate) fn pan(&mut self, ax: f32, ay: f32, speed: f32) {
        self.x = (self.x + ax * speed).clamp(FOOT_MIN as f32 + 4.0, FOOT_MAX as f32 - 4.0);
        self.z = (self.z + ay * speed).clamp(FOOT_MIN as f32 + 4.0, FOOT_MAX as f32 - 4.0);
    }

    /// Slide the cursor by the camera's focus delta so WASD-pan keeps
    /// the reticle anchored to the same screen position. Without this
    /// the cursor stays put in world-space and walks off the viewport
    /// as the camera pans.
    pub(crate) fn follow_camera(&mut self, dx: f32, dz: f32) {
        self.x = (self.x + dx).clamp(FOOT_MIN as f32 + 4.0, FOOT_MAX as f32 - 4.0);
        self.z = (self.z + dz).clamp(FOOT_MIN as f32 + 4.0, FOOT_MAX as f32 - 4.0);
    }

    /// Integer cell the cursor currently points at.
    pub(crate) fn cell(&self) -> (u32, u32) { (self.x as u32, self.z as u32) }

    /// Y of the cursor's marker voxel — one cell above the terrain
    /// surface at the cursor's (x, z).
    pub(crate) fn marker_y(&self) -> u32 {
        let (x, z) = self.cell();
        terrain_height(x, z)
    }

    /// Re-anchor the Billboard actor to the cursor's world point.
    /// Called once per frame after `pan`.
    pub(crate) fn render(&mut self) {
        if let Some(actor) = self.actor {
            let (cx, cz) = self.cell();
            let cy = self.marker_y();
            // Anchor a couple of cells above the ground so the cross
            // sits above terrain/units rather than on top of them.
            actor_set_position(
                actor,
                Vec3::new(cx as f32 + 0.5, cy as f32 + 1.5, cz as f32 + 0.5),
            );
        }
    }
}
