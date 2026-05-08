//! Prefabs + copy-on-write — see SPEC.md §11.4.
//!
//! Prefabs are prebuilt voxel volumes the cart authors and references by
//! `PrefabId`. Multiple actors instancing the same prefab share one baked
//! `ChunkData` via `Rc<BakedVolume>`; only when an instanced actor is
//! mutated does it fork into its own buffer.
//!
//! ## v0.0.5 status
//!
//! - **Loading path:** carts call the host import `prefab_define(id, ptr,
//!   len, sx, sy, sz)` from `init` to populate the table. This is the
//!   v0.0.5 stand-in for the cart-format-driven path that the §7 World
//!   section will eventually use; the runtime API surface (§11.7
//!   `actor_spawn_from`, `actor_set_prefab`) is unchanged.
//! - **Bake routine:** only `Orientation::Up` is implemented. Other
//!   orientations fall back to the Up bake until the 24-orientation
//!   bake routine lands (next milestone, §11.3 / §11.5). The cache key is
//!   already `(PrefabId, Orientation)` so the upgrade is local.

use std::collections::HashMap;
use std::rc::Rc;

use voxlconsl_svo::{build, ChunkData};
use voxlconsl_types::{Orientation, PrefabId, U8Vec3};

/// Cart-authored prefab source data — kept around so we can re-bake at
/// any orientation on demand.
struct PrefabSource {
    dense: Vec<u8>,
    size: U8Vec3,
}

/// A baked, ready-to-render volume for a `(prefab, orientation)` pair.
///
/// Stores both the dense buffer (so a CoW fork can clone it cheaply
/// without re-walking the SVO) and the SVO the renderer actually
/// raycasts against.
pub struct BakedVolume {
    pub dense: Vec<u8>,
    pub size: U8Vec3,
    pub chunk: ChunkData,
}

impl BakedVolume {
    /// Build a baked volume from a dense source rotated to the requested
    /// orientation. See SPEC.md §11.3 for the 24-element orientation
    /// group; the rotation here is a signed axis permutation, no
    /// trigonometry, so cost is one O(N) scan over the source voxels.
    fn bake(src: &PrefabSource, orientation: Orientation) -> Self {
        if matches!(orientation, Orientation::Up) {
            // Identity fast-path: skip the per-voxel rotate loop.
            let extents = [src.size.x as usize, src.size.y as usize, src.size.z as usize];
            let chunk = build_padded_chunk(&src.dense, extents);
            return Self { dense: src.dense.clone(), size: src.size, chunk };
        }
        let cols = orientation_matrix(orientation);
        let (dst_dense, dst_size) = rotate_dense_by_matrix(&src.dense, src.size, cols);
        let dst_extents = [dst_size.x as usize, dst_size.y as usize, dst_size.z as usize];
        let chunk = build_padded_chunk(&dst_dense, dst_extents);
        Self { dense: dst_dense, size: dst_size, chunk }
    }
}

/// 3×3 signed permutation matrix for an `Orientation`, in column-major
/// order: `[right, up, fwd]`. Each column is a signed world unit axis
/// where exactly one component is in `{-1, +1}`.
pub fn orientation_matrix(o: Orientation) -> [[i8; 3]; 3] {
    let (up, fwd) = o.axes();
    let right = cross(up, fwd);
    [right, up, fwd]
}

/// Transpose a 3×3 signed matrix. For a rotation matrix this is also
/// the inverse.
pub fn matrix_transpose(m: [[i8; 3]; 3]) -> [[i8; 3]; 3] {
    let mut t = [[0i8; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            t[i][j] = m[j][i];
        }
    }
    t
}

/// Multiply two signed permutation matrices (column-major). Used to
/// compose orientations (`R = A · B`).
pub fn matmul(a: [[i8; 3]; 3], b: [[i8; 3]; 3]) -> [[i8; 3]; 3] {
    let mut c = [[0i8; 3]; 3];
    for col in 0..3 {
        for row in 0..3 {
            let mut s: i32 = 0;
            for k in 0..3 {
                s += a[k][row] as i32 * b[col][k] as i32;
            }
            c[col][row] = s as i8;
        }
    }
    c
}

/// Rotate a dense voxel buffer by an arbitrary signed permutation
/// matrix. `cols` columns are interpreted as `[right, up, fwd]`: each
/// column is the signed world axis that the source basis maps to.
/// Returns the new dense buffer and its (possibly permuted) size.
pub fn rotate_dense_by_matrix(
    src_dense: &[u8],
    src_size: U8Vec3,
    cols: [[i8; 3]; 3],
) -> (Vec<u8>, U8Vec3) {
    // For each destination world axis k (0=X, 1=Y, 2=Z), find which
    // source axis (column index) maps to it, and the sign.
    let mut src_for_dst: [(usize, bool); 3] = [(0, false); 3];
    for s in 0..3 {
        let col = cols[s];
        for k in 0..3 {
            if col[k] != 0 {
                src_for_dst[k] = (s, col[k] < 0);
            }
        }
    }

    let src_extents = [src_size.x as usize, src_size.y as usize, src_size.z as usize];
    let dst_extents = [
        src_extents[src_for_dst[0].0],
        src_extents[src_for_dst[1].0],
        src_extents[src_for_dst[2].0],
    ];

    let dst_n = dst_extents[0] * dst_extents[1] * dst_extents[2];
    let mut dst_dense = vec![0u8; dst_n];

    let sx = src_extents[0];
    let sy = src_extents[1];
    let sz = src_extents[2];

    for zs in 0..sz {
        for ys in 0..sy {
            for xs in 0..sx {
                let src_coords = [xs, ys, zs];
                let xd = transform_axis(src_coords, src_extents, src_for_dst[0]);
                let yd = transform_axis(src_coords, src_extents, src_for_dst[1]);
                let zd = transform_axis(src_coords, src_extents, src_for_dst[2]);
                let src_idx = (zs * sy + ys) * sx + xs;
                let dst_idx = (zd * dst_extents[1] + yd) * dst_extents[0] + xd;
                dst_dense[dst_idx] = src_dense[src_idx];
            }
        }
    }

    let dst_size = U8Vec3::new(
        dst_extents[0] as u8,
        dst_extents[1] as u8,
        dst_extents[2] as u8,
    );
    (dst_dense, dst_size)
}

/// Build a 32³ padded `ChunkData` from a sub-extent dense buffer. Voxels
/// outside the source extent are air. Used both by prefab bakes and
/// owned-actor SVO rebuilds.
pub fn build_padded_chunk(dense: &[u8], extents: [usize; 3]) -> ChunkData {
    let pad_side = build::CHUNK_SIZE as usize;
    let mut padded = vec![0u8; pad_side * pad_side * pad_side];
    let [sx, sy, sz] = extents;
    for z in 0..sz {
        for y in 0..sy {
            for x in 0..sx {
                let s = (z * sy + y) * sx + x;
                let d = (z * pad_side + y) * pad_side + x;
                padded[d] = dense[s];
            }
        }
    }
    build::from_dense(&padded)
}

fn cross(a: [i8; 3], b: [i8; 3]) -> [i8; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn transform_axis(
    src_coords: [usize; 3],
    src_extents: [usize; 3],
    (src_axis, neg): (usize, bool),
) -> usize {
    let v = src_coords[src_axis];
    if neg { src_extents[src_axis] - 1 - v } else { v }
}

/// Per-cart prefab table + bake cache.
pub struct PrefabTable {
    sources: HashMap<PrefabId, PrefabSource>,
    /// Baked-volume cache. Multiple actors instancing the same key share
    /// one `Rc<BakedVolume>` until they fork via mutation.
    cache: HashMap<(PrefabId, Orientation), Rc<BakedVolume>>,
}

impl PrefabTable {
    pub fn new() -> Self {
        Self {
            sources: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    /// Register (or replace) a prefab's source dense buffer.
    /// Replacing invalidates the bake cache for that prefab.
    pub fn define(&mut self, id: PrefabId, dense: Vec<u8>, size: U8Vec3) {
        let expected_len = (size.x as usize) * (size.y as usize) * (size.z as usize);
        if dense.len() != expected_len {
            // Defensive: a malformed cart shouldn't crash the host; just
            // refuse the definition.
            return;
        }
        if size.x as u32 > build::CHUNK_SIZE
            || size.y as u32 > build::CHUNK_SIZE
            || size.z as u32 > build::CHUNK_SIZE
        {
            return;
        }
        self.sources.insert(id, PrefabSource { dense, size });
        self.cache.retain(|(pid, _), _| *pid != id);
    }

    /// Look up or bake a `(prefab, orientation)` combination.
    /// Returns `None` if the prefab id is unknown.
    pub fn bake(&mut self, id: PrefabId, orientation: Orientation) -> Option<Rc<BakedVolume>> {
        if let Some(rc) = self.cache.get(&(id, orientation)) {
            return Some(Rc::clone(rc));
        }
        let src = self.sources.get(&id)?;
        let baked = Rc::new(BakedVolume::bake(src, orientation));
        self.cache.insert((id, orientation), Rc::clone(&baked));
        Some(baked)
    }

    pub fn contains(&self, id: PrefabId) -> bool {
        self.sources.contains_key(&id)
    }
}

impl Default for PrefabTable {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dense(size: U8Vec3, fill: u8) -> Vec<u8> {
        vec![fill; (size.x as usize) * (size.y as usize) * (size.z as usize)]
    }

    #[test]
    fn define_and_bake_returns_same_rc_for_repeat_lookups() {
        let mut t = PrefabTable::new();
        let size = U8Vec3::new(4, 4, 4);
        t.define(PrefabId(1), make_dense(size, 7), size);

        let a = t.bake(PrefabId(1), Orientation::Up).expect("baked");
        let b = t.bake(PrefabId(1), Orientation::Up).expect("baked");
        // The cache returns the same allocation, not just an equal one.
        assert!(Rc::ptr_eq(&a, &b));
    }

    #[test]
    fn unknown_prefab_returns_none() {
        let mut t = PrefabTable::new();
        assert!(t.bake(PrefabId(99), Orientation::Up).is_none());
    }

    #[test]
    fn redefine_invalidates_cache() {
        let mut t = PrefabTable::new();
        let size = U8Vec3::new(2, 2, 2);
        t.define(PrefabId(1), make_dense(size, 7), size);
        let a = t.bake(PrefabId(1), Orientation::Up).expect("baked");
        t.define(PrefabId(1), make_dense(size, 8), size);
        let b = t.bake(PrefabId(1), Orientation::Up).expect("baked");
        assert!(!Rc::ptr_eq(&a, &b));
        assert_eq!(b.dense[0], 8);
    }

    #[test]
    fn malformed_define_is_rejected() {
        let mut t = PrefabTable::new();
        let size = U8Vec3::new(2, 2, 2);
        // length 4 != 8 expected
        t.define(PrefabId(1), vec![1, 2, 3, 4], size);
        assert!(!t.contains(PrefabId(1)));
    }

    #[test]
    fn oversize_define_is_rejected() {
        let mut t = PrefabTable::new();
        // CHUNK_SIZE is 32; 33 should be rejected.
        let size = U8Vec3::new(33, 1, 1);
        t.define(PrefabId(1), vec![0; 33], size);
        assert!(!t.contains(PrefabId(1)));
    }

    /// Build a 2×2×2 test prefab where each voxel encodes its (x,y,z)
    /// coords as a single byte: `1 + (z*4 + y*2 + x)`. That makes
    /// it easy to reason about where each voxel ends up after a
    /// rotation: byte 1 is at (0,0,0), byte 8 is at (1,1,1).
    fn make_coord_prefab() -> (PrefabTable, PrefabId) {
        let mut t = PrefabTable::new();
        let size = U8Vec3::new(2, 2, 2);
        let mut dense = vec![0u8; 8];
        for z in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    dense[(z * 2 + y) * 2 + x] = 1 + (z * 4 + y * 2 + x) as u8;
                }
            }
        }
        t.define(PrefabId(1), dense, size);
        (t, PrefabId(1))
    }

    fn voxel_at(b: &BakedVolume, x: usize, y: usize, z: usize) -> u8 {
        let [sx, sy, _] = [b.size.x as usize, b.size.y as usize, b.size.z as usize];
        b.dense[(z * sy + y) * sx + x]
    }

    #[test]
    fn up_rot180_flips_x_and_z() {
        let (mut t, id) = make_coord_prefab();
        let baked = t.bake(id, Orientation::UpRot180).expect("baked");
        assert_eq!(baked.size, U8Vec3::new(2, 2, 2));
        // UpRot180: (x, y, z) → ((sx-1)-x, y, (sz-1)-z).
        // Source byte 1 at (0,0,0) lands at dest (1, 0, 1).
        assert_eq!(voxel_at(&baked, 1, 0, 1), 1);
        assert_eq!(voxel_at(&baked, 0, 0, 0), 1 + (1 * 4 + 0 * 2 + 1));
        // Y is unchanged — top-row source voxels stay on top.
        assert_eq!(voxel_at(&baked, 1, 1, 1), 1 + (0 * 4 + 1 * 2 + 0));
    }

    #[test]
    fn east_up_permutes_extents() {
        let mut t = PrefabTable::new();
        // 5×7×3 — non-cubic to make the permutation visible.
        let size = U8Vec3::new(5, 7, 3);
        let dense = vec![1u8; 5 * 7 * 3];
        t.define(PrefabId(1), dense, size);
        // EastUp: up = +X, fwd = +Y, so right = +X × +Y = +Z.
        // The columns of R are (right, up, fwd) = (+Z, +X, +Y), meaning:
        //   source +X (size 5) → world +Z
        //   source +Y (size 7) → world +X
        //   source +Z (size 3) → world +Y
        // → world (X, Y, Z) extents = (7, 3, 5).
        let baked = t.bake(PrefabId(1), Orientation::EastUp).expect("baked");
        assert_eq!(baked.size, U8Vec3::new(7, 3, 5));
    }

    #[test]
    fn rotation_is_lossless_voxel_count() {
        // For any orientation, the rotated volume has the same total
        // non-zero voxel count as the source — rotation is a permutation.
        let (mut t, id) = make_coord_prefab();
        let src_count = 8;  // all 8 cells are non-zero (1..=8).
        for o in ALL_ORIENTATIONS.iter().copied() {
            let baked = t.bake(id, o).expect("baked");
            let count = baked.dense.iter().filter(|&&v| v != 0).count();
            assert_eq!(count, src_count, "voxel count differs for {:?}", o);
        }
    }

    #[test]
    fn applying_orientation_four_times_around_y_returns_identity_layout() {
        // UpRot90 applied 4 times to (x,y,z) should equal the identity.
        // Easier check: bake UpRot90 four times in a row and confirm
        // the final dense matches the source.
        let mut t = PrefabTable::new();
        let size = U8Vec3::new(3, 1, 2);
        let src_dense: Vec<u8> = (1..=6).collect(); // 3*1*2 = 6 unique bytes
        t.define(PrefabId(1), src_dense.clone(), size);

        // Build a rotated volume by hand: take baked.dense and feed it
        // back as a new prefab; rotate again. After 4 rotations we
        // should be back to the source layout.
        let mut current_dense = src_dense.clone();
        let mut current_size = size;
        for _ in 0..4 {
            let mut tt = PrefabTable::new();
            tt.define(PrefabId(2), current_dense.clone(), current_size);
            let b = tt.bake(PrefabId(2), Orientation::UpRot90).expect("baked");
            current_dense = b.dense.clone();
            current_size = b.size;
        }
        assert_eq!(current_size, size, "size didn't return to identity");
        assert_eq!(current_dense, src_dense, "voxels didn't return to identity");
    }

    #[test]
    fn east_up_rotation_chirality() {
        // EastUp: up = +X, fwd = +Y, right = +Z.
        // Source +X (right) → world +Z. So source byte 2 at (1, 0, 0)
        // (the rightmost-bottom-front voxel) lands at dst (0, 0, 1).
        let (mut t, id) = make_coord_prefab();
        let baked = t.bake(id, Orientation::EastUp).expect("baked");
        assert_eq!(voxel_at(&baked, 0, 0, 1), 2);
        // And the source +Y (up) corner — byte 3 at (0, 1, 0) — should
        // land at world +X (= EastUp's "up").
        assert_eq!(voxel_at(&baked, 1, 0, 0), 3);
    }

    const ALL_ORIENTATIONS: [Orientation; 24] = [
        Orientation::Up, Orientation::UpRot90, Orientation::UpRot180, Orientation::UpRot270,
        Orientation::Down, Orientation::DownRot90, Orientation::DownRot180, Orientation::DownRot270,
        Orientation::EastUp, Orientation::EastUpRot90, Orientation::EastUpRot180, Orientation::EastUpRot270,
        Orientation::WestUp, Orientation::WestUpRot90, Orientation::WestUpRot180, Orientation::WestUpRot270,
        Orientation::NorthUp, Orientation::NorthUpRot90, Orientation::NorthUpRot180, Orientation::NorthUpRot270,
        Orientation::SouthUp, Orientation::SouthUpRot90, Orientation::SouthUpRot180, Orientation::SouthUpRot270,
    ];
}
