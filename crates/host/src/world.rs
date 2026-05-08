//! World state shared between cart-driven mutation and the renderer.
//!
//! v0.0.3: a single 32³ chunk maintained as a dense buffer for ease of
//! mutation, lazily rebuilt into an SVO when the cart is finished writing
//! to it (before the next render). The dense + SVO duality is a
//! v0.0.x convenience — eventually mutations apply directly to the SVO
//! per §13.5 and there's no dense shadow.

use voxlconsl_svo::{build, ChunkData};
use voxlconsl_types::{Material, Vec3};

use crate::actors::ActorTable;
use crate::input::InputState;
use crate::prefabs::PrefabTable;
use crate::renderer::Camera;

const CHUNK_SIDE: u32 = build::CHUNK_SIZE;
const CHUNK_VOXELS: usize = (CHUNK_SIDE * CHUNK_SIDE * CHUNK_SIDE) as usize;

/// All host state the cart can read or mutate via host imports.
///
/// Single-chunk, single-actor-set placeholder for v0.0.3. The renderer
/// reads `chunk` (rebuilt from `dense` when `dirty` is true), `materials`,
/// `camera`, and `sun_dir` / `sky_*` to produce a frame.
pub struct WorldState {
    dense: Vec<u8>,
    chunk: ChunkData,
    pub materials: Box<[Material; 256]>,
    pub camera: Camera,
    pub sun_dir: Vec3,
    pub sky_top: u8,
    pub sky_horizon: u8,
    pub input: InputState,
    pub actors: ActorTable,
    pub prefabs: PrefabTable,
    dirty: bool,
}

impl WorldState {
    pub fn new() -> Self {
        Self {
            dense: vec![0; CHUNK_VOXELS],
            chunk: ChunkData::uniform(0),
            materials: Box::new([Material::AIR; 256]),
            camera: Camera::new(
                Vec3::new(50.0, 30.0, 50.0),
                Vec3::new(16.0, 8.0, 16.0),
                60.0,
            ),
            sun_dir: Vec3::new(-0.6, 0.8, 0.4),
            sky_top: ((7 << 2) | 0),    // deep blue, shade 0
            sky_horizon: ((6 << 2) | 0), // sky blue, shade 0
            input: InputState::new(),
            actors: ActorTable::new(),
            prefabs: PrefabTable::new(),
            dirty: true,
        }
    }

    /// Cart-driven world mutation.
    pub fn set_voxel(&mut self, x: u32, y: u32, z: u32, material: u8) {
        if x >= CHUNK_SIDE || y >= CHUNK_SIDE || z >= CHUNK_SIDE {
            return;
        }
        let i = ((z * CHUNK_SIDE + y) * CHUNK_SIDE + x) as usize;
        if self.dense[i] != material {
            self.dense[i] = material;
            self.dirty = true;
        }
    }

    pub fn fill_box(
        &mut self,
        min_x: u32, min_y: u32, min_z: u32,
        max_x: u32, max_y: u32, max_z: u32,
        material: u8,
    ) {
        let xs = min_x.min(CHUNK_SIDE - 1);
        let ys = min_y.min(CHUNK_SIDE - 1);
        let zs = min_z.min(CHUNK_SIDE - 1);
        let xe = max_x.min(CHUNK_SIDE - 1);
        let ye = max_y.min(CHUNK_SIDE - 1);
        let ze = max_z.min(CHUNK_SIDE - 1);
        for z in zs..=ze {
            for y in ys..=ye {
                for x in xs..=xe {
                    let i = ((z * CHUNK_SIDE + y) * CHUNK_SIDE + x) as usize;
                    self.dense[i] = material;
                }
            }
        }
        self.dirty = true;
    }

    pub fn clear_world(&mut self) {
        self.dense.fill(0);
        self.dirty = true;
    }

    pub fn set_material(&mut self, slot: u8, material: Material) {
        self.materials[slot as usize] = material;
    }

    /// Rebuild the SVO from the dense buffer if it's been mutated.
    /// Renderer must call this immediately before reading `chunk()`.
    pub fn flush(&mut self) {
        if self.dirty {
            self.chunk = build::from_dense(&self.dense);
            self.dirty = false;
        }
    }

    pub fn chunk(&self) -> &ChunkData { &self.chunk }
}

impl Default for WorldState {
    fn default() -> Self { Self::new() }
}
