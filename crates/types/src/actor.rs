//! Actor identifiers and orientation — see SPEC.md §11.

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ActorId(pub u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct PrefabId(pub u16);

/// 24 cube-symmetry orientations. See SPEC.md §11.3.
///
/// `Up` is the identity. Rotation around the world Y axis (yaw) is *not*
/// part of `Orientation`; yaw is applied at render time per ray and
/// composes on top of whichever orientation is baked into the volume.
//
// TODO: spec lists representative names; the canonical 24-element layout
// will be filled in when the bake routine is implemented.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Orientation {
    Up = 0,
    Down = 1,
    NorthUp = 2,
    NorthDown = 3,
    SouthUp = 4,
    SouthDown = 5,
    EastUp = 6,
    EastDown = 7,
    WestUp = 8,
    WestDown = 9,
    UpRot90 = 10,
    UpRot180 = 11,
    UpRot270 = 12,
    DownRot90 = 13,
    DownRot180 = 14,
    DownRot270 = 15,
    // Remaining 8 symmetries to be enumerated when baking is implemented.
}

impl Default for Orientation {
    fn default() -> Self { Self::Up }
}

/// Bitset of actors potentially overlapping a query. See SPEC.md §10.1.
///
/// Concrete representation is a host concern; this is just the type the
/// cart sees back from `aabb_overlap_actors`.
#[derive(Copy, Clone, Debug, Default)]
#[repr(transparent)]
pub struct ActorMask(pub u64);
