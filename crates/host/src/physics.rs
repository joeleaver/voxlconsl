//! Physics — Layer 1 query primitives (§10.1).
//!
//! Pure functions over `WorldState`: ray and AABB tests against the
//! active scene's chunks and the cart-global actor table. Reuses the
//! existing chunk SVO raycast (§13.4) and the macro-grid actor binning
//! (§11.6) the renderer already drives — single implementation, two
//! consumers.
//!
//! Layer 2 (rigid bodies) and Layer 3 (cellular automata) live in their
//! own modules; this file is queries only.

use voxlconsl_svo::ChunkKey;
use voxlconsl_types::{
    ActorId, ActorMask, Hit, IVec3, SweepHit, UVec3, Vec3,
};

use crate::actors::{Actor, ActorTable};
use crate::world::{WorldState, WORLD_SIDE};

const CHUNK_SIDE: f32 = 32.0;

/// Closest-hit ray against the active scene's chunks **and** visible
/// actors. Direction need not be normalized; `max_dist` is in world units.
pub fn raycast(
    world: &WorldState,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
) -> Option<Hit> {
    let dir = dir.normalize();
    let chunks = world.chunks_slice();
    let macro_grid = &world.macro_grid;
    let actors = &world.actors;

    let mut closest: Option<Hit> = None;
    for (cx, cy, cz) in macro_grid.ray_iter(origin, dir, max_dist) {
        let bound = closest.map(|h| h.t).unwrap_or(max_dist);
        let key = ChunkKey::new(cx as u8, cy as u8, cz as u8);
        if let Some(cs) = chunks.get(key.0 as usize).and_then(|c| c.as_deref()) {
            let chunk_origin = Vec3::new(cx as f32 * CHUNK_SIDE, cy as f32 * CHUNK_SIDE, cz as f32 * CHUNK_SIDE);
            let local_origin = origin - chunk_origin;
            if let Some(hit) = cs.chunk.raycast(local_origin, dir, bound) {
                let world_voxel = UVec3::new(
                    cx * 32 + hit.voxel.0,
                    cy * 32 + hit.voxel.1,
                    cz * 32 + hit.voxel.2,
                );
                if closest.map(|c| hit.t < c.t).unwrap_or(true) {
                    closest = Some(Hit {
                        pos: world_voxel,
                        material: hit.material,
                        _pad: [0; 3],
                        normal: IVec3::new(hit.normal.0, hit.normal.1, hit.normal.2),
                        t: hit.t,
                        actor: Hit::NO_ACTOR,
                    });
                }
            }
        }
        let bound = closest.map(|h| h.t).unwrap_or(max_dist);
        for &actor_idx in macro_grid.cell_actors(cx, cy, cz) {
            if let Some(actor) = actors.get(ActorId(actor_idx)) {
                if !actor.visible { continue; }
                if let Some(mut hit) = raycast_actor(actor, origin, dir, bound) {
                    hit.actor = actor_idx;
                    if closest.map(|c| hit.t < c.t).unwrap_or(true) {
                        closest = Some(hit);
                    }
                }
            }
        }
    }
    closest
}

/// Same as [`raycast`] but ignores actors. Useful when carts want to
/// trace against the static world only (e.g., line-of-sight that
/// shouldn't be blocked by NPCs).
pub fn raycast_world_only(
    world: &WorldState,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
) -> Option<Hit> {
    let dir = dir.normalize();
    let chunks = world.chunks_slice();
    let macro_grid = &world.macro_grid;

    let mut closest: Option<Hit> = None;
    for (cx, cy, cz) in macro_grid.ray_iter(origin, dir, max_dist) {
        let bound = closest.map(|h| h.t).unwrap_or(max_dist);
        let key = ChunkKey::new(cx as u8, cy as u8, cz as u8);
        if let Some(cs) = chunks.get(key.0 as usize).and_then(|c| c.as_deref()) {
            let chunk_origin = Vec3::new(cx as f32 * CHUNK_SIDE, cy as f32 * CHUNK_SIDE, cz as f32 * CHUNK_SIDE);
            let local_origin = origin - chunk_origin;
            if let Some(hit) = cs.chunk.raycast(local_origin, dir, bound) {
                let world_voxel = UVec3::new(
                    cx * 32 + hit.voxel.0,
                    cy * 32 + hit.voxel.1,
                    cz * 32 + hit.voxel.2,
                );
                if closest.map(|c| hit.t < c.t).unwrap_or(true) {
                    closest = Some(Hit {
                        pos: world_voxel,
                        material: hit.material,
                        _pad: [0; 3],
                        normal: IVec3::new(hit.normal.0, hit.normal.1, hit.normal.2),
                        t: hit.t,
                        actor: Hit::NO_ACTOR,
                    });
                }
            }
        }
    }
    closest
}

fn raycast_actor(actor: &Actor, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Hit> {
    let (lo, ld) = actor.world_to_local_ray(origin, dir);
    let raw = actor.chunk().raycast(lo, ld, max_dist)?;
    let nl = Vec3::new(raw.normal.0 as f32, raw.normal.1 as f32, raw.normal.2 as f32);
    let nw = actor.local_to_world_normal(nl);
    Some(Hit {
        pos: UVec3::new(raw.voxel.0, raw.voxel.1, raw.voxel.2),
        material: raw.material,
        _pad: [0; 3],
        normal: IVec3::new(
            nw.x.round() as i32,
            nw.y.round() as i32,
            nw.z.round() as i32,
        ),
        t: raw.t,
        actor: Hit::NO_ACTOR,
    })
}

/// Read the material at an integer voxel coordinate in the active scene.
/// Out-of-bounds returns 0 (air).
pub fn material_at(world: &WorldState, x: u32, y: u32, z: u32) -> u8 {
    if x >= WORLD_SIDE || y >= WORLD_SIDE || z >= WORLD_SIDE {
        return 0;
    }
    let cx = (x / 32) as u8;
    let cy = (y / 32) as u8;
    let cz = (z / 32) as u8;
    let lx = (x % 32) as usize;
    let ly = (y % 32) as usize;
    let lz = (z % 32) as usize;
    let key = ChunkKey::new(cx, cy, cz);
    let chunks = world.chunks_slice();
    let cs = match chunks.get(key.0 as usize).and_then(|c| c.as_deref()) {
        Some(c) => c,
        None => return 0,
    };
    let i = ((lz * 32) + ly) * 32 + lx;
    cs.dense.get(i).copied().unwrap_or(0)
}

/// True if any non-air voxel of the active scene's world overlaps the
/// AABB. Conservative: tests every voxel coordinate inside the rounded
/// inclusive bounds.
pub fn aabb_overlap_world(world: &WorldState, min: Vec3, max: Vec3) -> bool {
    let (lo, hi) = clamped_voxel_range(min, max);
    if lo.is_none() { return false; }
    let (lo, hi) = (lo.unwrap(), hi.unwrap());
    for z in lo.z..=hi.z {
        for y in lo.y..=hi.y {
            for x in lo.x..=hi.x {
                if material_at(world, x, y, z) != 0 {
                    return true;
                }
            }
        }
    }
    false
}

/// Bitmask of actor ids whose world AABB overlaps the query AABB.
/// Actors with `id >= 64` cannot be represented in [`ActorMask`] and
/// are silently dropped from the result.
pub fn aabb_overlap_actors(actors: &ActorTable, min: Vec3, max: Vec3) -> ActorMask {
    let mut mask: u64 = 0;
    actors.for_each_visible_with_index(|i, a| {
        if i >= 64 { return; }
        let (amin, amax) = a.world_aabb();
        if aabb_intersects(min, max, amin, amax) {
            mask |= 1u64 << i;
        }
    });
    ActorMask(mask)
}

/// Sweep an AABB through the world along `motion`. Returns the first
/// blocking hit (against the world or a visible actor) or `None` if the
/// box reaches the end of `motion` unimpeded.
///
/// v1 implementation: collapses the box to a point at its center and
/// calls [`raycast`] for `|motion|` units, then clips by the motion's
/// length so we don't return hits past the sweep end. Carts that need
/// shape-aware sweeps can compose `aabb_overlap_world` along their
/// motion until a richer sweep lands.
pub fn sweep_aabb(
    world: &WorldState,
    min: Vec3,
    max: Vec3,
    motion: Vec3,
) -> Option<SweepHit> {
    let center = Vec3::new(
        (min.x + max.x) * 0.5,
        (min.y + max.y) * 0.5,
        (min.z + max.z) * 0.5,
    );
    let len = motion.length();
    if len <= 0.0 { return None; }
    let dir = motion * (1.0 / len);
    let hit = raycast(world, center, dir, len)?;
    Some(SweepHit {
        t: hit.t / len,
        normal: hit.normal,
        blocked_by_actor: hit.actor,
    })
}

/// Encode a [`Hit`] to its 36-byte wire form. Used by sandbox.rs when
/// writing the result back to cart memory.
pub fn encode_hit(hit: &Hit) -> [u8; 36] {
    let mut out = [0u8; 36];
    out[0..4].copy_from_slice(&hit.pos.x.to_le_bytes());
    out[4..8].copy_from_slice(&hit.pos.y.to_le_bytes());
    out[8..12].copy_from_slice(&hit.pos.z.to_le_bytes());
    out[12] = hit.material;
    // bytes 13..16 are pad, already zero
    out[16..20].copy_from_slice(&hit.normal.x.to_le_bytes());
    out[20..24].copy_from_slice(&hit.normal.y.to_le_bytes());
    out[24..28].copy_from_slice(&hit.normal.z.to_le_bytes());
    out[28..32].copy_from_slice(&hit.t.to_le_bytes());
    out[32..36].copy_from_slice(&hit.actor.to_le_bytes());
    out
}

pub fn encode_sweep_hit(hit: &SweepHit) -> [u8; 20] {
    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&hit.t.to_le_bytes());
    out[4..8].copy_from_slice(&hit.normal.x.to_le_bytes());
    out[8..12].copy_from_slice(&hit.normal.y.to_le_bytes());
    out[12..16].copy_from_slice(&hit.normal.z.to_le_bytes());
    out[16..20].copy_from_slice(&hit.blocked_by_actor.to_le_bytes());
    out
}

// ============================================================================
// Helpers
// ============================================================================

fn aabb_intersects(a_min: Vec3, a_max: Vec3, b_min: Vec3, b_max: Vec3) -> bool {
    a_min.x <= b_max.x && a_max.x >= b_min.x
        && a_min.y <= b_max.y && a_max.y >= b_min.y
        && a_min.z <= b_max.z && a_max.z >= b_min.z
}

fn clamped_voxel_range(min: Vec3, max: Vec3) -> (Option<UVec3>, Option<UVec3>) {
    let world = WORLD_SIDE as f32;
    if max.x < 0.0 || max.y < 0.0 || max.z < 0.0
        || min.x >= world || min.y >= world || min.z >= world
    {
        return (None, None);
    }
    let lo = UVec3::new(
        min.x.max(0.0).floor() as u32,
        min.y.max(0.0).floor() as u32,
        min.z.max(0.0).floor() as u32,
    );
    let hi = UVec3::new(
        (max.x.min(world - 1.0)).floor() as u32,
        (max.y.min(world - 1.0)).floor() as u32,
        (max.z.min(world - 1.0)).floor() as u32,
    );
    (Some(lo), Some(hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_world() -> WorldState {
        let mut w = WorldState::new();
        w.flush();
        w.macro_grid.rebuild(&w.actors);
        w
    }

    #[test]
    fn raycast_misses_empty_world() {
        let world = empty_world();
        assert!(raycast(&world, Vec3::new(0.0, 0.0, 0.0), Vec3::X, 100.0).is_none());
    }

    #[test]
    fn raycast_hits_single_voxel() {
        let mut world = WorldState::new();
        world.set_voxel(10, 10, 10, 5);
        world.flush();
        world.macro_grid.rebuild(&world.actors);
        let hit = raycast(
            &world,
            Vec3::new(0.0, 10.5, 10.5),
            Vec3::X,
            100.0,
        ).expect("hit");
        assert_eq!(hit.material, 5);
        assert_eq!(hit.pos, UVec3::new(10, 10, 10));
        assert_eq!(hit.normal, IVec3::new(-1, 0, 0));
        assert_eq!(hit.actor, Hit::NO_ACTOR);
    }

    #[test]
    fn material_at_returns_air_for_oob() {
        let world = empty_world();
        assert_eq!(material_at(&world, 9999, 9999, 9999), 0);
    }

    #[test]
    fn aabb_overlap_world_detects_voxel() {
        let mut world = WorldState::new();
        world.set_voxel(20, 20, 20, 7);
        world.flush();
        assert!(aabb_overlap_world(&world,
            Vec3::new(19.0, 19.0, 19.0), Vec3::new(21.0, 21.0, 21.0)));
        assert!(!aabb_overlap_world(&world,
            Vec3::new(0.0, 0.0, 0.0), Vec3::new(5.0, 5.0, 5.0)));
    }

    #[test]
    fn raycast_world_only_axis_aligned_through_multiple_chunks() {
        // Regression for a NaN bug in the SVO ray_aabb slab math: a
        // straight-down ray starting on the chunk's x/z slab boundary
        // (origin.x exactly on a chunk edge, dir.x == 0) used to
        // produce 0 * ∞ = NaN and reject the chunk. Cast from above
        // through 4 chunks of empty space onto a single voxel of
        // terrain.
        let mut world = WorldState::new();
        world.set_voxel(260, 14, 260, 3);
        world.flush();
        world.macro_grid.rebuild(&world.actors);
        let hit = raycast_world_only(
            &world,
            Vec3::new(260.0, 100.0, 260.0),
            Vec3::new(0.0, -1.0, 0.0),
            200.0,
        ).expect("expected to hit terrain voxel from above");
        assert_eq!(hit.material, 3);
        assert_eq!(hit.pos, UVec3::new(260, 14, 260));
        assert_eq!(hit.normal, IVec3::new(0, 1, 0));
    }
}
