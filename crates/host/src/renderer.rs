//! Renderer — see SPEC.md §3.
//!
//! v0.1.0: pinhole-camera CPU ray marcher walking the **sparse 512³
//! voxel world** (§13.6). Per ray we use the macro-grid to step the
//! 32³ chunk grid front-to-back; for each populated chunk we DDA the
//! local SVO, and for each macro-cell we test the actors registered
//! there. World voxels and actors participate in the same depth
//! comparison, closest hit wins.
//!
//! TODO progression toward full §3:
//!   - Real lighting model with shadows (§3.3)
//!   - Sky gradient + sun disc (§3.4)
//!   - Camera projections beyond perspective (§3.2)

use voxlconsl_svo::{ray::RayHit, ChunkKey};
use voxlconsl_types::{ActorId, ActorRenderMode, Material, MaterialFlags, Vec3};

use crate::actors::ActorTable;
use crate::ca::{CaState, LIQUID_LEVEL_MAX};
use crate::macro_grid::MacroGrid;
use crate::palette::{SYSTEM_PALETTE, lit_color_index};
use crate::world::{ChunkState, WORLD_SIDE};

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
    /// Sparse chunk slot table indexed by `ChunkKey.0 as usize`. None
    /// = uniform air. The macro-grid traversal indexes this directly.
    pub chunks: &'a [Option<Box<ChunkState>>],
    pub actors: &'a ActorTable,
    pub macro_grid: &'a MacroGrid,
    pub materials: &'a [Material; 256],
    /// CA active-set state; sourced by the renderer for liquid sub-cell
    /// surface heights (§10.3 "Renderer integration"). Liquid voxels
    /// outside the active set default to full level.
    pub ca: &'a CaState,
    pub sun_dir: Vec3,
    /// Sky gradient — palette indices for the zenith colour (looking
    /// straight up) and the horizon colour. Rays that miss every voxel
    /// sample this gradient based on their vertical direction. Setting
    /// both to the same index gives a flat-colour sky.
    pub sky_top: u8,
    pub sky_horizon: u8,
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
    let sky_top_rgb = SYSTEM_PALETTE[scene.sky_top.min(63) as usize];
    let sky_horizon_rgb = SYSTEM_PALETTE[scene.sky_horizon.min(63) as usize];

    // Pre-compute world AABBs for every visible actor so the per-ray
    // pass doesn't redo the corner transforms. The actor index is the
    // slot index, matching what the macro-grid stores.
    let mut actor_aabbs: Vec<(Vec3, Vec3)> = Vec::with_capacity(scene.actors.capacity());
    actor_aabbs.resize(scene.actors.capacity(), (Vec3::ZERO, Vec3::ZERO));
    let mut actor_present: Vec<bool> = vec![false; scene.actors.capacity()];
    scene.actors.for_each_visible_with_index(|i, a| {
        // Only Worldspace actors participate in the world ray-march.
        // Billboard / Screen actors have their own composite pass.
        if a.render_mode != ActorRenderMode::Worldspace { return; }
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

    // Generous cap; macro_grid::ray_iter clips to WORLD_SIDE internally.
    let max_t = 512.0;

    for py in 0..HEIGHT {
        let v = ((py as f32 + 0.5) / HEIGHT as f32) * 2.0 - 1.0;
        for px in 0..WIDTH {
            let u = ((px as f32 + 0.5) / WIDTH as f32) * 2.0 - 1.0;

            let dir = (basis.forward
                + basis.right * (u * half_w)
                + basis.up * (-v * half_h))
                .normalize();

            // Bump the per-ray stamp. On wrap (every 256 rays) reset
            // `seen_actor` so old stamps don't ghost as "already seen."
            ray_stamp = ray_stamp.wrapping_add(1);
            if ray_stamp == 0 {
                seen_actor.iter_mut().for_each(|s| *s = 0);
                ray_stamp = 1;
            }

            // Walk the macro-grid front-to-back; for each cell, test
            // the world chunk that lives there (if any), then the
            // actors binned into that cell. The cell-side traversal
            // is the chunk-side traversal — they share a coord system.
            let mut closest: Option<RayHit> = None;
            for (cx, cy, cz) in scene.macro_grid.ray_iter(camera.eye, dir, max_t) {
                let bound = closest.as_ref().map(|h| h.t).unwrap_or(max_t);

                // World chunk in this cell.
                let key = ChunkKey::new(cx as u8, cy as u8, cz as u8);
                if let Some(cs) = scene.chunks[key.0 as usize].as_deref() {
                    let chunk_origin = Vec3::new(
                        cx as f32 * 32.0,
                        cy as f32 * 32.0,
                        cz as f32 * 32.0,
                    );
                    let local_origin = camera.eye - chunk_origin;
                    if let Some(mut hit) = cs.chunk.raycast(local_origin, dir, bound) {
                        // §10.3 "Renderer integration": if the hit is
                        // on a partial-fill liquid voxel and the ray
                        // came in through the top face, drop t to the
                        // sub-cell surface. Side and bottom entries
                        // render as full this pass — handling those
                        // requires SVO-level continuation, which we
                        // defer.
                        let m = scene.materials[hit.material as usize];
                        if m.flags.contains(MaterialFlags::LIQUID)
                            && hit.normal == (0, 1, 0)
                            && dir.y < 0.0
                        {
                            let wx = hit.voxel.0 + cx * 32;
                            let wy = hit.voxel.1 + cy * 32;
                            let wz = hit.voxel.2 + cz * 32;
                            // Only show a sub-cell surface for the
                            // top voxel of a column — i.e., when the
                            // cell directly above is NOT the same
                            // liquid. A pressured cell (water above)
                            // fluctuates levels as mass cycles through
                            // it; rendering that as varying height
                            // produces the flicker you'd otherwise
                            // see at a stream's impact point.
                            let pressured_above =
                                read_world_material(scene.chunks, wx, wy + 1, wz) == hit.material;
                            if !pressured_above {
                                let level = scene.ca.liquid_level(wx, wy, wz);
                                if level < LIQUID_LEVEL_MAX {
                                    let surface_y =
                                        wy as f32 + level as f32 / LIQUID_LEVEL_MAX as f32;
                                    let t_surface = (surface_y - camera.eye.y) / dir.y;
                                    if t_surface > hit.t {
                                        hit.t = t_surface;
                                    }
                                }
                            }
                        }
                        if closest.as_ref().map(|c| hit.t < c.t).unwrap_or(true) {
                            closest = Some(hit);
                        }
                    }
                }

                // Actors binned into this cell.
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
                None => sample_sky(dir.y, sky_top_rgb, sky_horizon_rgb),
            };

            let i = ((py * WIDTH + px) * 4) as usize;
            framebuffer[i]     = color.r;
            framebuffer[i + 1] = color.g;
            framebuffer[i + 2] = color.b;
            framebuffer[i + 3] = 255;
        }
    }

    composite_billboard_actors(scene, camera, &basis, half_w, half_h, framebuffer);
    composite_screen_actors(scene, framebuffer);
}

/// Composite every visible `Billboard`-mode actor onto the framebuffer
/// after the world ray-march. `position` is interpreted as a world
/// anchor; we project it through the camera basis to a framebuffer
/// pixel and blit the actor's voxel grid **centered** on that point
/// (1 voxel = 1 pixel). Local `+X` → screen-right; local `+Y` →
/// screen-up, so an actor painted with `Axis::XY` reads right-side
/// up. Air voxels are transparent. No depth test in Phase 1 —
/// billboards always sit on top of the world ray-march.
///
/// Anchors behind the camera (forward distance ≤ 0) are skipped. A
/// future depth-buffer pass can clip billboards against world hits;
/// for HUD-style use (cursors, labels) "always on top" is the right
/// default.
fn composite_billboard_actors(
    scene: &Scene,
    camera: &Camera,
    basis: &Basis,
    half_w: f32,
    half_h: f32,
    framebuffer: &mut [u8],
) {
    let fb_w = WIDTH as i32;
    let fb_h = HEIGHT as i32;
    let cx = WIDTH as f32 * 0.5;
    let cy = HEIGHT as f32 * 0.5;

    scene.actors.for_each_visible_with_index(|i, actor| {
        if actor.render_mode != ActorRenderMode::Billboard { return; }
        let _ = i;
        // Project anchor through camera basis.
        let to = actor.position - camera.eye;
        let z_along_fwd = to.x * basis.forward.x
                        + to.y * basis.forward.y
                        + to.z * basis.forward.z;
        if z_along_fwd <= 0.001 { return; }    // behind camera or on the eye plane
        let r_dot = to.x * basis.right.x + to.y * basis.right.y + to.z * basis.right.z;
        let u_dot = to.x * basis.up.x    + to.y * basis.up.y    + to.z * basis.up.z;
        // Normalized device coords in [-1, 1] within the camera frustum.
        let ndc_x = r_dot / (z_along_fwd * half_w);
        let ndc_y = u_dot / (z_along_fwd * half_h);

        let size = actor.volume_size();
        let sw = size.x as i32;
        let sh = size.y as i32;

        // Center of the blit rect in framebuffer pixels. Y is flipped
        // since pixel Y grows down but NDC Y grows up.
        let center_x = (cx + ndc_x * cx) as i32;
        let center_y = (cy - ndc_y * cy) as i32;
        let sx = center_x - sw / 2;
        let sy = center_y - sh / 2;

        // Clip against framebuffer.
        let px_min = sx.max(0);
        let py_min = sy.max(0);
        let px_max = (sx + sw).min(fb_w);
        let py_max = (sy + sh).min(fb_h);
        if px_min >= px_max || py_min >= py_max { return; }

        for py in py_min..py_max {
            for px in px_min..px_max {
                let lx = (px - sx) as u8;
                let ly = (size.y as i32 - 1 - (py - sy)) as u8;
                let mut paint: Option<u8> = None;
                for lz in 0..size.z {
                    let m = actor.get_voxel(lx, ly, lz);
                    if m != 0 { paint = Some(m); break; }
                }
                if let Some(mat) = paint {
                    let color_index = scene.materials[mat as usize].color;
                    let rgb = SYSTEM_PALETTE[color_index.min(63) as usize];
                    let fbi = ((py * fb_w + px) * 4) as usize;
                    framebuffer[fbi]     = rgb.r;
                    framebuffer[fbi + 1] = rgb.g;
                    framebuffer[fbi + 2] = rgb.b;
                    framebuffer[fbi + 3] = 255;
                }
            }
        }
    });
}


/// Composite every visible `Screen`-mode actor onto the framebuffer
/// after the world ray-march. Each Screen actor is a 2D blit:
/// `position.(x, y)` are framebuffer pixel coords of the rect's
/// upper-left corner, `position.z` is the layer key (lower z paints
/// first → higher z overwrites). The actor's local `+X` maps to
/// screen-right; local `+Y` maps to screen-up, so the volume's top
/// row paints at the top of the rect (consistent with `Axis::XY`
/// glyph painting where row 0 lands at the highest local Y).
///
/// Air voxels (material 0) are transparent. Along the volume's local
/// Z axis we walk front-to-back and keep the first non-air voxel —
/// gives the cart a simple way to draw layered icons by stacking
/// slices.
fn composite_screen_actors(scene: &Scene, framebuffer: &mut [u8]) {
    // Collect Screen actors with their layer key for sort.
    let cap = scene.actors.capacity();
    let mut entries: Vec<(i32, u32)> = Vec::with_capacity(cap.min(64));
    scene.actors.for_each_visible_with_index(|i, a| {
        if a.render_mode == ActorRenderMode::Screen {
            entries.push((a.position.z as i32, i));
        }
    });
    if entries.is_empty() { return; }
    entries.sort_by_key(|&(z, _)| z);

    let fb_w = WIDTH as i32;
    let fb_h = HEIGHT as i32;

    for &(_z, idx) in &entries {
        let actor = match scene.actors.get(ActorId(idx)) {
            Some(a) => a,
            None => continue,
        };
        let size = actor.volume_size();
        let sx = actor.position.x as i32;
        let sy = actor.position.y as i32;
        let sw = size.x as i32;
        let sh = size.y as i32;

        // Clip the actor's screen rect against the framebuffer.
        let px_min = sx.max(0);
        let py_min = sy.max(0);
        let px_max = (sx + sw).min(fb_w);
        let py_max = (sy + sh).min(fb_h);
        if px_min >= px_max || py_min >= py_max { continue; }

        for py in py_min..py_max {
            for px in px_min..px_max {
                // Map framebuffer pixel back into actor-local (x, y).
                // Local +Y maps to screen-up, so flip vertically: the
                // top row of the rect (py_min) corresponds to the
                // largest local Y (`size.y - 1`).
                let lx = (px - sx) as u8;
                let ly = (size.y as i32 - 1 - (py - sy)) as u8;
                // Front-most non-air voxel along Z wins.
                let mut paint: Option<u8> = None;
                for lz in 0..size.z {
                    let m = actor.get_voxel(lx, ly, lz);
                    if m != 0 { paint = Some(m); break; }
                }
                if let Some(mat) = paint {
                    let color_index = scene.materials[mat as usize].color;
                    let rgb = SYSTEM_PALETTE[color_index.min(63) as usize];
                    let fbi = ((py * fb_w + px) * 4) as usize;
                    framebuffer[fbi]     = rgb.r;
                    framebuffer[fbi + 1] = rgb.g;
                    framebuffer[fbi + 2] = rgb.b;
                    framebuffer[fbi + 3] = 255;
                }
            }
        }
    }
}

/// Look up a world voxel's material directly from the chunk dense
/// buffers. Used by the liquid surface-clip path to peek at the cell
/// above a hit. Out-of-bounds and unallocated chunks return 0 (air).
fn read_world_material(
    chunks: &[Option<Box<ChunkState>>],
    x: u32,
    y: u32,
    z: u32,
) -> u8 {
    if x >= WORLD_SIDE || y >= WORLD_SIDE || z >= WORLD_SIDE { return 0; }
    let cx = (x >> 5) as u8;
    let cy = (y >> 5) as u8;
    let cz = (z >> 5) as u8;
    let key = ChunkKey::new(cx, cy, cz);
    let cs = match chunks.get(key.0 as usize).and_then(|c| c.as_deref()) {
        Some(c) => c,
        None => return 0,
    };
    let lx = (x & 31) as usize;
    let ly = (y & 31) as usize;
    let lz = (z & 31) as usize;
    cs.dense[(lz * 32 + ly) * 32 + lx]
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

/// Sample the sky gradient for a ray's vertical direction.
///
/// `dir_y == 1.0` (zenith) returns `top`; `dir_y == 0.0` (horizon)
/// returns `horizon`; rays below the horizon clamp to `horizon` since
/// the host doesn't render a separate ground colour. The cubic
/// smoothstep keeps the horizon band wide and the top band narrow,
/// which reads more like a real sky than a straight linear lerp —
/// most of the framebuffer near the horizon stays close to the
/// horizon colour.
fn sample_sky(
    dir_y: f32,
    top: voxlconsl_types::PaletteColor,
    horizon: voxlconsl_types::PaletteColor,
) -> voxlconsl_types::PaletteColor {
    let t = dir_y.max(0.0).min(1.0);
    let t = t * t * (3.0 - 2.0 * t); // smoothstep(0, 1, t)
    voxlconsl_types::PaletteColor {
        r: lerp_u8(horizon.r, top.r, t),
        g: lerp_u8(horizon.g, top.g, t),
        b: lerp_u8(horizon.b, top.b, t),
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let af = a as f32;
    let bf = b as f32;
    (af + (bf - af) * t).round().clamp(0.0, 255.0) as u8
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
