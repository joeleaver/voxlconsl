//! World state shared between cart-driven mutation and the renderer.
//!
//! v0.0.8: a sparse 32³ grid of 32³ chunks per SPEC.md §13.6, covering
//! the full 1024³ voxel world. Chunks are allocated lazily on first
//! mutation; the slot table is a dense `Vec<Option<Box<ChunkState>>>`
//! length 32768 (256 KB upfront, niche-optimized 8 bytes per slot) so
//! per-chunk lookup is `O(1)` array indexing rather than hash table
//! probing — important for the per-ray inner loop that visits up to
//! one chunk per macro-cell.
//!
//! Each populated `ChunkState` holds:
//!   - `dense`: 32 KB material buffer for cart-driven edits.
//!   - `chunk`: cached SVO rebuilt from `dense` on `flush()`.
//!   - `dirty`: set by mutations, cleared by `flush()`.
//!
//! The dense + SVO duality is a v0.0.x convenience — eventually
//! mutations apply directly to the SVO per §13.5 and there's no dense
//! shadow.

use voxlconsl_svo::{build, ChunkData, ChunkKey};
use voxlconsl_types::{Material, Vec3};

use crate::actors::ActorTable;
use crate::input::InputState;
use crate::macro_grid::MacroGrid;
use crate::prefabs::PrefabTable;
use crate::renderer::Camera;

const CHUNK_SIDE: u32 = build::CHUNK_SIZE;
const CHUNK_VOXELS: usize = (CHUNK_SIDE * CHUNK_SIDE * CHUNK_SIDE) as usize;
/// Cells per axis in the world; world side = `WORLD_CHUNKS * CHUNK_SIDE`.
const WORLD_CHUNKS: u32 = 32;
const N_CHUNKS: usize = (WORLD_CHUNKS * WORLD_CHUNKS * WORLD_CHUNKS) as usize;
/// World side in voxels (1024).
pub const WORLD_SIDE: u32 = WORLD_CHUNKS * CHUNK_SIDE;

/// One 32³ chunk: cart-mutable dense buffer + cached SVO.
pub struct ChunkState {
    pub dense: Vec<u8>,
    pub chunk: ChunkData,
    pub dirty: bool,
}

impl ChunkState {
    fn new_air() -> Self {
        Self {
            dense: vec![0; CHUNK_VOXELS],
            chunk: ChunkData::uniform(0),
            dirty: false,
        }
    }
}

/// All host state the cart can read or mutate via host imports.
pub struct WorldState {
    /// Sparse chunk slots, indexed by `ChunkKey.0 as usize`. `None` =
    /// the chunk is uniform air and not allocated. Empty world =
    /// 32 768 × 8 bytes = 256 KB upfront; populated chunks add ~33 KB
    /// each from their dense buffer.
    chunks: Vec<Option<Box<ChunkState>>>,
    pub materials: Box<[Material; 256]>,
    pub camera: Camera,
    pub sun_dir: Vec3,
    pub sky_top: u8,
    pub sky_horizon: u8,
    pub input: InputState,
    pub actors: ActorTable,
    pub prefabs: PrefabTable,
    pub macro_grid: MacroGrid,
}

impl WorldState {
    pub fn new() -> Self {
        let mut chunks: Vec<Option<Box<ChunkState>>> = Vec::with_capacity(N_CHUNKS);
        chunks.resize_with(N_CHUNKS, || None);
        Self {
            chunks,
            materials: Box::new([Material::AIR; 256]),
            camera: Camera::new(
                Vec3::new(50.0, 30.0, 50.0),
                Vec3::new(16.0, 8.0, 16.0),
                60.0,
            ),
            sun_dir: Vec3::new(-0.6, 0.8, 0.4),
            sky_top: ((7 << 2) | 0),
            sky_horizon: ((6 << 2) | 0),
            input: InputState::new(),
            actors: ActorTable::new(),
            prefabs: PrefabTable::new(),
            macro_grid: MacroGrid::new(),
        }
    }

    /// Cart-driven world mutation. Coords ≥ `WORLD_SIDE` are silently
    /// rejected per §3.6's "clamped or rejected — implementation must
    /// be consistent" guidance.
    pub fn set_voxel(&mut self, x: u32, y: u32, z: u32, material: u8) {
        if x >= WORLD_SIDE || y >= WORLD_SIDE || z >= WORLD_SIDE {
            return;
        }
        let (cx, cy, cz, lx, ly, lz) = split_world_coords(x, y, z);
        let key = ChunkKey::new(cx, cy, cz);
        let slot = &mut self.chunks[key.0 as usize];
        // Setting air into a non-existent chunk is a no-op. Avoids
        // allocating a chunk full of air just to write the same.
        if slot.is_none() {
            if material == 0 {
                return;
            }
            *slot = Some(Box::new(ChunkState::new_air()));
        }
        let cs = slot.as_mut().unwrap();
        let i = local_index(lx, ly, lz);
        if cs.dense[i] != material {
            cs.dense[i] = material;
            cs.dirty = true;
        }
    }

    pub fn fill_box(
        &mut self,
        min_x: u32, min_y: u32, min_z: u32,
        max_x: u32, max_y: u32, max_z: u32,
        material: u8,
    ) {
        // Clamp into [0, WORLD_SIDE).
        let xs = min_x.min(WORLD_SIDE - 1);
        let ys = min_y.min(WORLD_SIDE - 1);
        let zs = min_z.min(WORLD_SIDE - 1);
        let xe = max_x.min(WORLD_SIDE - 1);
        let ye = max_y.min(WORLD_SIDE - 1);
        let ze = max_z.min(WORLD_SIDE - 1);
        if xs > xe || ys > ye || zs > ze {
            return;
        }

        // Iterate the chunks the box overlaps.
        let cx_min = xs >> 5;
        let cx_max = xe >> 5;
        let cy_min = ys >> 5;
        let cy_max = ye >> 5;
        let cz_min = zs >> 5;
        let cz_max = ze >> 5;

        for cz in cz_min..=cz_max {
            for cy in cy_min..=cy_max {
                for cx in cx_min..=cx_max {
                    // Box's intersection with this chunk, in chunk-local coords.
                    let lxs = if cx == cx_min { (xs & 31) as u8 } else { 0 };
                    let lxe = if cx == cx_max { (xe & 31) as u8 } else { 31 };
                    let lys = if cy == cy_min { (ys & 31) as u8 } else { 0 };
                    let lye = if cy == cy_max { (ye & 31) as u8 } else { 31 };
                    let lzs = if cz == cz_min { (zs & 31) as u8 } else { 0 };
                    let lze = if cz == cz_max { (ze & 31) as u8 } else { 31 };

                    let key = ChunkKey::new(cx as u8, cy as u8, cz as u8);
                    let slot = &mut self.chunks[key.0 as usize];
                    if slot.is_none() {
                        if material == 0 {
                            continue;
                        }
                        *slot = Some(Box::new(ChunkState::new_air()));
                    }
                    let cs = slot.as_mut().unwrap();
                    for lz in lzs..=lze {
                        for ly in lys..=lye {
                            for lx in lxs..=lxe {
                                let i = local_index(lx, ly, lz);
                                cs.dense[i] = material;
                            }
                        }
                    }
                    cs.dirty = true;
                }
            }
        }
    }

    /// Reset the entire mutable world to all-air. Drops every populated
    /// chunk; subsequent reads see uniform air.
    pub fn clear_world(&mut self) {
        for slot in self.chunks.iter_mut() {
            *slot = None;
        }
    }

    pub fn set_material(&mut self, slot: u8, material: Material) {
        self.materials[slot as usize] = material;
    }

    /// Rebuild any dirty chunk's SVO. Renderer must call this once per
    /// frame before reading chunk data.
    pub fn flush(&mut self) {
        for slot in self.chunks.iter_mut() {
            if let Some(cs) = slot.as_mut() {
                if cs.dirty {
                    cs.chunk = build::from_dense(&cs.dense);
                    cs.dirty = false;
                }
            }
        }
    }

    /// Look up a chunk by key. Returns `None` if uniform air.
    pub fn chunk_at(&self, key: ChunkKey) -> Option<&ChunkState> {
        self.chunks.get(key.0 as usize)?.as_deref()
    }

    /// Borrow the entire chunk slot table — used by the renderer as a
    /// flat per-cell lookup indexed by `ChunkKey.0 as usize`.
    pub fn chunks_slice(&self) -> &[Option<Box<ChunkState>>] {
        &self.chunks
    }

    /// Number of currently allocated chunks. Useful for tests + telemetry.
    pub fn populated_chunk_count(&self) -> usize {
        self.chunks.iter().filter(|s| s.is_some()).count()
    }
}

impl Default for WorldState {
    fn default() -> Self { Self::new() }
}

#[inline]
fn split_world_coords(x: u32, y: u32, z: u32) -> (u8, u8, u8, u8, u8, u8) {
    (
        (x >> 5) as u8, (y >> 5) as u8, (z >> 5) as u8,
        (x & 31) as u8, (y & 31) as u8, (z & 31) as u8,
    )
}

#[inline]
fn local_index(lx: u8, ly: u8, lz: u8) -> usize {
    ((lz as usize * CHUNK_SIDE as usize) + ly as usize) * CHUNK_SIDE as usize + lx as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_world_has_no_populated_chunks() {
        let w = WorldState::new();
        assert_eq!(w.populated_chunk_count(), 0);
    }

    #[test]
    fn set_voxel_in_origin_chunk_allocates_only_that_chunk() {
        let mut w = WorldState::new();
        w.set_voxel(5, 5, 5, 7);
        assert_eq!(w.populated_chunk_count(), 1);
        let cs = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert_eq!(cs.dense[local_index(5, 5, 5)], 7);
        assert!(cs.dirty);
    }

    #[test]
    fn set_voxel_in_far_chunk_routes_correctly() {
        let mut w = WorldState::new();
        w.set_voxel(40, 64, 100, 9);
        // (40, 64, 100) → chunk (1, 2, 3), local (8, 0, 4)
        let cs = w.chunk_at(ChunkKey::new(1, 2, 3)).unwrap();
        assert_eq!(cs.dense[local_index(8, 0, 4)], 9);
        assert!(w.chunk_at(ChunkKey::new(0, 0, 0)).is_none());
    }

    #[test]
    fn set_voxel_air_in_empty_chunk_is_noop() {
        let mut w = WorldState::new();
        w.set_voxel(40, 64, 100, 0);
        assert_eq!(w.populated_chunk_count(), 0);
    }

    #[test]
    fn out_of_range_coords_silently_rejected() {
        let mut w = WorldState::new();
        w.set_voxel(WORLD_SIDE, 0, 0, 5);
        w.set_voxel(0, 5000, 0, 5);
        assert_eq!(w.populated_chunk_count(), 0);
    }

    #[test]
    fn fill_box_spanning_two_chunks_populates_both() {
        let mut w = WorldState::new();
        // Box (28..36, 0..2, 0..2) crosses the cx=0 / cx=1 boundary.
        w.fill_box(28, 0, 0, 36, 1, 1, 4);
        assert_eq!(w.populated_chunk_count(), 2);
        let c0 = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        let c1 = w.chunk_at(ChunkKey::new(1, 0, 0)).unwrap();
        assert_eq!(c0.dense[local_index(28, 0, 0)], 4);
        assert_eq!(c0.dense[local_index(31, 1, 1)], 4);
        assert_eq!(c1.dense[local_index(0, 0, 0)], 4);
        assert_eq!(c1.dense[local_index(4, 1, 1)], 4);
    }

    #[test]
    fn clear_world_drops_everything() {
        let mut w = WorldState::new();
        w.set_voxel(5, 5, 5, 7);
        w.set_voxel(500, 500, 500, 7);
        assert_eq!(w.populated_chunk_count(), 2);
        w.clear_world();
        assert_eq!(w.populated_chunk_count(), 0);
    }

    #[test]
    fn flush_rebuilds_dirty_chunk_svo() {
        let mut w = WorldState::new();
        w.set_voxel(5, 5, 5, 7);
        let cs = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert!(cs.dirty);
        w.flush();
        let cs = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert!(!cs.dirty);
        // Non-uniform chunk after the edit.
        assert!(!cs.chunk.is_uniform());
    }

    #[test]
    fn flush_skips_clean_chunks() {
        let mut w = WorldState::new();
        w.set_voxel(5, 5, 5, 7);
        w.flush();
        // Second flush should be a no-op (nothing dirty); read SVO is
        // unchanged.
        let nodes_before = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap().chunk.nodes.clone();
        w.flush();
        let nodes_after = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap().chunk.nodes.clone();
        assert_eq!(nodes_before, nodes_after);
    }

    #[test]
    fn fill_box_with_air_doesnt_allocate_empty_chunks() {
        let mut w = WorldState::new();
        w.fill_box(100, 100, 100, 200, 200, 200, 0);
        assert_eq!(w.populated_chunk_count(), 0);
    }
}
