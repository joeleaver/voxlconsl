//! Actors — see SPEC.md §11.
//!
//! v0.0.5 scope:
//!   - Actor table with up to 256 actors (`actor_spawn` / `actor_despawn`).
//!   - Per-actor: position, yaw, visibility, and a volume that is either
//!     **Owned** (private dense + SVO) or **Shared** (`Rc<BakedVolume>`
//!     pulled from the prefab cache, §11.4).
//!   - `actor_spawn_from(prefab, orientation)` returns a Shared actor.
//!   - `actor_set_prefab(id, prefab)` swaps the Shared reference; transform
//!     is preserved.
//!   - Volume editing API (`actor_set_voxel`, `actor_fill_box`,
//!     `actor_clear`) **forks** a Shared actor into Owned on first edit.
//!
//! Deferred to later passes:
//!   - 24 fixed orientations + bake routine (§11.3, §11.5) — the cache key
//!     is already `(prefab, orientation)`, but the bake itself is identity
//!     for all orientations until the next milestone.
//!   - Anchor offsets — currently always volume's local origin.
//!   - Macro-grid binning (§11.6) — currently iterates all visible actors.
//!   - Body attachment (§10.2).
//!   - Volume-size mutation, `actor_load_volume`.

use std::rc::Rc;

use voxlconsl_svo::{build, ChunkData};
use voxlconsl_types::{ActorId, Orientation, PrefabId, U8Vec3, Vec3};

use crate::prefabs::{
    build_padded_chunk, matmul, matrix_transpose, orientation_matrix, rotate_dense_by_matrix,
    BakedVolume, PrefabTable,
};

/// Default per-actor volume side length for `actor_spawn`. Cart can leave
/// most voxels empty.
pub const DEFAULT_VOLUME_SIDE: u8 = 16;
const MAX_ACTORS: usize = 256;

/// An actor's volume is either privately owned (post-fork or post-`spawn`)
/// or shared via the prefab bake cache (§11.4).
pub enum ActorVolume {
    /// Privately owned dense buffer + cached SVO.
    Owned(OwnedVolume),
    /// Shared baked volume from `PrefabTable`. Multiple actors hold an
    /// `Rc` to the same `BakedVolume`; the first to mutate forks.
    Shared(Rc<BakedVolume>),
}

pub struct OwnedVolume {
    /// Dense voxel buffer, size_x × size_y × size_z bytes, row-major
    /// (x fastest, then y, then z). Material `0` is empty.
    pub dense: Vec<u8>,
    pub size: U8Vec3,
    /// Cached SVO derived from `dense`. Rebuilt by `flush()` when
    /// `dirty` is true.
    pub chunk: ChunkData,
    pub dirty: bool,
}

impl OwnedVolume {
    fn empty(side: u8) -> Self {
        let size = U8Vec3::new(side, side, side);
        let n = (side as usize).pow(3);
        Self {
            dense: vec![0; n],
            size,
            chunk: ChunkData::uniform(0),
            dirty: false,
        }
    }

    fn from_baked(baked: &BakedVolume) -> Self {
        Self {
            dense: baked.dense.clone(),
            size: baked.size,
            chunk: baked.chunk.clone(),
            dirty: false,
        }
    }

    fn voxel_index(&self, x: u8, y: u8, z: u8) -> Option<usize> {
        if x >= self.size.x || y >= self.size.y || z >= self.size.z {
            return None;
        }
        let s = self.size.x as usize;
        let sy = self.size.y as usize;
        Some(((z as usize * sy) + y as usize) * s + x as usize)
    }

    fn set_voxel(&mut self, x: u8, y: u8, z: u8, material: u8) {
        if let Some(i) = self.voxel_index(x, y, z) {
            if self.dense[i] != material {
                self.dense[i] = material;
                self.dirty = true;
            }
        }
    }

    fn fill_box(&mut self, min: U8Vec3, max: U8Vec3, material: u8) {
        let xs = min.x.min(self.size.x.saturating_sub(1));
        let ys = min.y.min(self.size.y.saturating_sub(1));
        let zs = min.z.min(self.size.z.saturating_sub(1));
        let xe = max.x.min(self.size.x.saturating_sub(1));
        let ye = max.y.min(self.size.y.saturating_sub(1));
        let ze = max.z.min(self.size.z.saturating_sub(1));
        for z in zs..=ze {
            for y in ys..=ye {
                for x in xs..=xe {
                    let i = self.voxel_index(x, y, z).unwrap();
                    self.dense[i] = material;
                }
            }
        }
        self.dirty = true;
    }

    fn clear(&mut self) {
        self.dense.fill(0);
        self.dirty = true;
    }

    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let pad_side = build::CHUNK_SIZE;
        let mut padded = vec![0u8; (pad_side * pad_side * pad_side) as usize];
        let sx = self.size.x as u32;
        let sy = self.size.y as u32;
        let sz = self.size.z as u32;
        for z in 0..sz {
            for y in 0..sy {
                for x in 0..sx {
                    let src = ((z * sy + y) * sx + x) as usize;
                    let dst = ((z * pad_side + y) * pad_side + x) as usize;
                    padded[dst] = self.dense[src];
                }
            }
        }
        self.chunk = build::from_dense(&padded);
        self.dirty = false;
    }

    fn get_voxel(&self, x: u8, y: u8, z: u8) -> u8 {
        self.voxel_index(x, y, z)
            .map(|i| self.dense[i])
            .unwrap_or(0)
    }
}

pub struct Actor {
    pub position: Vec3,
    pub yaw: f32,
    pub visible: bool,
    pub volume: ActorVolume,
    /// Tracked for `actor_get_orientation` and as the cache key for
    /// future re-baking. v0.0.5 always Up.
    pub orientation: Orientation,
    /// Set on actors spawned via `actor_spawn_from`; used when `actor_set_prefab`
    /// or `actor_set_orientation` need to look up a baked volume.
    pub prefab: Option<PrefabId>,
}

impl Actor {
    fn new_owned_default() -> Self {
        Self {
            position: Vec3::ZERO,
            yaw: 0.0,
            visible: true,
            volume: ActorVolume::Owned(OwnedVolume::empty(DEFAULT_VOLUME_SIDE)),
            orientation: Orientation::Up,
            prefab: None,
        }
    }

    fn new_shared(prefab: PrefabId, orientation: Orientation, baked: Rc<BakedVolume>) -> Self {
        Self {
            position: Vec3::ZERO,
            yaw: 0.0,
            visible: true,
            volume: ActorVolume::Shared(baked),
            orientation,
            prefab: Some(prefab),
        }
    }

    /// Renderer-facing accessor for the SVO to raycast against. For
    /// Owned actors callers must `flush()` first; for Shared actors the
    /// SVO is always up-to-date because the bake is immutable.
    pub fn chunk(&self) -> &ChunkData {
        match &self.volume {
            ActorVolume::Owned(o) => &o.chunk,
            ActorVolume::Shared(rc) => &rc.chunk,
        }
    }

    pub fn volume_size(&self) -> U8Vec3 {
        match &self.volume {
            ActorVolume::Owned(o) => o.size,
            ActorVolume::Shared(rc) => rc.size,
        }
    }

    pub fn get_voxel(&self, x: u8, y: u8, z: u8) -> u8 {
        match &self.volume {
            ActorVolume::Owned(o) => o.get_voxel(x, y, z),
            ActorVolume::Shared(rc) => {
                if x >= rc.size.x || y >= rc.size.y || z >= rc.size.z {
                    return 0;
                }
                let s = rc.size.x as usize;
                let sy = rc.size.y as usize;
                let i = ((z as usize * sy) + y as usize) * s + x as usize;
                rc.dense[i]
            }
        }
    }

    /// Force the actor's volume into the Owned form, cloning the shared
    /// `BakedVolume` if needed. After this call further mutations are
    /// in-place and don't affect any other actors.
    fn fork_for_mutation(&mut self) -> &mut OwnedVolume {
        if let ActorVolume::Shared(_) = &self.volume {
            let owned = match &self.volume {
                ActorVolume::Shared(rc) => OwnedVolume::from_baked(rc),
                _ => unreachable!(),
            };
            self.volume = ActorVolume::Owned(owned);
            // A fork detaches from the prefab table — subsequent mutations
            // belong to this actor alone. The prefab id is cleared so a
            // later `actor_set_prefab` doesn't think the new owned data
            // came from a particular prefab.
            self.prefab = None;
        }
        match &mut self.volume {
            ActorVolume::Owned(o) => o,
            _ => unreachable!(),
        }
    }

    pub fn set_voxel(&mut self, x: u8, y: u8, z: u8, material: u8) {
        self.fork_for_mutation().set_voxel(x, y, z, material);
    }

    pub fn fill_box(&mut self, min: U8Vec3, max: U8Vec3, material: u8) {
        self.fork_for_mutation().fill_box(min, max, material);
    }

    pub fn clear(&mut self) {
        self.fork_for_mutation().clear();
    }

    /// Swap to a different baked volume from the prefab cache. The actor's
    /// transform (position, yaw, orientation, visibility) is preserved.
    /// Forks-on-write: if the actor was Owned, the owned buffer is dropped
    /// and the actor becomes Shared again.
    pub fn set_prefab(&mut self, prefab: PrefabId, baked: Rc<BakedVolume>) {
        self.volume = ActorVolume::Shared(baked);
        self.prefab = Some(prefab);
        // Don't touch orientation — `actor_set_prefab` keeps the actor's
        // current pose. The bake the caller passed in is for that
        // orientation.
    }

    /// Re-orient the actor (§11.5).
    ///
    /// Shared + has prefab: look up `(prefab, new)` in the bake cache and
    /// Rc-swap. No allocation in the cart's RAM budget.
    ///
    /// Owned (post-fork or post-`actor_spawn`): rotate the owned dense
    /// by the *delta* `R_new · R_current⁻¹`, rebuild SVO eagerly. The
    /// delta is itself a signed permutation, so the cost is one O(N)
    /// pass over the actor's voxels.
    pub fn set_orientation(&mut self, new_ori: Orientation, prefabs: &mut PrefabTable) {
        if self.orientation == new_ori {
            return;
        }
        match (&self.volume, self.prefab) {
            (ActorVolume::Shared(_), Some(prefab_id)) => {
                let Some(baked) = prefabs.bake(prefab_id, new_ori) else { return };
                self.volume = ActorVolume::Shared(baked);
            }
            (ActorVolume::Owned(_), _) => {
                let r_new = orientation_matrix(new_ori);
                let r_current = orientation_matrix(self.orientation);
                let r_delta = matmul(r_new, matrix_transpose(r_current));

                // Borrow-juggle: pull dense out, build new, swap in.
                let (new_dense, new_size) = match &self.volume {
                    ActorVolume::Owned(o) => rotate_dense_by_matrix(&o.dense, o.size, r_delta),
                    _ => unreachable!(),
                };
                let chunk = build_padded_chunk(&new_dense, [
                    new_size.x as usize,
                    new_size.y as usize,
                    new_size.z as usize,
                ]);
                self.volume = ActorVolume::Owned(OwnedVolume {
                    dense: new_dense,
                    size: new_size,
                    chunk,
                    dirty: false,
                });
            }
            // Shared without prefab — should not occur with current paths
            // (`actor_spawn_from` always sets `prefab`); leave a no-op.
            _ => {}
        }
        self.orientation = new_ori;
    }

    /// Flush a dirty Owned actor's SVO. Shared actors are no-ops since
    /// their SVO is immutable.
    pub fn flush(&mut self) {
        if let ActorVolume::Owned(o) = &mut self.volume {
            o.flush();
        }
    }

    /// World-space AABB of the actor's volume after yaw rotation.
    /// Position is the world location of the volume's local (0, 0, 0)
    /// corner; yaw rotates around that corner about the world Y axis.
    pub fn world_aabb(&self) -> (Vec3, Vec3) {
        let s = self.volume_size();
        let (sx, sy, sz) = (s.x as f32, s.y as f32, s.z as f32);
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

    /// True iff this actor currently shares its volume with the prefab
    /// table. Tests / debugging use this to verify CoW behavior.
    pub fn is_shared(&self) -> bool {
        matches!(self.volume, ActorVolume::Shared(_))
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

    /// Spawn an empty default-sized owned actor.
    pub fn spawn(&mut self) -> Option<ActorId> {
        self.insert(Actor::new_owned_default())
    }

    /// Spawn an actor instancing a prefab (§11.4).
    /// Returns `None` if the prefab id is unknown or the actor cap is hit.
    pub fn spawn_from(
        &mut self,
        prefab_id: PrefabId,
        orientation: Orientation,
        prefabs: &mut PrefabTable,
    ) -> Option<ActorId> {
        let baked = prefabs.bake(prefab_id, orientation)?;
        self.insert(Actor::new_shared(prefab_id, orientation, baked))
    }

    fn insert(&mut self, actor: Actor) -> Option<ActorId> {
        if self.live_count as usize >= MAX_ACTORS {
            return None;
        }
        let idx = if let Some(i) = self.free.pop() {
            self.slots[i as usize] = Some(actor);
            i
        } else {
            let i = self.slots.len() as u32;
            self.slots.push(Some(actor));
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

    /// Flush every dirty Owned actor's SVO. Call once per frame before render.
    /// Shared actors are no-ops (their SVO is immutable).
    pub fn flush_all(&mut self) {
        for slot in self.slots.iter_mut().flatten() {
            slot.flush();
        }
    }

    /// Swap an actor's prefab. Requires a `&mut PrefabTable` to look up
    /// the bake. No-op if the actor id or prefab id is unknown.
    pub fn set_actor_prefab(
        &mut self,
        id: ActorId,
        prefab: PrefabId,
        prefabs: &mut PrefabTable,
    ) {
        let orientation = match self.get(id) {
            Some(a) => a.orientation,
            None => return,
        };
        let Some(baked) = prefabs.bake(prefab, orientation) else { return };
        if let Some(actor) = self.get_mut(id) {
            actor.set_prefab(prefab, baked);
        }
    }

    /// Re-orient an actor (§11.5). No-op if id is unknown.
    pub fn set_actor_orientation(
        &mut self,
        id: ActorId,
        ori: Orientation,
        prefabs: &mut PrefabTable,
    ) {
        if let Some(actor) = self.get_mut(id) {
            actor.set_orientation(ori, prefabs);
        }
    }
}

impl Default for ActorTable {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dense_filled(size: U8Vec3, fill: u8) -> Vec<u8> {
        vec![fill; (size.x as usize) * (size.y as usize) * (size.z as usize)]
    }

    #[test]
    fn spawn_from_shares_baked_volume() {
        let mut table = ActorTable::new();
        let mut prefabs = PrefabTable::new();
        let size = U8Vec3::new(4, 4, 4);
        prefabs.define(PrefabId(1), dense_filled(size, 7), size);

        let a = table.spawn_from(PrefabId(1), Orientation::Up, &mut prefabs).unwrap();
        let b = table.spawn_from(PrefabId(1), Orientation::Up, &mut prefabs).unwrap();

        // Both actors should be Shared and reference the same Rc.
        assert!(table.get(a).unwrap().is_shared());
        assert!(table.get(b).unwrap().is_shared());

        // Pull out the two Rcs and confirm pointer equality.
        let (a_rc, b_rc) = match (&table.get(a).unwrap().volume, &table.get(b).unwrap().volume) {
            (ActorVolume::Shared(ra), ActorVolume::Shared(rb)) => (Rc::clone(ra), Rc::clone(rb)),
            _ => panic!("expected Shared volumes"),
        };
        assert!(Rc::ptr_eq(&a_rc, &b_rc));
    }

    #[test]
    fn first_mutation_forks_only_the_mutated_actor() {
        let mut table = ActorTable::new();
        let mut prefabs = PrefabTable::new();
        let size = U8Vec3::new(4, 4, 4);
        prefabs.define(PrefabId(1), dense_filled(size, 7), size);

        let a = table.spawn_from(PrefabId(1), Orientation::Up, &mut prefabs).unwrap();
        let b = table.spawn_from(PrefabId(1), Orientation::Up, &mut prefabs).unwrap();

        // Mutate `a`. It should fork to Owned; `b` stays Shared.
        table.get_mut(a).unwrap().set_voxel(0, 0, 0, 9);
        assert!(!table.get(a).unwrap().is_shared());
        assert!(table.get(b).unwrap().is_shared());

        // The forked `a` actually has the new voxel; `b` doesn't.
        assert_eq!(table.get(a).unwrap().get_voxel(0, 0, 0), 9);
        assert_eq!(table.get(b).unwrap().get_voxel(0, 0, 0), 7);
    }

    #[test]
    fn set_prefab_swaps_reference_without_forking_other_actors() {
        let mut table = ActorTable::new();
        let mut prefabs = PrefabTable::new();
        let size = U8Vec3::new(4, 4, 4);
        prefabs.define(PrefabId(1), dense_filled(size, 7), size);
        prefabs.define(PrefabId(2), dense_filled(size, 8), size);

        let a = table.spawn_from(PrefabId(1), Orientation::Up, &mut prefabs).unwrap();
        let b = table.spawn_from(PrefabId(1), Orientation::Up, &mut prefabs).unwrap();

        // Swap `a` to prefab 2.
        table.set_actor_prefab(a, PrefabId(2), &mut prefabs);

        // `a` is still Shared, but to a different bake. `b` is unchanged.
        assert!(table.get(a).unwrap().is_shared());
        assert_eq!(table.get(a).unwrap().get_voxel(0, 0, 0), 8);
        assert_eq!(table.get(b).unwrap().get_voxel(0, 0, 0), 7);
    }

    #[test]
    fn spawn_from_unknown_prefab_returns_none() {
        let mut table = ActorTable::new();
        let mut prefabs = PrefabTable::new();
        assert!(table.spawn_from(PrefabId(99), Orientation::Up, &mut prefabs).is_none());
    }

    #[test]
    fn owned_actor_supports_set_voxel_and_fill_box() {
        // Regression coverage for the path that didn't change semantics.
        let mut table = ActorTable::new();
        let id = table.spawn().unwrap();
        let a = table.get_mut(id).unwrap();
        a.set_voxel(0, 0, 0, 5);
        a.fill_box(U8Vec3::new(1, 0, 0), U8Vec3::new(2, 0, 0), 6);
        a.flush();
        assert_eq!(a.get_voxel(0, 0, 0), 5);
        assert_eq!(a.get_voxel(1, 0, 0), 6);
        assert_eq!(a.get_voxel(2, 0, 0), 6);
    }

    #[test]
    fn set_orientation_on_shared_actor_uses_bake_cache() {
        let mut table = ActorTable::new();
        let mut prefabs = PrefabTable::new();
        let size = U8Vec3::new(2, 2, 2);
        prefabs.define(PrefabId(1), dense_filled(size, 7), size);

        let a = table.spawn_from(PrefabId(1), Orientation::Up, &mut prefabs).unwrap();
        let b = table.spawn_from(PrefabId(1), Orientation::Up, &mut prefabs).unwrap();
        // Re-orient `a` to UpRot90; `b` keeps Up.
        table.set_actor_orientation(a, Orientation::UpRot90, &mut prefabs);

        // Both still Shared; `a` now references a different bake.
        assert!(table.get(a).unwrap().is_shared());
        assert!(table.get(b).unwrap().is_shared());
        assert_eq!(table.get(a).unwrap().orientation, Orientation::UpRot90);
        assert_eq!(table.get(b).unwrap().orientation, Orientation::Up);

        // Spawning a third actor at UpRot90 should reuse `a`'s bake.
        let c = table.spawn_from(PrefabId(1), Orientation::UpRot90, &mut prefabs).unwrap();
        let (a_rc, c_rc) = match (&table.get(a).unwrap().volume, &table.get(c).unwrap().volume) {
            (ActorVolume::Shared(ra), ActorVolume::Shared(rc)) => (Rc::clone(ra), Rc::clone(rc)),
            _ => panic!("expected Shared"),
        };
        assert!(Rc::ptr_eq(&a_rc, &c_rc));
    }

    #[test]
    fn set_orientation_on_owned_actor_rotates_dense() {
        // Spawn an owned actor (default 16³, all-air), set one voxel,
        // then re-orient. The voxel should follow the rotation.
        let mut table = ActorTable::new();
        let mut prefabs = PrefabTable::new();
        let id = table.spawn().unwrap();
        let a = table.get_mut(id).unwrap();
        a.set_voxel(0, 0, 0, 5);
        a.set_voxel(15, 0, 0, 9);
        a.flush();
        // After UpRot180 (flips X and Z): voxel (0,0,0) → (15, 0, 15);
        // voxel (15, 0, 0) → (0, 0, 15).
        table.set_actor_orientation(id, Orientation::UpRot180, &mut prefabs);
        let a = table.get(id).unwrap();
        assert!(!a.is_shared(), "owned actor should stay owned");
        assert_eq!(a.orientation, Orientation::UpRot180);
        assert_eq!(a.get_voxel(15, 0, 15), 5);
        assert_eq!(a.get_voxel(0, 0, 15), 9);
        // Original locations are now empty.
        assert_eq!(a.get_voxel(0, 0, 0), 0);
        assert_eq!(a.get_voxel(15, 0, 0), 0);
    }

    #[test]
    fn owned_orientation_composes_correctly_across_two_calls() {
        // UpRot90 followed by UpRot90 should equal UpRot180.
        let mut table = ActorTable::new();
        let mut prefabs = PrefabTable::new();
        let id = table.spawn().unwrap();
        let a = table.get_mut(id).unwrap();
        a.set_voxel(0, 0, 0, 5);
        a.flush();

        table.set_actor_orientation(id, Orientation::UpRot90, &mut prefabs);
        table.set_actor_orientation(id, Orientation::UpRot180, &mut prefabs);

        // Reference path: a fresh actor with a single set_voxel + UpRot180
        // applied directly should match.
        let mut tbl2 = ActorTable::new();
        let mut prefabs2 = PrefabTable::new();
        let ref_id = tbl2.spawn().unwrap();
        tbl2.get_mut(ref_id).unwrap().set_voxel(0, 0, 0, 5);
        tbl2.get_mut(ref_id).unwrap().flush();
        tbl2.set_actor_orientation(ref_id, Orientation::UpRot180, &mut prefabs2);

        let got = match &table.get(id).unwrap().volume {
            ActorVolume::Owned(o) => o.dense.clone(),
            _ => panic!(),
        };
        let want = match &tbl2.get(ref_id).unwrap().volume {
            ActorVolume::Owned(o) => o.dense.clone(),
            _ => panic!(),
        };
        assert_eq!(got, want);
    }
}
