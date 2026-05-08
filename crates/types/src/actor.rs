//! Actor identifiers and orientation — see SPEC.md §11.

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ActorId(pub u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct PrefabId(pub u16);

/// Identifier for one of the cart's scenes — a 1024³ voxel grid the
/// cart can populate and switch to with `scene_set_active`. Up to 256
/// per cart. `Scene 0` is the default active scene at boot. See
/// SPEC.md §3.6 / §13.6.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct SceneId(pub u8);

/// 24 cube-symmetry orientations. See SPEC.md §11.3.
///
/// Each orientation is uniquely specified by a pair of signed world
/// axes: which world direction the actor's local **+Y** (up) ends up
/// pointing, and which world direction the actor's local **+Z**
/// (forward) ends up pointing. The remaining axis (right) is the cross
/// product of those two.
///
/// The 6 possible up-axes (±X, ±Y, ±Z) × 4 yaw rotations around each
/// = 24 orientations. They group into 6 "stances":
///
///   - **Up-stance** (`up = +Y`) — Up, UpRot90, UpRot180, UpRot270
///   - **Down-stance** (`up = -Y`) — Down, DownRot90, DownRot180, DownRot270
///   - **EastUp-stance** (`up = +X`) — EastUp, EastUpRot90, …, EastUpRot270
///   - **WestUp-stance** (`up = -X`) — WestUp, WestUpRot90, …, WestUpRot270
///   - **NorthUp-stance** (`up = +Z`) — NorthUp, NorthUpRot90, …, NorthUpRot270
///   - **SouthUp-stance** (`up = -Z`) — SouthUp, SouthUpRot90, …, SouthUpRot270
///
/// `RotN` denotes N degrees of CCW rotation about the stance's up-axis
/// (right-hand rule), starting from the stance's identity forward.
///
/// `Up` is the identity (`up = +Y`, `forward = +Z`). Yaw, applied per
/// frame at render time around world +Y (§11.3), composes on top of
/// whichever orientation is baked into the volume.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Orientation {
    // up = +Y
    Up           = 0,
    UpRot90      = 1,
    UpRot180     = 2,
    UpRot270     = 3,
    // up = -Y
    Down         = 4,
    DownRot90    = 5,
    DownRot180   = 6,
    DownRot270   = 7,
    // up = +X
    EastUp       = 8,
    EastUpRot90  = 9,
    EastUpRot180 = 10,
    EastUpRot270 = 11,
    // up = -X
    WestUp       = 12,
    WestUpRot90  = 13,
    WestUpRot180 = 14,
    WestUpRot270 = 15,
    // up = +Z
    NorthUp       = 16,
    NorthUpRot90  = 17,
    NorthUpRot180 = 18,
    NorthUpRot270 = 19,
    // up = -Z
    SouthUp       = 20,
    SouthUpRot90  = 21,
    SouthUpRot180 = 22,
    SouthUpRot270 = 23,
}

impl Default for Orientation {
    fn default() -> Self { Self::Up }
}

impl Orientation {
    /// Returns the orientation's `(up_world, fwd_world)` as signed unit
    /// axes. Each axis is `[i8; 3]` with exactly one nonzero component
    /// in `{-1, +1}`. The right axis is `up × fwd` and can be derived
    /// where needed.
    ///
    /// `Up` is the identity: `up = (0,1,0)`, `fwd = (0,0,1)`.
    pub const fn axes(self) -> ([i8; 3], [i8; 3]) {
        // Stance ups.
        const POS_Y: [i8; 3] = [0,  1, 0];
        const NEG_Y: [i8; 3] = [0, -1, 0];
        const POS_X: [i8; 3] = [ 1, 0, 0];
        const NEG_X: [i8; 3] = [-1, 0, 0];
        const POS_Z: [i8; 3] = [0, 0,  1];
        const NEG_Z: [i8; 3] = [0, 0, -1];

        match self {
            // up = +Y; fwd cycles +Z, +X, -Z, -X (CCW about +Y, right-hand rule)
            Orientation::Up        => (POS_Y, POS_Z),
            Orientation::UpRot90   => (POS_Y, POS_X),
            Orientation::UpRot180  => (POS_Y, NEG_Z),
            Orientation::UpRot270  => (POS_Y, NEG_X),

            // up = -Y; CCW about -Y is CW about +Y → fwd cycles +Z, -X, -Z, +X
            Orientation::Down        => (NEG_Y, POS_Z),
            Orientation::DownRot90   => (NEG_Y, NEG_X),
            Orientation::DownRot180  => (NEG_Y, NEG_Z),
            Orientation::DownRot270  => (NEG_Y, POS_X),

            // up = +X; CCW about +X → fwd cycles +Y, +Z, -Y, -Z
            Orientation::EastUp        => (POS_X, POS_Y),
            Orientation::EastUpRot90   => (POS_X, POS_Z),
            Orientation::EastUpRot180  => (POS_X, NEG_Y),
            Orientation::EastUpRot270  => (POS_X, NEG_Z),

            // up = -X; CCW about -X → fwd cycles +Y, -Z, -Y, +Z
            Orientation::WestUp        => (NEG_X, POS_Y),
            Orientation::WestUpRot90   => (NEG_X, NEG_Z),
            Orientation::WestUpRot180  => (NEG_X, NEG_Y),
            Orientation::WestUpRot270  => (NEG_X, POS_Z),

            // up = +Z; CCW about +Z → fwd cycles +X, +Y, -X, -Y
            Orientation::NorthUp        => (POS_Z, POS_X),
            Orientation::NorthUpRot90   => (POS_Z, POS_Y),
            Orientation::NorthUpRot180  => (POS_Z, NEG_X),
            Orientation::NorthUpRot270  => (POS_Z, NEG_Y),

            // up = -Z; CCW about -Z → fwd cycles +X, -Y, -X, +Y
            Orientation::SouthUp        => (NEG_Z, POS_X),
            Orientation::SouthUpRot90   => (NEG_Z, NEG_Y),
            Orientation::SouthUpRot180  => (NEG_Z, NEG_X),
            Orientation::SouthUpRot270  => (NEG_Z, POS_Y),
        }
    }
}

/// Bitset of actors potentially overlapping a query. See SPEC.md §10.1.
///
/// Concrete representation is a host concern; this is just the type the
/// cart sees back from `aabb_overlap_actors`.
#[derive(Copy, Clone, Debug, Default)]
#[repr(transparent)]
pub struct ActorMask(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    /// `up × fwd = right` should always be a unit signed axis (no zero
    /// vector and no parallel pair). Sanity-check all 24.
    #[test]
    fn axes_form_orthogonal_signed_basis() {
        const ALL: &[Orientation] = &[
            Orientation::Up, Orientation::UpRot90, Orientation::UpRot180, Orientation::UpRot270,
            Orientation::Down, Orientation::DownRot90, Orientation::DownRot180, Orientation::DownRot270,
            Orientation::EastUp, Orientation::EastUpRot90, Orientation::EastUpRot180, Orientation::EastUpRot270,
            Orientation::WestUp, Orientation::WestUpRot90, Orientation::WestUpRot180, Orientation::WestUpRot270,
            Orientation::NorthUp, Orientation::NorthUpRot90, Orientation::NorthUpRot180, Orientation::NorthUpRot270,
            Orientation::SouthUp, Orientation::SouthUpRot90, Orientation::SouthUpRot180, Orientation::SouthUpRot270,
        ];
        assert_eq!(ALL.len(), 24);
        let mut keys = [0i32; 24];
        for (i, &o) in ALL.iter().enumerate() {
            let (up, fwd) = o.axes();
            // Must be perpendicular signed unit axes.
            let nonzero_up: i32 = up.iter().filter(|&&c| c != 0).count() as i32;
            let nonzero_fwd: i32 = fwd.iter().filter(|&&c| c != 0).count() as i32;
            assert_eq!(nonzero_up, 1, "{:?}: up not unit signed axis", o);
            assert_eq!(nonzero_fwd, 1, "{:?}: fwd not unit signed axis", o);
            // Up's nonzero index ≠ fwd's nonzero index → perpendicular.
            let up_axis = up.iter().position(|&c| c != 0).unwrap();
            let fwd_axis = fwd.iter().position(|&c| c != 0).unwrap();
            assert_ne!(up_axis, fwd_axis, "{:?}: up and fwd parallel", o);
            // Encode (up, fwd) into a single int key (5 bits suffice per
            // signed-axis: 6 values × 6 values < 64) so we can dedupe
            // without depending on std collections in this no_std crate.
            keys[i] = pack_key(up, fwd);
        }
        // Distinct (up, fwd) pairs across all 24 orientations.
        for i in 0..24 {
            for j in (i + 1)..24 {
                assert_ne!(keys[i], keys[j], "duplicate at {} and {}: {:?} == {:?}", i, j, ALL[i], ALL[j]);
            }
        }
    }

    fn pack_key(up: [i8; 3], fwd: [i8; 3]) -> i32 {
        // Map a signed unit axis to 0..=5 (3 axes × 2 signs).
        let pack_axis = |v: [i8; 3]| -> i32 {
            for (i, &c) in v.iter().enumerate() {
                if c != 0 {
                    return (i as i32) * 2 + if c < 0 { 1 } else { 0 };
                }
            }
            -1
        };
        pack_axis(up) * 6 + pack_axis(fwd)
    }

    fn cross(a: [i8; 3], b: [i8; 3]) -> [i8; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
}
