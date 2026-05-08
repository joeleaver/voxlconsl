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
    /// Build a baked volume from a dense source at the requested
    /// orientation. v0.0.5: `Up` is identity; other orientations fall
    /// back to identity with a TODO. See SPEC.md §11.3 for the 24
    /// orientations the next milestone will implement.
    fn bake(src: &PrefabSource, orientation: Orientation) -> Self {
        match orientation {
            Orientation::Up => Self::bake_identity(src),
            // TODO(orientations): 23 non-Up cases per §11.3. For now we
            // bake them as Up so cart code calling `actor_spawn_from(p,
            // SouthDown)` doesn't crash — it just renders the prefab in
            // its authored orientation.
            _ => Self::bake_identity(src),
        }
    }

    fn bake_identity(src: &PrefabSource) -> Self {
        let pad_side = build::CHUNK_SIZE;
        let mut padded = vec![0u8; (pad_side * pad_side * pad_side) as usize];
        let sx = src.size.x as u32;
        let sy = src.size.y as u32;
        let sz = src.size.z as u32;
        for z in 0..sz {
            for y in 0..sy {
                for x in 0..sx {
                    let s = ((z * sy + y) * sx + x) as usize;
                    let d = ((z * pad_side + y) * pad_side + x) as usize;
                    padded[d] = src.dense[s];
                }
            }
        }
        let chunk = build::from_dense(&padded);
        Self {
            dense: src.dense.clone(),
            size: src.size,
            chunk,
        }
    }
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
}
