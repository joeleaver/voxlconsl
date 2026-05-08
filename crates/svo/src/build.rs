//! Build a `ChunkData` from a dense voxel buffer.
//!
//! Used for v0.0.x test scenes and as the simplest correct path until the
//! incremental mutation API (§13.5) is implemented.
//!
//! Algorithm: recursive subdivision. At each node we check whether the
//! region is uniform; if so, emit a single leaf. Otherwise, subdivide into
//! 8 octants and recurse. Air-filled regions never get an entry — they're
//! simply absent from the parent's `valid_mask`.

use alloc::vec::Vec;

use crate::{ChunkData, ChunkHeader, Node};

/// Side length of a chunk in voxels (§13.1: depth-5 octree → 32³).
pub const CHUNK_SIZE: u32 = 32;

/// Build a chunk from a dense `[material; CHUNK_SIZE³]` buffer.
///
/// Indexing: `dense[(z * CHUNK_SIZE + y) * CHUNK_SIZE + x]`. Material `0`
/// is air. The resulting chunk has the most compact tree the buffer admits.
pub fn from_dense(dense: &[u8]) -> ChunkData {
    let n = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;
    assert_eq!(dense.len(), n, "dense buffer must be CHUNK_SIZE^3 bytes");

    // Whole-chunk uniform fast-path.
    let first = dense[0];
    if dense.iter().all(|&m| m == first) {
        if first == 0 {
            // All-air. Caller probably shouldn't even store this chunk, but
            // returning a defined "empty" form is convenient.
            return ChunkData::uniform(0);
        }
        return ChunkData::uniform(first);
    }

    // Build recursively. Reserve a placeholder root, then fill it in.
    let mut nodes = Vec::with_capacity(64);
    nodes.push(Node::leaf(0)); // placeholder; overwritten below
    let root = build_node(dense, 0, 0, 0, CHUNK_SIZE, &mut nodes);
    nodes[0] = root;

    ChunkData {
        header: ChunkHeader {
            flags: 0,
            material: 0,
            node_count: nodes.len() as u16,
        },
        nodes,
    }
}

/// Recursively build the node covering `[origin, origin + size)` and append
/// any sub-nodes to `nodes`. Returns the constructed node. The caller owns
/// where this node ends up living.
fn build_node(
    dense: &[u8],
    ox: u32, oy: u32, oz: u32,
    size: u32,
    nodes: &mut Vec<Node>,
) -> Node {
    // Scan the region for uniformity.
    let first = sample(dense, ox, oy, oz);
    let mut uniform = true;
    'outer: for z in oz..oz + size {
        for y in oy..oy + size {
            for x in ox..ox + size {
                if sample(dense, x, y, z) != first {
                    uniform = false;
                    break 'outer;
                }
            }
        }
    }
    if uniform {
        return Node::leaf(first);
    }

    // Not uniform → subdivide into 8 octants. Note: a non-uniform region of
    // size 1 is impossible (a 1³ cell is by definition uniform), so size > 1
    // here.
    debug_assert!(size > 1);
    let half = size / 2;

    // First pass: build child sub-nodes into a scratch vec while determining
    // which octants are non-air.
    let mut child_nodes: [Option<Node>; 8] = [None; 8];
    for k in 0..8 {
        let (cx, cy, cz) = octant_origin(ox, oy, oz, half, k);
        let child = build_node(dense, cx, cy, cz, half, nodes);
        // Skip all-air child entirely (don't include in valid_mask).
        if child.is_leaf() && child.material() == 0 {
            continue;
        }
        child_nodes[k as usize] = Some(child);
    }

    // Compute valid_mask and reserve contiguous slots in `nodes`.
    let mut valid_mask: u8 = 0;
    let mut count = 0u32;
    for (k, c) in child_nodes.iter().enumerate() {
        if c.is_some() {
            valid_mask |= 1 << k;
            count += 1;
        }
    }

    let first_child = nodes.len() as u16;
    for c in child_nodes.iter().flatten() {
        nodes.push(*c);
    }

    debug_assert_eq!(count, valid_mask.count_ones());
    Node::branch(valid_mask, first_child)
}

#[inline]
fn sample(dense: &[u8], x: u32, y: u32, z: u32) -> u8 {
    let i = ((z * CHUNK_SIZE + y) * CHUNK_SIZE + x) as usize;
    dense[i]
}

#[inline]
fn octant_origin(ox: u32, oy: u32, oz: u32, half: u32, k: u8) -> (u32, u32, u32) {
    // octant = (z << 2) | (y << 1) | x  per SPEC.md §13.3
    let dx = (k & 1) as u32 * half;
    let dy = ((k >> 1) & 1) as u32 * half;
    let dz = ((k >> 2) & 1) as u32 * half;
    (ox + dx, oy + dy, oz + dz)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dense_filled(material: u8) -> Vec<u8> {
        alloc::vec![material; (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize]
    }

    #[test]
    fn uniform_air_collapses_to_uniform() {
        let chunk = from_dense(&dense_filled(0));
        assert!(chunk.is_uniform());
        assert_eq!(chunk.header.material, 0);
        assert!(chunk.nodes.is_empty());
    }

    #[test]
    fn uniform_solid_collapses_to_uniform() {
        let chunk = from_dense(&dense_filled(7));
        assert!(chunk.is_uniform());
        assert_eq!(chunk.header.material, 7);
    }

    #[test]
    fn single_voxel_at_origin() {
        let mut dense = dense_filled(0);
        dense[0] = 5; // (0,0,0) → material 5
        let chunk = from_dense(&dense);
        assert!(!chunk.is_uniform());
        // Root must be a branch
        assert!(!chunk.nodes[0].is_leaf());
        // Total node count is small for a single-voxel chunk:
        // 5 levels of branches, each with valid_mask = 0b00000001 (only octant 0
        // is non-air), plus 1 leaf at the bottom. = 5 branches + 1 leaf = 6.
        assert!(chunk.nodes.len() <= 8, "got {} nodes", chunk.nodes.len());
    }
}
