//! Ray-AABB and ray-vs-chunk traversal — see SPEC.md §13.4.
//!
//! Recursive front-to-back DFS. At each branch we sort the valid octants by
//! ray-AABB entry distance and recurse closest-first; the first leaf hit
//! wins. Depth ≤ 5 means recursion is bounded.

use voxlconsl_types::Vec3;

use crate::{ChunkData, Node};

/// A successful raycast result against a chunk.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RayHit {
    /// Voxel-space coordinates of the hit voxel within the chunk (0..32 per axis).
    pub voxel: (u32, u32, u32),
    pub material: u8,
    /// Outward face normal in chunk-local axes; components in {-1, 0, +1}.
    pub normal: (i32, i32, i32),
    /// Distance along the ray from origin to the hit point (in voxels).
    pub t: f32,
}

impl ChunkData {
    /// Cast a ray through the chunk's local space.
    ///
    /// `origin` and `dir` are in chunk-local coordinates: the chunk
    /// occupies the AABB `[0, 32]³`. `dir` should be normalized; non-zero
    /// components are required (zero components produce infinite slabs and
    /// are handled but pessimize the math). `max_t` clamps the ray length.
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_t: f32) -> Option<RayHit> {
        let chunk_size = crate::build::CHUNK_SIZE as f32;

        // Uniform fast path.
        if self.is_uniform() {
            if self.header.material == 0 {
                return None;
            }
            let (t_in, t_out) = ray_aabb(origin, dir, Vec3::ZERO, Vec3::splat(chunk_size))?;
            if t_out < 0.0 || t_in > max_t {
                return None;
            }
            let t = t_in.max(0.0);
            return Some(make_hit(origin, dir, t, Vec3::ZERO, Vec3::splat(chunk_size), self.header.material));
        }

        // Trace the root branch (nodes[0] by convention).
        let (t_in, t_out) = ray_aabb(origin, dir, Vec3::ZERO, Vec3::splat(chunk_size))?;
        if t_out < 0.0 || t_in > max_t {
            return None;
        }
        let t_min = t_in.max(0.0);
        let t_max = t_out.min(max_t);
        traverse(
            &self.nodes,
            0,
            origin, dir,
            t_min, t_max,
            Vec3::ZERO, Vec3::splat(chunk_size),
        )
    }
}

fn traverse(
    nodes: &[Node],
    node_idx: usize,
    origin: Vec3,
    dir: Vec3,
    t_min: f32,
    t_max: f32,
    aabb_min: Vec3,
    aabb_max: Vec3,
) -> Option<RayHit> {
    if t_min > t_max {
        return None;
    }
    let node = nodes[node_idx];
    if node.is_leaf() {
        if node.material() == 0 {
            return None;
        }
        return Some(make_hit(origin, dir, t_min, aabb_min, aabb_max, node.material()));
    }

    let valid = node.valid_mask();
    let first_child = node.first_child() as usize;
    let center = (aabb_min + aabb_max) * 0.5;

    // Compute t_enter for each valid octant.
    let mut entries: [(u8, f32, f32); 8] = [(0, f32::INFINITY, f32::INFINITY); 8];
    let mut count = 0;
    for k in 0..8 {
        if valid & (1 << k) == 0 {
            continue;
        }
        let (cmin, cmax) = sub_aabb(aabb_min, aabb_max, center, k);
        let Some((t_a, t_b)) = ray_aabb(origin, dir, cmin, cmax) else { continue };
        if t_b < t_min || t_a > t_max {
            continue;
        }
        entries[count] = (k, t_a.max(t_min), t_b.min(t_max));
        count += 1;
    }

    // Insertion sort by t_a (front-to-back). count ≤ 8.
    for i in 1..count {
        let cur = entries[i];
        let mut j = i;
        while j > 0 && entries[j - 1].1 > cur.1 {
            entries[j] = entries[j - 1];
            j -= 1;
        }
        entries[j] = cur;
    }

    for &(octant, ta, tb) in &entries[..count] {
        let child_offset =
            (valid & ((1 << octant) - 1)).count_ones() as usize;
        let child_idx = first_child + child_offset;
        let (cmin, cmax) = sub_aabb(aabb_min, aabb_max, center, octant);
        if let Some(hit) = traverse(nodes, child_idx, origin, dir, ta, tb, cmin, cmax) {
            return Some(hit);
        }
    }

    None
}

/// Slab-method ray-AABB intersection. Returns `(t_enter, t_exit)` or `None`
/// when the ray misses entirely.
fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<(f32, f32)> {
    let inv = dir.componentwise_recip();
    let t1 = (
        (min.x - origin.x) * inv.x,
        (min.y - origin.y) * inv.y,
        (min.z - origin.z) * inv.z,
    );
    let t2 = (
        (max.x - origin.x) * inv.x,
        (max.y - origin.y) * inv.y,
        (max.z - origin.z) * inv.z,
    );
    let tmin_x = t1.0.min(t2.0);
    let tmin_y = t1.1.min(t2.1);
    let tmin_z = t1.2.min(t2.2);
    let tmax_x = t1.0.max(t2.0);
    let tmax_y = t1.1.max(t2.1);
    let tmax_z = t1.2.max(t2.2);

    let t_enter = tmin_x.max(tmin_y).max(tmin_z);
    let t_exit = tmax_x.min(tmax_y).min(tmax_z);
    if t_enter > t_exit || t_exit < 0.0 {
        None
    } else {
        Some((t_enter, t_exit))
    }
}

#[inline]
fn sub_aabb(min: Vec3, max: Vec3, center: Vec3, octant: u8) -> (Vec3, Vec3) {
    let cmin = Vec3 {
        x: if octant & 1 != 0 { center.x } else { min.x },
        y: if octant & 2 != 0 { center.y } else { min.y },
        z: if octant & 4 != 0 { center.z } else { min.z },
    };
    let cmax = Vec3 {
        x: if octant & 1 != 0 { max.x } else { center.x },
        y: if octant & 2 != 0 { max.y } else { center.y },
        z: if octant & 4 != 0 { max.z } else { center.z },
    };
    (cmin, cmax)
}

fn make_hit(
    origin: Vec3, dir: Vec3,
    t: f32,
    aabb_min: Vec3, aabb_max: Vec3,
    material: u8,
) -> RayHit {
    // Determine which face the ray entered through. The entering plane is
    // the one whose tNear == t (the slab argmax that produced the entry).
    let inv = dir.componentwise_recip();
    let (mut tx0, mut tx1) = ((aabb_min.x - origin.x) * inv.x, (aabb_max.x - origin.x) * inv.x);
    let (mut ty0, mut ty1) = ((aabb_min.y - origin.y) * inv.y, (aabb_max.y - origin.y) * inv.y);
    let (mut tz0, mut tz1) = ((aabb_min.z - origin.z) * inv.z, (aabb_max.z - origin.z) * inv.z);
    if tx0 > tx1 { core::mem::swap(&mut tx0, &mut tx1); }
    if ty0 > ty1 { core::mem::swap(&mut ty0, &mut ty1); }
    if tz0 > tz1 { core::mem::swap(&mut tz0, &mut tz1); }

    // The entering plane is the one whose tNear == t (within a small epsilon).
    let nx = if (tx0 - t).abs() < 1e-4 { if dir.x > 0.0 { -1 } else { 1 } } else { 0 };
    let ny = if (ty0 - t).abs() < 1e-4 { if dir.y > 0.0 { -1 } else { 1 } } else { 0 };
    let nz = if (tz0 - t).abs() < 1e-4 { if dir.z > 0.0 { -1 } else { 1 } } else { 0 };
    // If none matched (rare numerical case), fall back to axis with largest entry.
    let normal = if nx | ny | nz != 0 {
        (nx, ny, nz)
    } else if tx0 >= ty0 && tx0 >= tz0 {
        (if dir.x > 0.0 { -1 } else { 1 }, 0, 0)
    } else if ty0 >= tz0 {
        (0, if dir.y > 0.0 { -1 } else { 1 }, 0)
    } else {
        (0, 0, if dir.z > 0.0 { -1 } else { 1 })
    };

    let voxel = (
        aabb_min.x as u32,
        aabb_min.y as u32,
        aabb_min.z as u32,
    );

    RayHit { voxel, material, normal, t }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::{from_dense, CHUNK_SIZE};

    #[test]
    fn ray_misses_empty_chunk() {
        let dense = alloc::vec![0u8; (CHUNK_SIZE.pow(3)) as usize];
        let chunk = from_dense(&dense);
        let hit = chunk.raycast(
            Vec3::new(-10.0, 16.0, 16.0),
            Vec3::new(1.0, 0.0, 0.0),
            1000.0,
        );
        assert!(hit.is_none());
    }

    #[test]
    fn ray_hits_uniform_filled_chunk() {
        let dense = alloc::vec![3u8; (CHUNK_SIZE.pow(3)) as usize];
        let chunk = from_dense(&dense);
        let hit = chunk.raycast(
            Vec3::new(-10.0, 16.0, 16.0),
            Vec3::new(1.0, 0.0, 0.0),
            1000.0,
        ).unwrap();
        assert_eq!(hit.material, 3);
        assert!((hit.t - 10.0).abs() < 0.01);
        assert_eq!(hit.normal, (-1, 0, 0));
    }

    #[test]
    fn ray_hits_single_voxel() {
        let mut dense = alloc::vec![0u8; (CHUNK_SIZE.pow(3)) as usize];
        // Place a single voxel at (5, 5, 5) with material 9.
        let i = ((5u32 * CHUNK_SIZE + 5) * CHUNK_SIZE + 5) as usize;
        dense[i] = 9;
        let chunk = from_dense(&dense);

        // Aim a ray straight at it.
        let hit = chunk.raycast(
            Vec3::new(-1.0, 5.5, 5.5),
            Vec3::new(1.0, 0.0, 0.0),
            100.0,
        );
        let hit = hit.expect("expected to hit the voxel");
        assert_eq!(hit.material, 9);
        assert_eq!(hit.voxel, (5, 5, 5));
        assert_eq!(hit.normal, (-1, 0, 0));
    }
}
