//! Physics queries — see SPEC.md §10.1.
//!
//! Pure read-only intersection primitives against the active scene's
//! voxel grid and the cart's actor table. The host imports auto-flush
//! pending mutations so a `set_voxel` immediately followed by a
//! `raycast` sees the post-write state.

use voxlconsl_types::{ActorMask, Hit, SweepHit, Vec3};

use crate::host;

/// Closest-hit ray against world voxels and visible actors.
///
/// `dir` is normalized internally; `max_dist` is in world units.
/// Returns `Some(Hit)` if anything was struck within `max_dist`,
/// `None` otherwise.
pub fn raycast(origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Hit> {
    let mut out = empty_hit();
    let hit = unsafe {
        host::raycast(
            origin.x, origin.y, origin.z,
            dir.x, dir.y, dir.z,
            max_dist,
            &mut out as *mut Hit,
        )
    };
    if hit != 0 { Some(out) } else { None }
}

/// Same as [`raycast`] but ignores actors — useful for line-of-sight
/// checks against static geometry.
pub fn raycast_world_only(origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Hit> {
    let mut out = empty_hit();
    let hit = unsafe {
        host::raycast_world_only(
            origin.x, origin.y, origin.z,
            dir.x, dir.y, dir.z,
            max_dist,
            &mut out as *mut Hit,
        )
    };
    if hit != 0 { Some(out) } else { None }
}

/// True if any non-air voxel in the active scene's world overlaps the
/// AABB.
pub fn aabb_overlap_world(min: Vec3, max: Vec3) -> bool {
    let r = unsafe {
        host::aabb_overlap_world(min.x, min.y, min.z, max.x, max.y, max.z)
    };
    r != 0
}

/// Bitmask of visible actor ids whose world AABB overlaps the query
/// AABB. Actors with id ≥ 64 cannot be represented and are dropped from
/// the result.
pub fn aabb_overlap_actors(min: Vec3, max: Vec3) -> ActorMask {
    let r = unsafe {
        host::aabb_overlap_actors(min.x, min.y, min.z, max.x, max.y, max.z)
    };
    ActorMask(r)
}

/// Sweep an AABB through the world along `motion`. Returns the first
/// blocking hit (parameterized along `motion`, so `t = 0.5` means the
/// box reached the hit point at half the requested motion length) or
/// `None` if it reaches the end unimpeded.
pub fn sweep_aabb(min: Vec3, max: Vec3, motion: Vec3) -> Option<SweepHit> {
    let mut out = empty_sweep_hit();
    let hit = unsafe {
        host::sweep_aabb(
            min.x, min.y, min.z,
            max.x, max.y, max.z,
            motion.x, motion.y, motion.z,
            &mut out as *mut SweepHit,
        )
    };
    if hit != 0 { Some(out) } else { None }
}

/// Read the material at an integer voxel coordinate. Out-of-bounds is 0.
pub fn material_at(x: u32, y: u32, z: u32) -> u8 {
    let r = unsafe { host::material_at(x, y, z) };
    r as u8
}

fn empty_hit() -> Hit {
    Hit {
        pos: voxlconsl_types::UVec3::ZERO,
        material: 0,
        _pad: [0; 3],
        normal: voxlconsl_types::IVec3::ZERO,
        t: 0.0,
        actor: Hit::NO_ACTOR,
    }
}

fn empty_sweep_hit() -> SweepHit {
    SweepHit {
        t: 0.0,
        normal: voxlconsl_types::IVec3::ZERO,
        blocked_by_actor: SweepHit::NO_ACTOR,
    }
}
