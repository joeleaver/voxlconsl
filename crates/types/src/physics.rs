//! Physics types — see SPEC.md §10.

use crate::math::{IVec3, UVec3, Vec3};
use crate::actor::ActorId;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct BodyId(pub u32);

impl BodyId {
    pub const INVALID: BodyId = BodyId(u32::MAX);
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BodyKind {
    /// Never moves; mass treated as infinite.
    Static = 0,
    /// Fully simulated: gravity, collisions, impulses.
    Dynamic = 1,
    /// Cart-controlled position; pushes dynamic bodies, isn't pushed by them.
    Kinematic = 2,
}

impl BodyKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => BodyKind::Static,
            1 => BodyKind::Dynamic,
            2 => BodyKind::Kinematic,
            _ => BodyKind::Dynamic,
        }
    }
}

/// Shape tag — first byte of a `Shape` payload over the WASM boundary.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ShapeTag {
    Aabb = 0,
    Sphere = 1,
}

impl ShapeTag {
    pub fn from_u8(v: u8) -> Self {
        match v { 1 => ShapeTag::Sphere, _ => ShapeTag::Aabb }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum Shape {
    /// Axis-aligned box of full extents `(width, height, depth)`. The
    /// body's `position` is its center.
    Aabb { extents: Vec3 },
    /// Sphere of `radius` centered at the body's `position`.
    Sphere { radius: f32 },
}

impl Shape {
    pub fn tag(&self) -> ShapeTag {
        match self { Shape::Aabb { .. } => ShapeTag::Aabb, Shape::Sphere { .. } => ShapeTag::Sphere }
    }

    /// Pack the shape into 3 floats for ABI passing. AABB stores its
    /// full extents (width, height, depth); sphere stores `(radius, 0, 0)`.
    pub fn to_floats(&self) -> [f32; 3] {
        match *self {
            Shape::Aabb { extents } => [extents.x, extents.y, extents.z],
            Shape::Sphere { radius } => [radius, 0.0, 0.0],
        }
    }

    pub fn from_parts(tag: ShapeTag, data: [f32; 3]) -> Self {
        match tag {
            ShapeTag::Aabb => Shape::Aabb { extents: Vec3::new(data[0], data[1], data[2]) },
            ShapeTag::Sphere => Shape::Sphere { radius: data[0] },
        }
    }

    /// Axis-aligned bounding box of the shape, around the origin. The
    /// body's world AABB is `position ± half-extents`.
    pub fn half_extents(&self) -> Vec3 {
        match *self {
            Shape::Aabb { extents } => Vec3::new(extents.x * 0.5, extents.y * 0.5, extents.z * 0.5),
            Shape::Sphere { radius } => Vec3::new(radius, radius, radius),
        }
    }
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

/// Snapshot of a body's state, returned by `body_get` (§10.2).
///
/// `repr(C)` — the host writes the byte form of this struct into cart
/// memory via the `out_ptr` argument to the host import. Fields are
/// ordered so that f32 / u32 entries land on natural alignment after
/// the leading u8 fields and their pad.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct BodyState {
    pub kind: u8,
    pub shape_tag: u8,
    pub layer: u8,
    pub mask: u8,
    pub sensor: u8,
    pub _pad: [u8; 3],
    /// AABB full extents `(w, h, d)`, or `(radius, 0, 0)` for sphere.
    pub shape: [f32; 3],
    pub position: Vec3,
    pub velocity: Vec3,
    pub mass: f32,
    pub restitution: f32,
    pub friction: f32,
    /// `u32::MAX` if the body is unattached; otherwise the attached actor's id.
    pub actor: u32,
}

const _: () = assert!(core::mem::size_of::<BodyState>() == 60);

impl BodyState {
    pub const NO_ACTOR: u32 = u32::MAX;

    pub fn body_kind(&self) -> BodyKind { BodyKind::from_u8(self.kind) }
    pub fn shape(&self) -> Shape { Shape::from_parts(ShapeTag::from_u8(self.shape_tag), self.shape) }
    pub fn actor_id(&self) -> Option<ActorId> {
        if self.actor == Self::NO_ACTOR { None } else { Some(ActorId(self.actor)) }
    }
}

/// Drained from `drain_collision_events` once per frame (§10.2).
///
/// `b == u32::MAX` is the body-vs-world case; otherwise `b` is the
/// second body's id.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct CollisionEvent {
    pub a: u32,
    pub b: u32,
    pub point: Vec3,
    pub normal: Vec3,
    pub impulse: f32,
}

const _: () = assert!(core::mem::size_of::<CollisionEvent>() == 36);

impl CollisionEvent {
    pub const WORLD: u32 = u32::MAX;

    pub fn a_id(&self) -> BodyId { BodyId(self.a) }
    pub fn b_id(&self) -> Option<BodyId> {
        if self.b == Self::WORLD { None } else { Some(BodyId(self.b)) }
    }
    pub fn is_world(&self) -> bool { self.b == Self::WORLD }
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
