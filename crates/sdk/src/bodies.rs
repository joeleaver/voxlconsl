//! Layer 2 rigid bodies — SPEC.md §10.2.
//!
//! Each cart may spawn up to 64 host-integrated bodies. Bodies are
//! AABB or sphere shaped, attached to an optional actor (so the
//! renderer follows the simulated transform), and integrated each
//! frame by the host using gravity + axis-separated voxel-grid
//! collision + pairwise body-vs-body resolution.
//!
//! Spawn a body, push it around with impulses or velocity, and drain
//! collision events each frame to drive cart-side reactions (e.g.,
//! voxel destruction).

use voxlconsl_types::{ActorId, BodyId, BodyKind, BodyState, CollisionEvent, Shape, Vec3};

use crate::host;

const NO_ACTOR: u32 = u32::MAX;

/// Spawn a body attached to `actor`. Returns `None` when the per-cart
/// body cap (64) is reached. `mass` ≤ 0 is treated as 1.0 by the host
/// (Static / Kinematic ignore mass anyway).
///
/// The body's initial position is set from the attached actor's
/// position + the shape's half-extents, so the actor's local origin
/// continues to line up with the volume's `(0, 0, 0)` corner. For
/// unattached bodies the host picks `(0, 0, 0)` — call
/// [`body_set_position`] right after spawn.
pub fn body_spawn(actor: Option<ActorId>, kind: BodyKind, shape: Shape, mass: f32) -> Option<BodyId> {
    let actor_id = actor.map(|a| a.0).unwrap_or(NO_ACTOR);
    let [sx, sy, sz] = shape.to_floats();
    let id = unsafe {
        host::body_spawn(actor_id, kind as u32, shape.tag() as u32, sx, sy, sz, mass)
    };
    if id == u32::MAX { None } else { Some(BodyId(id)) }
}

pub fn body_despawn(id: BodyId) { unsafe { host::body_despawn(id.0) } }

pub fn body_set_kind(id: BodyId, kind: BodyKind) {
    unsafe { host::body_set_kind(id.0, kind as u32) }
}

pub fn body_set_position(id: BodyId, pos: Vec3) {
    unsafe { host::body_set_position(id.0, pos.x, pos.y, pos.z) }
}

pub fn body_set_velocity(id: BodyId, v: Vec3) {
    unsafe { host::body_set_velocity(id.0, v.x, v.y, v.z) }
}

/// Apply an instantaneous impulse `j` (units: mass·velocity). Effective
/// only on Dynamic bodies; Static / Kinematic are no-ops.
pub fn body_apply_impulse(id: BodyId, j: Vec3) {
    unsafe { host::body_apply_impulse(id.0, j.x, j.y, j.z) }
}

/// Assign a collision layer (0–7) and mask of layers this body will
/// collide with. See SPEC.md §10.2 for the 8×8 matrix semantics.
pub fn body_set_layer(id: BodyId, layer: u8, mask: u8) {
    unsafe { host::body_set_layer(id.0, layer as u32, mask as u32) }
}

/// Toggle sensor mode. Sensors emit collision events but don't resolve
/// contact — useful for triggers and pickups.
pub fn body_set_sensor(id: BodyId, sensor: bool) {
    unsafe { host::body_set_sensor(id.0, sensor as u32) }
}

/// Set per-body material constants — `restitution` is bounciness
/// (0 = inelastic, 1 = perfectly elastic), `friction` is the Coulomb
/// coefficient applied to tangential motion on contact. Both clamped to
/// [0, 1] by the host.
pub fn body_set_material(id: BodyId, restitution: f32, friction: f32) {
    unsafe { host::body_set_material(id.0, restitution, friction) }
}

/// Snapshot a body's current state. Returns `None` if the id was
/// despawned.
pub fn body_get(id: BodyId) -> Option<BodyState> {
    let mut out = empty_state();
    let ok = unsafe { host::body_get(id.0, &mut out as *mut BodyState) };
    if ok != 0 { Some(out) } else { None }
}

/// Set the world gravity vector applied to every Dynamic body each
/// substep. Defaults to `(0, 0, 0)` at boot — a cart must opt in.
pub fn world_set_gravity(g: Vec3) {
    unsafe { host::world_set_gravity(g.x, g.y, g.z) }
}

/// Drain up to `buf.len()` queued collision events from this frame.
/// Returns the slice of events actually filled. Events not drained
/// stay queued for the next call; the host caps the queue at 256 and
/// drops the oldest when full.
pub fn drain_collision_events<'a>(buf: &'a mut [CollisionEvent]) -> &'a [CollisionEvent] {
    let n = unsafe {
        host::drain_collision_events(buf.as_mut_ptr(), buf.len() as u32)
    } as usize;
    &buf[..n.min(buf.len())]
}

fn empty_state() -> BodyState {
    BodyState {
        kind: 0,
        shape_tag: 0,
        layer: 0,
        mask: 0,
        sensor: 0,
        _pad: [0; 3],
        shape: [0.0; 3],
        position: Vec3::ZERO,
        velocity: Vec3::ZERO,
        mass: 0.0,
        restitution: 0.0,
        friction: 0.0,
        actor: BodyState::NO_ACTOR,
    }
}
