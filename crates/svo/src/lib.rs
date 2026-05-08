//! Sparse voxel octree — see SPEC.md §13.
//!
//! Two-tier:
//!   - World level: 32×32×32 grid of `ChunkData`s, sparse.
//!   - Per-chunk:   depth-5 octree over a 32³ volume.
//!
//! This crate defines the canonical types and (eventually) traversal,
//! mutation, and serialization. v0.0.1 is types-only — no algorithms yet.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Chunk-level header. See SPEC.md §13.2.
///
/// On-disk layout for a uniform chunk is just this header (4 bytes).
/// For a sparse chunk, this header is followed by `node_count` × `Node` entries.
#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct ChunkHeader {
    /// Bit 0: `is_uniform`. Bits 1–7 reserved.
    pub flags: u8,
    /// Material index when `is_uniform`; ignored otherwise.
    pub material: u8,
    /// Number of `Node` entries that follow; 0 when uniform.
    pub node_count: u16,
}

impl ChunkHeader {
    pub const FLAG_UNIFORM: u8 = 1 << 0;

    pub const fn is_uniform(&self) -> bool {
        (self.flags & Self::FLAG_UNIFORM) != 0
    }
}

/// One SVO node. 4 bytes, self-describing via `is_leaf`.
///
/// See SPEC.md §13.3 for the bit layout.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Node(pub u32);

impl Node {
    const IS_LEAF_BIT: u32 = 1 << 31;

    /// Construct a leaf node carrying a material index.
    pub const fn leaf(material: u8) -> Self {
        Self(Self::IS_LEAF_BIT | material as u32)
    }

    /// Construct a branch node.
    pub const fn branch(valid_mask: u8, first_child: u16) -> Self {
        Self((valid_mask as u32) | ((first_child as u32) << 8))
    }

    pub const fn is_leaf(self) -> bool { (self.0 & Self::IS_LEAF_BIT) != 0 }

    pub const fn material(self) -> u8 { self.0 as u8 }

    pub const fn valid_mask(self) -> u8 { self.0 as u8 }

    pub const fn first_child(self) -> u16 { (self.0 >> 8) as u16 }
}

/// In-memory chunk: a header plus (if not uniform) a flat node array.
///
/// On disk, this is what's stored per chunk (see SPEC.md §13.2 / §13.6);
/// in RAM, the same layout is used directly.
#[derive(Clone, Debug, Default)]
pub struct ChunkData {
    pub header: ChunkHeader,
    pub nodes: Vec<Node>,
}

impl ChunkData {
    /// A chunk that is uniformly one material (typically air).
    pub const fn uniform(material: u8) -> Self {
        Self {
            header: ChunkHeader {
                flags: ChunkHeader::FLAG_UNIFORM,
                material,
                node_count: 0,
            },
            nodes: Vec::new(),
        }
    }

    pub const fn is_uniform(&self) -> bool { self.header.is_uniform() }
}

/// Compute child slot offset within a branch's contiguous child run.
///
/// Returns `None` when the requested octant is air (its bit is clear in
/// `valid_mask`).
pub fn child_offset(valid_mask: u8, octant: u8) -> Option<u8> {
    debug_assert!(octant < 8);
    if valid_mask & (1 << octant) == 0 {
        return None;
    }
    let lower = valid_mask & ((1 << octant) - 1);
    Some(lower.count_ones() as u8)
}

/// 12-bit packed `(cx, cy, cz)` chunk coordinate, 4 bits per axis (16
/// chunks per axis × 32 voxels each = 512³ world). See §13.6.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ChunkKey(pub u16);

impl ChunkKey {
    pub const fn new(cx: u8, cy: u8, cz: u8) -> Self {
        debug_assert!(cx < 16 && cy < 16 && cz < 16);
        Self((cx as u16) | ((cy as u16) << 4) | ((cz as u16) << 8))
    }

    pub const fn cx(self) -> u8 { (self.0 & 0x0F) as u8 }
    pub const fn cy(self) -> u8 { ((self.0 >> 4) & 0x0F) as u8 }
    pub const fn cz(self) -> u8 { ((self.0 >> 8) & 0x0F) as u8 }
}

// TODO: incremental mutation (§13.5) — descend / set / collapse upward
// TODO: serialization to/from raw bytes (matches §13.2 byte layout)
// TODO: world-level chunk index (§13.6)

pub mod build;
pub mod ray;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_layout_round_trip() {
        let leaf = Node::leaf(42);
        assert!(leaf.is_leaf());
        assert_eq!(leaf.material(), 42);

        let branch = Node::branch(0b10110100, 1234);
        assert!(!branch.is_leaf());
        assert_eq!(branch.valid_mask(), 0b10110100);
        assert_eq!(branch.first_child(), 1234);
    }

    #[test]
    fn child_offsets() {
        // valid_mask = 0b10110100 → octants 2, 4, 5, 7 are valid.
        // Their offsets in the children array: 0, 1, 2, 3.
        let m = 0b10110100;
        assert_eq!(child_offset(m, 0), None);
        assert_eq!(child_offset(m, 1), None);
        assert_eq!(child_offset(m, 2), Some(0));
        assert_eq!(child_offset(m, 3), None);
        assert_eq!(child_offset(m, 4), Some(1));
        assert_eq!(child_offset(m, 5), Some(2));
        assert_eq!(child_offset(m, 6), None);
        assert_eq!(child_offset(m, 7), Some(3));
    }

    #[test]
    fn chunk_key_round_trip() {
        // 4 bits per axis → max 15 per coord.
        let k = ChunkKey::new(5, 12, 15);
        assert_eq!(k.cx(), 5);
        assert_eq!(k.cy(), 12);
        assert_eq!(k.cz(), 15);
    }
}
