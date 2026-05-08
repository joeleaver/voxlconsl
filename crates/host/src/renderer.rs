//! Renderer — see SPEC.md §3.
//!
//! v0.0.2: a single-chunk pinhole-camera ray marcher. Renders a 256×144
//! framebuffer using the system palette + a hand-rolled flat-shaded lighting
//! model (sun + ambient, no shadows yet). Materials are looked up directly
//! from the cart's material table.
//!
//! TODO progression toward full §3:
//!   - Multi-chunk world (§13.6)
//!   - Actor compositing (§11.6)
//!   - Real lighting model with shadows (§3.3)
//!   - Sky gradient + sun disc (§3.4)
//!   - Camera projections beyond perspective (§3.2)

use voxlconsl_svo::{ChunkData, ray::RayHit};
use voxlconsl_types::{Material, Vec3};

use crate::palette::{SYSTEM_PALETTE, lit_color_index};

pub const WIDTH: u32 = 256;
pub const HEIGHT: u32 = 144;

/// Pinhole camera. v0.0.2 uses look-at + a vertical FOV; matches §3.2's
/// `Projection::Perspective` with `camera_set_lookat`.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub fov_y_deg: f32,
}

impl Camera {
    pub fn new(eye: Vec3, target: Vec3, fov_y_deg: f32) -> Self {
        Self { eye, target, up: Vec3::Y, fov_y_deg }
    }
}

/// World state the renderer reads from. v0.0.2 holds a single chunk;
/// expand to a multi-chunk grid + actors later.
pub struct Scene<'a> {
    pub chunk: &'a ChunkData,
    /// Origin of the chunk in world coordinates. With a single chunk we'll
    /// usually keep this at zero and look at it from outside.
    pub chunk_origin: Vec3,
    pub materials: &'a [Material; 256],
    pub sun_dir: Vec3,
    /// Sky color shown when a ray misses everything. Palette index.
    pub sky: u8,
}

/// Render one frame into `framebuffer`. Buffer is RGBA8 row-major,
/// `WIDTH * HEIGHT * 4` bytes. Buffer length is checked.
pub fn render_frame(scene: &Scene, camera: &Camera, framebuffer: &mut [u8]) {
    assert_eq!(
        framebuffer.len(),
        (WIDTH * HEIGHT * 4) as usize,
        "framebuffer size mismatch"
    );

    let basis = camera_basis(camera);
    let aspect = WIDTH as f32 / HEIGHT as f32;
    let half_h = (camera.fov_y_deg.to_radians() * 0.5).tan();
    let half_w = half_h * aspect;

    let sun_dir = scene.sun_dir.normalize();
    let sky_rgb = SYSTEM_PALETTE[scene.sky.min(63) as usize];

    for py in 0..HEIGHT {
        // NDC y goes top-to-bottom; flip so +Y is up.
        let v = ((py as f32 + 0.5) / HEIGHT as f32) * 2.0 - 1.0;
        for px in 0..WIDTH {
            let u = ((px as f32 + 0.5) / WIDTH as f32) * 2.0 - 1.0;

            // Ray direction in world space.
            let dir = (basis.forward
                + basis.right * (u * half_w)
                + basis.up * (-v * half_h))
                .normalize();

            // Translate ray into chunk-local space (chunk occupies [0, 32]).
            let local_origin = camera.eye - scene.chunk_origin;

            let color = match scene.chunk.raycast(local_origin, dir, 1024.0) {
                Some(hit) => shade(hit, scene, sun_dir),
                None => sky_rgb,
            };

            let i = ((py * WIDTH + px) * 4) as usize;
            framebuffer[i]     = color.r;
            framebuffer[i + 1] = color.g;
            framebuffer[i + 2] = color.b;
            framebuffer[i + 3] = 255;
        }
    }
}

/// Translate a hit + lighting into a final palette color.
fn shade(hit: RayHit, scene: &Scene, sun_dir: Vec3) -> voxlconsl_types::PaletteColor {
    let m = scene.materials[hit.material as usize];

    // Convert the hit's integer face normal to a Vec3 for lighting math.
    let n = Vec3::new(hit.normal.0 as f32, hit.normal.1 as f32, hit.normal.2 as f32);

    // Ambient (always at least one shade above black) + directional dot product.
    // Sun_dir points *toward* the sun; positive dot means the surface faces it.
    let ndotl = n.dot(sun_dir).max(0.0);
    // 0.0 → ambient only (shade index 1), 1.0 → fully lit (shade index 3).
    let brightness = 0.35 + 0.65 * ndotl;

    // Brighten by emission. Emissive surfaces always render at shade 3.
    let shade_idx = if m.emission > 0 {
        3
    } else {
        // Map brightness in [0, 1] to shade index 0..3.
        (brightness * 4.0).min(3.0) as u8
    };

    let lit_idx = lit_color_index(m.color, shade_idx);
    SYSTEM_PALETTE[lit_idx as usize]
}

struct Basis {
    forward: Vec3,
    right: Vec3,
    up: Vec3,
}

fn camera_basis(camera: &Camera) -> Basis {
    let forward = (camera.target - camera.eye).normalize();
    let right = forward.cross(camera.up).normalize();
    let up = right.cross(forward).normalize();
    Basis { forward, right, up }
}
