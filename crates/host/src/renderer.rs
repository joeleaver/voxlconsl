//! Renderer — see SPEC.md §3.
//!
//! v0.0.7: a single-chunk pinhole-camera ray marcher with actor compositing
//! and macro-grid actor binning (§11.6). Per-ray, we walk the macro-grid
//! along the ray's path and gather only the actors whose AABBs overlap
//! cells the ray actually enters; each candidate gets the same AABB →
//! SVO-raycast pipeline as before. With one populated chunk in the
//! current demo every actor is in cell (0, 0, 0), so the savings are
//! purely structural — when multi-chunk worlds (§13.6) land the
//! per-cell filtering becomes the real win.
//!
//! TODO progression toward full §3:
//!   - Multi-chunk world (§13.6) + per-chunk DDA across the macro-grid
//!   - Real lighting model with shadows (§3.3)
//!   - Sky gradient + sun disc (§3.4)
//!   - Camera projections beyond perspective (§3.2)

use voxlconsl_svo::{ChunkData, ray::RayHit};
use voxlconsl_types::{ActorId, Material, Vec3};

use crate::actors::ActorTable;
use crate::macro_grid::MacroGrid;
use crate::palette::{SYSTEM_PALETTE, lit_color_index};

pub const WIDTH: u32 = 256;
pub const HEIGHT: u32 = 144;

/// Pinhole camera. Matches §3.2's `Projection::Perspective` with `camera_set_lookat`.
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

/// World state the renderer reads from.
pub struct Scene<'a> {
    pub chunk: &'a ChunkData,
    pub chunk_origin: Vec3,
    pub actors: &'a ActorTable,
    pub macro_grid: &'a MacroGrid,
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

    // Pre-compute world AABBs for every visible actor so the per-ray
    // pass doesn't redo the corner transforms. The actor index is the
    // slot index, matching what the macro-grid stores.
    let mut actor_aabbs: Vec<(Vec3, Vec3)> = Vec::with_capacity(scene.actors.capacity());
    actor_aabbs.resize(scene.actors.capacity(), (Vec3::ZERO, Vec3::ZERO));
    let mut actor_present: Vec<bool> = vec![false; scene.actors.capacity()];
    scene.actors.for_each_visible_with_index(|i, a| {
        actor_aabbs[i as usize] = a.world_aabb();
        actor_present[i as usize] = true;
    });

    // Per-ray "already considered" bitmap. We iterate the macro-grid
    // cells along each ray and an actor can appear in several cells; the
    // bitset turns those duplicates into O(1) skips.
    let mut seen_actor: Vec<u8> = vec![0; scene.actors.capacity()];
    // Stamp counter — bumped per ray. Avoids the cost of clearing
    // `seen_actor` to zero each ray.
    let mut ray_stamp: u8 = 0;

    let max_t = 1024.0;

    for py in 0..HEIGHT {
        let v = ((py as f32 + 0.5) / HEIGHT as f32) * 2.0 - 1.0;
        for px in 0..WIDTH {
            let u = ((px as f32 + 0.5) / WIDTH as f32) * 2.0 - 1.0;

            let dir = (basis.forward
                + basis.right * (u * half_w)
                + basis.up * (-v * half_h))
                .normalize();

            // World chunk first.
            let world_origin = camera.eye - scene.chunk_origin;
            let mut closest = scene.chunk.raycast(world_origin, dir, max_t);

            // Bump the per-ray stamp. On wrap (every 256 rays) reset
            // `seen_actor` so old stamps don't ghost as "already seen."
            ray_stamp = ray_stamp.wrapping_add(1);
            if ray_stamp == 0 {
                seen_actor.iter_mut().for_each(|s| *s = 0);
                ray_stamp = 1;
            }

            // Walk the macro-grid; for each cell the ray enters, fold
            // its actor candidates into the depth comparison.
            for (cx, cy, cz) in scene.macro_grid.ray_iter(camera.eye, dir, max_t) {
                let bound = closest.as_ref().map(|h| h.t).unwrap_or(max_t);
                for &actor_idx in scene.macro_grid.cell_actors(cx, cy, cz) {
                    let i = actor_idx as usize;
                    if seen_actor[i] == ray_stamp { continue; }
                    seen_actor[i] = ray_stamp;
                    if !actor_present[i] { continue; }
                    let (aabb_min, aabb_max) = actor_aabbs[i];
                    if !ray_aabb_hit(camera.eye, dir, aabb_min, aabb_max, bound) {
                        continue;
                    }
                    let actor = match scene.actors.get(ActorId(actor_idx)) {
                        Some(a) => a,
                        None => continue,
                    };
                    let (lo, ld) = actor.world_to_local_ray(camera.eye, dir);
                    if let Some(mut hit) = actor.chunk().raycast(lo, ld, bound) {
                        let nl = Vec3::new(
                            hit.normal.0 as f32,
                            hit.normal.1 as f32,
                            hit.normal.2 as f32,
                        );
                        let nw = actor.local_to_world_normal(nl);
                        hit.normal = (
                            nw.x.round() as i32,
                            nw.y.round() as i32,
                            nw.z.round() as i32,
                        );
                        if closest.as_ref().map(|c| hit.t < c.t).unwrap_or(true) {
                            closest = Some(hit);
                        }
                    }
                }
            }

            let color = match closest {
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

/// Cheap ray-AABB hit test: returns true if the ray enters the AABB before
/// `max_t`. Doesn't return the entry distance (the inner DDA computes that).
fn ray_aabb_hit(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3, max_t: f32) -> bool {
    let inv = (
        if dir.x != 0.0 { 1.0 / dir.x } else { f32::INFINITY },
        if dir.y != 0.0 { 1.0 / dir.y } else { f32::INFINITY },
        if dir.z != 0.0 { 1.0 / dir.z } else { f32::INFINITY },
    );
    let t1 = ((min.x - origin.x) * inv.0, (min.y - origin.y) * inv.1, (min.z - origin.z) * inv.2);
    let t2 = ((max.x - origin.x) * inv.0, (max.y - origin.y) * inv.1, (max.z - origin.z) * inv.2);
    let t_enter = t1.0.min(t2.0).max(t1.1.min(t2.1)).max(t1.2.min(t2.2));
    let t_exit  = t1.0.max(t2.0).min(t1.1.max(t2.1)).min(t1.2.max(t2.2));
    t_enter <= t_exit && t_exit >= 0.0 && t_enter <= max_t
}

/// Translate a hit + lighting into a final palette color.
fn shade(hit: RayHit, scene: &Scene, sun_dir: Vec3) -> voxlconsl_types::PaletteColor {
    let m = scene.materials[hit.material as usize];
    let n = Vec3::new(hit.normal.0 as f32, hit.normal.1 as f32, hit.normal.2 as f32);
    let ndotl = n.dot(sun_dir).max(0.0);
    let brightness = 0.35 + 0.65 * ndotl;

    let shade_idx = if m.emission > 0 {
        3
    } else {
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
