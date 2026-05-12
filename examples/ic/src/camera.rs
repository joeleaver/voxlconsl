//! RTS-style overhead camera. Fixed yaw (north-up), fixed tilt;
//! pan + zoom only.
//!
//! `focus` is the world-space anchor the eye looks at. WASD shifts
//! `focus` in the XZ plane at a speed proportional to camera
//! distance — at far zoom you can flick across the whole map; at
//! close zoom panning feels delicate.

use voxlconsl_sdk::*;

use crate::mathlib::{cosine, sine};
use crate::terrain::{terrain_height, FOOT_MAX, FOOT_MIN};

pub(crate) struct Camera {
    pub focus_x: f32,
    pub focus_z: f32,
    pub distance: f32,
}

pub(crate) const DIST_MIN: f32 = 50.0;
pub(crate) const DIST_MAX: f32 = 260.0;
/// Fraction of current distance applied per wheel notch.
const ZOOM_PER_NOTCH: f32 = 0.18;

/// Camera looks "from above and south" at the focus point. tilt is
/// measured from horizontal — 60° means eye is well above ground.
const TILT_RAD: f32 = 0.95;        // ~54°
const FOV_DEG:  f32 = 35.0;        // telephoto / iso-ish

/// Pan speed in voxels per axis-unit per second, scaled by zoom.
const PAN_SPEED_BASE: f32 = 0.45;

impl Camera {
    pub(crate) const fn new(focus_x: f32, focus_z: f32) -> Self {
        Self { focus_x, focus_z, distance: 150.0 }
    }

    /// Apply one frame of input: `(mx, my)` is the pan axis (left
    /// stick / WASD), `zoom_delta` is the wheel scroll (positive =
    /// scroll up = zoom in). `dt` is seconds.
    pub(crate) fn update(&mut self, mx: f32, my: f32, zoom_delta: f32, dt: f32) {
        let speed = PAN_SPEED_BASE * self.distance * dt;
        // WASD: W moves north (-Z), D moves east (+X). Axis2D
        // convention: my is forward (+1 = up).
        self.focus_x += mx * speed;
        self.focus_z -= my * speed;
        self.clamp_focus();

        if zoom_delta != 0.0 {
            self.distance = (self.distance * (1.0 - zoom_delta * ZOOM_PER_NOTCH))
                .clamp(DIST_MIN, DIST_MAX);
        }
    }

    fn clamp_focus(&mut self) {
        self.focus_x = self.focus_x.clamp(FOOT_MIN as f32 + 8.0, FOOT_MAX as f32 - 8.0);
        self.focus_z = self.focus_z.clamp(FOOT_MIN as f32 + 8.0, FOOT_MAX as f32 - 8.0);
    }

    /// Push the camera state to the host renderer. Eye sits above and
    /// south of focus; up is -Z so north points up the screen.
    pub(crate) fn apply(&self) {
        let height = self.distance * sine(TILT_RAD);
        let back   = self.distance * cosine(TILT_RAD);
        let fy = terrain_height(self.focus_x as u32, self.focus_z as u32) as f32;
        let eye = Vec3::new(self.focus_x, fy + height, self.focus_z + back);
        let target = Vec3::new(self.focus_x, fy, self.focus_z);
        camera_set_lookat(eye, target, Vec3::new(0.0, 0.0, -1.0));
        camera_set_fov(FOV_DEG);
    }

    /// Cursor pan speed scales with camera distance — at far zoom
    /// the cursor needs to cross more world distance per mouse-pixel
    /// of motion.
    pub(crate) fn cursor_speed(&self) -> f32 {
        // 0.07 cells per mouse-axis-unit at min distance, ~0.35 at
        // max. Tuned by feel on the browser host.
        0.07 + (self.distance - DIST_MIN) / (DIST_MAX - DIST_MIN) * 0.28
    }
}
