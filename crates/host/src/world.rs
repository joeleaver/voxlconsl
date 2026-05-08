//! World state shared between cart-driven mutation and the renderer.
//!
//! v0.1.0: cart-managed multi-scene model. The cart can address up to
//! **256 scenes**, each a 512³ voxel grid (sparse 16³ chunk grid per
//! SPEC.md §13.6). All voxel mutations target the **active scene**;
//! `scene_set_active(id)` switches the active scene and lazy-allocates
//! it on first touch.
//!
//! The host's only responsibility is to swap which voxel grid is
//! active — actors, materials, prefabs, audio, save block all stay
//! cart-global. Carts that want per-scene cleanup (despawn enemies on
//! exit, etc.) handle that themselves; the host doesn't try to be
//! clever about it. See SPEC.md §3.6 for the design rationale.
//!
//! ## Memory model
//!
//! - `scenes: Vec<Option<Box<Scene>>>` length 256 — 2 KB upfront slot
//!   table, niche-optimized to 8 bytes per slot.
//! - Each populated `Scene` holds its own `Vec<Option<Box<ChunkState>>>`
//!   length 4096 — 32 KB per scene; only populated chunks add their
//!   ~33 KB dense buffer.
//! - Worst case (all 256 scenes populated): 8 MB for slot tables, plus
//!   chunk dense data. Practical carts populate a handful.

use voxlconsl_svo::{build, ChunkData, ChunkKey};
use voxlconsl_types::{Material, Vec3};

use crate::actors::ActorTable;
use crate::input::InputState;
use crate::macro_grid::MacroGrid;
use crate::prefabs::PrefabTable;
use crate::renderer::Camera;

const CHUNK_SIDE: u32 = build::CHUNK_SIZE;
const CHUNK_VOXELS: usize = (CHUNK_SIDE * CHUNK_SIDE * CHUNK_SIDE) as usize;
const WORLD_CHUNKS: u32 = 16;
const N_CHUNKS: usize = (WORLD_CHUNKS * WORLD_CHUNKS * WORLD_CHUNKS) as usize;
/// World side in voxels per scene (512).
pub const WORLD_SIDE: u32 = WORLD_CHUNKS * CHUNK_SIDE;
/// Maximum number of scenes per cart.
pub const MAX_SCENES: usize = 256;

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

/// One scene's chunk grid. Sparse — `chunks[k] = None` means the chunk
/// is uniformly air and not allocated.
pub struct Scene {
    chunks: Vec<Option<Box<ChunkState>>>,
}

impl Scene {
    fn new_empty() -> Self {
        let mut chunks: Vec<Option<Box<ChunkState>>> = Vec::with_capacity(N_CHUNKS);
        chunks.resize_with(N_CHUNKS, || None);
        Self { chunks }
    }

    pub fn chunks_slice(&self) -> &[Option<Box<ChunkState>>] {
        &self.chunks
    }

    pub fn chunk_at(&self, key: ChunkKey) -> Option<&ChunkState> {
        self.chunks.get(key.0 as usize)?.as_deref()
    }

    pub fn populated_chunk_count(&self) -> usize {
        self.chunks.iter().filter(|s| s.is_some()).count()
    }

    fn flush(&mut self) {
        for slot in self.chunks.iter_mut() {
            if let Some(cs) = slot.as_mut() {
                if cs.dirty {
                    cs.chunk = build::from_dense(&cs.dense);
                    cs.dirty = false;
                }
            }
        }
    }
}

/// All host state the cart can read or mutate via host imports.
pub struct WorldState {
    /// Sparse per-cart scene table. `None` = unallocated (uniform air).
    /// Lazy-allocated on first `set_voxel` / `fill_box` / scene activate
    /// that touches the slot.
    scenes: Vec<Option<Box<Scene>>>,
    active_scene: u8,
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
        let mut scenes: Vec<Option<Box<Scene>>> = Vec::with_capacity(MAX_SCENES);
        scenes.resize_with(MAX_SCENES, || None);
        Self {
            scenes,
            active_scene: 0,
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

    /// Switch the active scene. The new scene is lazy-allocated if it
    /// hasn't been touched before, so reading from a fresh scene
    /// returns uniform air without paying for chunk storage.
    /// IDs ≥ MAX_SCENES are silently rejected.
    pub fn scene_set_active(&mut self, id: u8) {
        // u8 caps at 255; MAX_SCENES is 256 — every u8 is in range.
        let _ = id;
        self.active_scene = id;
        self.ensure_active_scene();
    }

    pub fn scene_get_active(&self) -> u8 { self.active_scene }

    fn ensure_active_scene(&mut self) -> &mut Scene {
        let i = self.active_scene as usize;
        if self.scenes[i].is_none() {
            self.scenes[i] = Some(Box::new(Scene::new_empty()));
        }
        self.scenes[i].as_mut().unwrap()
    }

    fn active_scene_ref(&self) -> Option<&Scene> {
        self.scenes[self.active_scene as usize].as_deref()
    }

    /// Cart-driven world mutation, targeting the active scene. Coords
    /// ≥ `WORLD_SIDE` are silently rejected per §3.6.
    pub fn set_voxel(&mut self, x: u32, y: u32, z: u32, material: u8) {
        if x >= WORLD_SIDE || y >= WORLD_SIDE || z >= WORLD_SIDE {
            return;
        }
        // Setting air into an unallocated scene is a no-op.
        if material == 0 && self.active_scene_ref().is_none() {
            return;
        }
        let scene = self.ensure_active_scene();
        let (cx, cy, cz, lx, ly, lz) = split_world_coords(x, y, z);
        let key = ChunkKey::new(cx, cy, cz);
        let slot = &mut scene.chunks[key.0 as usize];
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
        let xs = min_x.min(WORLD_SIDE - 1);
        let ys = min_y.min(WORLD_SIDE - 1);
        let zs = min_z.min(WORLD_SIDE - 1);
        let xe = max_x.min(WORLD_SIDE - 1);
        let ye = max_y.min(WORLD_SIDE - 1);
        let ze = max_z.min(WORLD_SIDE - 1);
        if xs > xe || ys > ye || zs > ze {
            return;
        }
        // Filling air into an unallocated scene is a no-op.
        if material == 0 && self.active_scene_ref().is_none() {
            return;
        }
        let scene = self.ensure_active_scene();

        let cx_min = xs >> 5;
        let cx_max = xe >> 5;
        let cy_min = ys >> 5;
        let cy_max = ye >> 5;
        let cz_min = zs >> 5;
        let cz_max = ze >> 5;

        for cz in cz_min..=cz_max {
            for cy in cy_min..=cy_max {
                for cx in cx_min..=cx_max {
                    let lxs = if cx == cx_min { (xs & 31) as u8 } else { 0 };
                    let lxe = if cx == cx_max { (xe & 31) as u8 } else { 31 };
                    let lys = if cy == cy_min { (ys & 31) as u8 } else { 0 };
                    let lye = if cy == cy_max { (ye & 31) as u8 } else { 31 };
                    let lzs = if cz == cz_min { (zs & 31) as u8 } else { 0 };
                    let lze = if cz == cz_max { (ze & 31) as u8 } else { 31 };

                    let key = ChunkKey::new(cx as u8, cy as u8, cz as u8);
                    let slot = &mut scene.chunks[key.0 as usize];
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

    /// Reset the **active scene** to all-air. Other scenes are
    /// unaffected; this is per-scene, not cart-wide.
    pub fn clear_world(&mut self) {
        if let Some(scene) = self.scenes.get_mut(self.active_scene as usize).and_then(|s| s.as_mut()) {
            for slot in scene.chunks.iter_mut() {
                *slot = None;
            }
        }
    }

    pub fn set_material(&mut self, slot: u8, material: Material) {
        self.materials[slot as usize] = material;
    }

    /// Rebuild any dirty chunks in the **active scene**'s SVO. Renderer
    /// must call this once per frame before reading chunk data. Inactive
    /// scenes' dirty chunks are flushed lazily the next time their
    /// scene becomes active.
    pub fn flush(&mut self) {
        if let Some(scene) = self.scenes.get_mut(self.active_scene as usize).and_then(|s| s.as_mut()) {
            scene.flush();
        }
    }

    /// Look up a chunk in the active scene. None = uniform air.
    pub fn chunk_at(&self, key: ChunkKey) -> Option<&ChunkState> {
        self.active_scene_ref()?.chunk_at(key)
    }

    /// Borrow the active scene's chunk slot table. Returns an empty
    /// slice if the active scene is unallocated (renders as all sky).
    pub fn chunks_slice(&self) -> &[Option<Box<ChunkState>>] {
        self.active_scene_ref().map(|s| s.chunks_slice()).unwrap_or(&[])
    }

    /// Number of populated chunks in the active scene. Tests + telemetry.
    pub fn populated_chunk_count(&self) -> usize {
        self.active_scene_ref().map(|s| s.populated_chunk_count()).unwrap_or(0)
    }

    /// Number of populated scenes (allocated, even if their chunk
    /// table is empty). Tests + telemetry.
    pub fn populated_scene_count(&self) -> usize {
        self.scenes.iter().filter(|s| s.is_some()).count()
    }

    pub fn scene_chunk_count(&self, scene: u8) -> usize {
        self.scenes
            .get(scene as usize)
            .and_then(|s| s.as_deref())
            .map(|s| s.populated_chunk_count())
            .unwrap_or(0)
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
    fn empty_world_has_no_populated_scenes_or_chunks() {
        let w = WorldState::new();
        assert_eq!(w.populated_scene_count(), 0);
        assert_eq!(w.populated_chunk_count(), 0);
    }

    #[test]
    fn set_voxel_in_origin_chunk_lazy_allocates_active_scene() {
        let mut w = WorldState::new();
        w.set_voxel(5, 5, 5, 7);
        assert_eq!(w.populated_scene_count(), 1);
        assert_eq!(w.populated_chunk_count(), 1);
        let cs = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert_eq!(cs.dense[local_index(5, 5, 5)], 7);
    }

    #[test]
    fn set_voxel_in_far_chunk_routes_correctly() {
        let mut w = WorldState::new();
        w.set_voxel(40, 64, 100, 9);
        let cs = w.chunk_at(ChunkKey::new(1, 2, 3)).unwrap();
        assert_eq!(cs.dense[local_index(8, 0, 4)], 9);
        assert!(w.chunk_at(ChunkKey::new(0, 0, 0)).is_none());
    }

    #[test]
    fn set_voxel_air_in_empty_scene_is_noop() {
        let mut w = WorldState::new();
        w.set_voxel(40, 64, 100, 0);
        assert_eq!(w.populated_scene_count(), 0);
    }

    #[test]
    fn fill_box_with_air_in_empty_scene_doesnt_allocate() {
        let mut w = WorldState::new();
        w.fill_box(0, 0, 0, 100, 100, 100, 0);
        assert_eq!(w.populated_scene_count(), 0);
    }

    #[test]
    fn out_of_range_coords_silently_rejected() {
        let mut w = WorldState::new();
        w.set_voxel(WORLD_SIDE, 0, 0, 5);
        w.set_voxel(0, 5000, 0, 5);
        assert_eq!(w.populated_scene_count(), 0);
    }

    #[test]
    fn fill_box_spanning_two_chunks_populates_both_in_active_scene() {
        let mut w = WorldState::new();
        w.fill_box(28, 0, 0, 36, 1, 1, 4);
        assert_eq!(w.populated_chunk_count(), 2);
        let c0 = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        let c1 = w.chunk_at(ChunkKey::new(1, 0, 0)).unwrap();
        assert_eq!(c0.dense[local_index(28, 0, 0)], 4);
        assert_eq!(c1.dense[local_index(0, 0, 0)], 4);
    }

    #[test]
    fn clear_world_only_affects_active_scene() {
        let mut w = WorldState::new();
        w.set_voxel(5, 5, 5, 7);            // scene 0
        w.scene_set_active(1);
        w.set_voxel(5, 5, 5, 8);            // scene 1
        assert_eq!(w.scene_chunk_count(0), 1);
        assert_eq!(w.scene_chunk_count(1), 1);

        w.clear_world();                    // clears scene 1 only
        assert_eq!(w.scene_chunk_count(0), 1);
        assert_eq!(w.scene_chunk_count(1), 0);

        w.scene_set_active(0);
        let cs = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert_eq!(cs.dense[local_index(5, 5, 5)], 7);
    }

    #[test]
    fn voxels_dont_leak_across_scenes() {
        let mut w = WorldState::new();
        w.set_voxel(10, 0, 10, 7);          // scene 0
        w.scene_set_active(7);
        // Reading from scene 7's (10, 0, 10) — chunk (0,0,0) is unallocated.
        assert!(w.chunk_at(ChunkKey::new(0, 0, 0)).is_none());

        // Writing scene 7 doesn't touch scene 0.
        w.set_voxel(10, 0, 10, 9);
        let cs = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert_eq!(cs.dense[local_index(10, 0, 10)], 9);

        // Switch back to scene 0 — original data still there.
        w.scene_set_active(0);
        let cs = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert_eq!(cs.dense[local_index(10, 0, 10)], 7);
    }

    #[test]
    fn scene_set_active_to_unused_id_returns_empty_chunks_slice() {
        let mut w = WorldState::new();
        w.set_voxel(0, 0, 0, 5);
        w.scene_set_active(42);
        // Scene 42 was just allocated by scene_set_active (eager), but
        // it has no chunks.
        assert_eq!(w.populated_chunk_count(), 0);
        assert!(w.chunk_at(ChunkKey::new(0, 0, 0)).is_none());
    }

    #[test]
    fn scene_get_active_round_trips() {
        let mut w = WorldState::new();
        assert_eq!(w.scene_get_active(), 0);
        w.scene_set_active(255);
        assert_eq!(w.scene_get_active(), 255);
        w.scene_set_active(0);
        assert_eq!(w.scene_get_active(), 0);
    }

    #[test]
    fn flush_only_rebuilds_active_scene() {
        let mut w = WorldState::new();
        w.set_voxel(5, 5, 5, 7);
        w.scene_set_active(1);
        w.set_voxel(5, 5, 5, 8);
        // Both scenes have a dirty chunk now (each scene was active when set).
        assert!(w.scene_chunk_count(0) == 1);
        assert!(w.scene_chunk_count(1) == 1);

        // Flush while scene 1 is active.
        w.flush();
        // Scene 1's chunk should be clean.
        let cs1 = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert!(!cs1.dirty);

        // Switch back to scene 0 — its chunk is still dirty until flush.
        w.scene_set_active(0);
        let cs0 = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert!(cs0.dirty);
        w.flush();
        let cs0 = w.chunk_at(ChunkKey::new(0, 0, 0)).unwrap();
        assert!(!cs0.dirty);
    }
}
