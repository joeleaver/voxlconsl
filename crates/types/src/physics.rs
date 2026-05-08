//! Physics types — see SPEC.md §10.

use crate::math::{IVec3, UVec3, Vec3};
use crate::actor::ActorId;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct BodyId(pub u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BodyKind {
    /// Never moves; mass treated as infinite.
    Static,
    /// Fully simulated: gravity, collisions, impulses.
    Dynamic,
    /// Cart-controlled position; pushes dynamic bodies, isn't pushed by them.
    Kinematic,
}

#[derive(Copy, Clone, Debug)]
pub enum Shape {
    Aabb { extents: Vec3 },
    Sphere { radius: f32 },
}

/// Result of `raycast` and `material_at` queries (§10.1).
#[derive(Copy, Clone, Debug)]
pub struct Hit {
    pub pos: UVec3,
    pub material: u8,
    pub normal: IVec3,
    pub t: f32,
}

#[derive(Copy, Clone, Debug)]
pub struct SweepHit {
    pub t: f32,
    pub normal: IVec3,
    pub blocked_by_actor: Option<ActorId>,
}

/// Snapshot of a body's state, returned by `body_get`.
#[derive(Copy, Clone, Debug)]
pub struct BodyState {
    pub kind: BodyKind,
    pub shape: Shape,
    pub position: Vec3,
    pub velocity: Vec3,
    pub mass: f32,
    pub restitution: f32,
    pub friction: f32,
    pub layer: u8,
    pub mask: u8,
    pub sensor: bool,
}

/// Drained from `drain_collision_events` once per frame.
#[derive(Copy, Clone, Debug)]
pub struct CollisionEvent {
    pub a: BodyId,
    pub b: Option<BodyId>,    // None for body-vs-world
    pub point: Vec3,
    pub normal: Vec3,
    pub impulse: f32,
}

/// Globally tunable CA parameter ids. See SPEC.md §10.3.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CaParam {
    LiquidViscosity = 0,
    GranularAngleOfRepose = 1,
    GasDecayRate = 2,
    FlammableHeatThreshold = 3,
    FireLifetime = 4,
}
