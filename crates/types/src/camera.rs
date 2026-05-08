//! Camera projection — see SPEC.md §3.2.

#[derive(Copy, Clone, Debug)]
pub enum Projection {
    /// Standard 3D perspective. `fov_y_deg` clamped to 30°..120° by the host.
    Perspective { fov_y_deg: f32 },
    /// Parallel projection. `height` is viewport height in world voxels.
    Orthographic { height: f32 },
    /// Fixed 30°/45° iso angles. `scale` is pixels per voxel.
    Isometric { scale: f32 },
}

impl Default for Projection {
    fn default() -> Self {
        Self::Perspective { fov_y_deg: 60.0 }
    }
}
