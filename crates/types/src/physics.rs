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

/// Result of a [`raycast`](crate::physics) query (§10.1).
///
/// `repr(C)` so the SDK and host agree on the wire layout for the
/// `*mut Hit` out pointer the cart passes through the host import.
/// `material` is a u8; the trailing `_pad` keeps `normal` properly
/// 4-byte-aligned.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct Hit {
    pub pos: UVec3,
    pub material: u8,
    pub _pad: [u8; 3],
    pub normal: IVec3,
    pub t: f32,
    /// `u32::MAX` when the hit is against the world; otherwise the id
    /// of the actor whose volume was struck. SDK exposes this as
    /// `Option<ActorId>` via [`Hit::actor_id`].
    pub actor: u32,
}

const _: () = assert!(core::mem::size_of::<Hit>() == 36);

impl Hit {
    pub const NO_ACTOR: u32 = u32::MAX;

    pub fn actor_id(&self) -> Option<ActorId> {
        if self.actor == Self::NO_ACTOR { None } else { Some(ActorId(self.actor)) }
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct SweepHit {
    pub t: f32,
    pub normal: IVec3,
    /// `u32::MAX` when the sweep was blocked by world voxels; otherwise
    /// the id of the actor that blocked it.
    pub blocked_by_actor: u32,
}

const _: () = assert!(core::mem::size_of::<SweepHit>() == 20);

impl SweepHit {
    pub const NO_ACTOR: u32 = u32::MAX;

    pub fn blocked_by_actor_id(&self) -> Option<ActorId> {
        if self.blocked_by_actor == Self::NO_ACTOR {
            None
        } else {
            Some(ActorId(self.blocked_by_actor))
        }
    }
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
