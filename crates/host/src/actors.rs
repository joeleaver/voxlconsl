//! Actors — see SPEC.md §11.
//!
//! v0.0.4 minimum-viable scope:
//!   - Actor table with up to 256 actors (`actor_spawn` / `actor_despawn`).
//!   - Per-actor: position, yaw, visibility, dense voxel buffer (default 16³).
//!   - Volume editing API: `actor_set_voxel`, `actor_fill_box`, `actor_clear`.
//!   - Renderer composites visible actors with the world chunk.
//!
//! Deferred to later passes:
//!   - Prefabs + copy-on-write sharing (§11.4)
//!   - 24 fixed orientations + bake cache (§11.3, §11.5)
//!   - Anchor offsets — currently always volume's local origin
//!   - Macro-grid binning (§11.6) — currently iterates all visible actors
//!   - Body attachment (§10.2)
//!   - Volume-size mutation, prefab `actor_spawn_from`

use voxlconsl_svo::{build, ChunkData};
use voxlconsl_types::{ActorId, U8Vec3, Vec3};

/// Default per-actor volume side length. Cart can leave most voxels empty.
pub const DEFAULT_VOLUME_SIDE: u8 = 16;
const MAX_ACTORS: usize = 256;

pub struct Actor {
    pub position: Vec3,
    pub yaw: f32,
    pub visible: bool,
    /// Dense voxel buffer, size_x × size_y × size_z bytes, row-major
    /// (x fastest, then y, then z). Material `0` is empty.
    pub volume_dense: Vec<u8>,
    pub volume_size: U8Vec3,
    /// Cached SVO derived from `volume_dense`. Rebuilt by `flush()` when
    /// `dirty` is true.
    pub volume_chunk: ChunkData,
    pub dirty: bool,
}

impl Actor {
    fn new() -> Self {
        let side = DEFAULT_VOLUME_SIDE;
        let size = U8Vec3::new(side, side, side);
        let n = (side as usize).pow(3);
        Self {
            position: Vec3::ZERO,
            yaw: 0.0,
            visible: true,
            volume_dense: vec![0; n],
            volume_size: size,
            volume_chunk: ChunkData::uniform(0),
            dirty: false,
        }
    }

    fn voxel_index(&self, x: u8, y: u8, z: u8) -> Option<usize> {
        if x >= self.volume_size.x || y >= self.volume_size.y || z >= self.volume_size.z {
            return None;
        }
        let s = self.volume_size.x as usize;
        let sy = self.volume_size.y as usize;
        Some(((z as usize * sy) + y as usize) * s + x as usize)
    }

    pub fn set_voxel(&mut self, x: u8, y: u8, z: u8, material: u8) {
        if let Some(i) = self.voxel_index(x, y, z) {
            if self.volume_dense[i] != material {
                self.volume_dense[i] = material;
                self.dirty = true;
            }
        }
    }

    pub fn get_voxel(&self, x: u8, y: u8, z: u8) -> u8 {
        self.voxel_index(x, y, z)
            .map(|i| self.volume_dense[i])
            .unwrap_or(0)
    }

    pub fn fill_box(&mut self, min: U8Vec3, max: U8Vec3, material: u8) {
        let xs = min.x.min(self.volume_size.x.saturating_sub(1));
        let ys = min.y.min(self.volume_size.y.saturating_sub(1));
        let zs = min.z.min(self.volume_size.z.saturating_sub(1));
        let xe = max.x.min(self.volume_size.x.saturating_sub(1));
        let ye = max.y.min(self.volume_size.y.saturating_sub(1));
        let ze = max.z.min(self.volume_size.z.saturating_sub(1));
        for z in zs..=ze {
            for y in ys..=ye {
                for x in xs..=xe {
                    let i = self.voxel_index(x, y, z).unwrap();
                    self.volume_dense[i] = material;
                }
            }
        }
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        self.volume_dense.fill(0);
        self.dirty = true;
    }

    /// Rebuild the SVO from the dense buffer if dirty. Renderer must call
    /// this before reading `volume_chunk`.
    ///
    /// v0.0.4 uses `build::from_dense` which assumes a 32³ buffer; for
    /// smaller actor volumes we build from a padded 32³ block. (The padded
    /// approach is wasteful but works for the minimum-viable pass — the
    /// proper fix is making `from_dense` accept variable extents.)
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let pad_side = build::CHUNK_SIZE;
        let mut padded = vec![0u8; (pad_side * pad_side * pad_side) as usize];
        let sx = self.volume_size.x as u32;
        let sy = self.volume_size.y as u32;
        let sz = self.volume_size.z as u32;
        for z in 0..sz {
            for y in 0..sy {
                for x in 0..sx {
                    let src = ((z * sy + y) * sx + x) as usize;
                    let dst = ((z * pad_side + y) * pad_side + x) as usize;
                    padded[dst] = self.volume_dense[src];
                }
            }
        }
        self.volume_chunk = build::from_dense(&padded);
        self.dirty = false;
    }

    /// World-space AABB of the actor's volume after yaw rotation.
    /// Position is the world location of the volume's local (0, 0, 0)
    /// corner; yaw rotates around that corner about the world Y axis.
    pub fn world_aabb(&self) -> (Vec3, Vec3) {
        let (sx, sy, sz) = (
            self.volume_size.x as f32,
            self.volume_size.y as f32,
            self.volume_size.z as f32,
        );
        // 8 corners of [0, size] in actor-local coords.
        let corners_local = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(sx,  0.0, 0.0),
            Vec3::new(0.0, sy,  0.0),
            Vec3::new(sx,  sy,  0.0),
            Vec3::new(0.0, 0.0, sz ),
            Vec3::new(sx,  0.0, sz ),
            Vec3::new(0.0, sy,  sz ),
            Vec3::new(sx,  sy,  sz ),
        ];
        let cosy = self.yaw.cos();
        let siny = self.yaw.sin();

        let mut min = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
        let mut max = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        for c in corners_local {
            let world = Vec3::new(
                self.position.x + c.x * cosy - c.z * siny,
                self.position.y + c.y,
                self.position.z + c.x * siny + c.z * cosy,
            );
            min = min.min(world);
            max = max.max(world);
        }
        (min, max)
    }

    /// Transform a world-space ray into actor-local space (inverse yaw +
    /// translate). Returns (origin, dir). Direction is rotated but not
    /// scaled; ray-parameter `t` values are the same in both spaces.
    pub fn world_to_local_ray(&self, origin: Vec3, dir: Vec3) -> (Vec3, Vec3) {
        let cosy = (-self.yaw).cos();
        let siny = (-self.yaw).sin();
        let p = origin - self.position;
        let local_origin = Vec3::new(
            p.x * cosy - p.z * siny,
            p.y,
            p.x * siny + p.z * cosy,
        );
        let local_dir = Vec3::new(
            dir.x * cosy - dir.z * siny,
            dir.y,
            dir.x * siny + dir.z * cosy,
        );
        (local_origin, local_dir)
    }

    /// Rotate an actor-local face normal back into world space (forward yaw).
    /// Face normals come out of SVO traversal in actor-local axes; lighting
    /// math in `shade` wants world-space normals.
    pub fn local_to_world_normal(&self, normal: Vec3) -> Vec3 {
        let cosy = self.yaw.cos();
        let siny = self.yaw.sin();
        Vec3::new(
            normal.x * cosy - normal.z * siny,
            normal.y,
            normal.x * siny + normal.z * cosy,
        )
    }
}

/// Slot-based table. `ActorId.0` indexes into `slots`. Despawn marks the
/// slot vacant (slot becomes `None`); subsequent spawns reuse free slots.
pub struct ActorTable {
    slots: Vec<Option<Actor>>,
    free: Vec<u32>,
    live_count: u32,
}

impl ActorTable {
    pub fn new() -> Self {
        Self {
            slots: Vec::with_capacity(64),
            free: Vec::with_capacity(64),
            live_count: 0,
        }
    }

    pub fn spawn(&mut self) -> Option<ActorId> {
        if self.live_count as usize >= MAX_ACTORS {
            return None;
        }
        let idx = if let Some(i) = self.free.pop() {
            self.slots[i as usize] = Some(Actor::new());
            i
        } else {
            let i = self.slots.len() as u32;
            self.slots.push(Some(Actor::new()));
            i
        };
        self.live_count += 1;
        Some(ActorId(idx))
    }

    pub fn despawn(&mut self, id: ActorId) {
        let i = id.0 as usize;
        if let Some(slot) = self.slots.get_mut(i) {
            if slot.is_some() {
                *slot = None;
                self.free.push(id.0);
                self.live_count = self.live_count.saturating_sub(1);
            }
        }
    }

    pub fn get(&self, id: ActorId) -> Option<&Actor> {
        self.slots.get(id.0 as usize).and_then(|s| s.as_ref())
    }

    pub fn get_mut(&mut self, id: ActorId) -> Option<&mut Actor> {
        self.slots.get_mut(id.0 as usize).and_then(|s| s.as_mut())
    }

    pub fn count(&self) -> u32 { self.live_count }

    /// Iterate all visible actors. Order is by slot index, stable across
    /// spawn/despawn for non-recycled slots.
    pub fn iter_visible(&self) -> impl Iterator<Item = &Actor> {
        self.slots.iter().filter_map(|s| s.as_ref()).filter(|a| a.visible)
    }

    /// Flush every dirty actor's SVO. Call once per frame before render.
    pub fn flush_all(&mut self) {
        for slot in self.slots.iter_mut().flatten() {
            slot.flush();
        }
    }
}

impl Default for ActorTable {
    fn default() -> Self { Self::new() }
}
