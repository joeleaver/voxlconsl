//! voxlconsl SDK — what cart authors depend on.
//!
//! Re-exports the shared types from `voxlconsl-types` and provides safe Rust
//! wrappers over the host's WASM imports (§8 of SPEC.md).
//!
//! v0.0.3: a minimal surface — world mutation, materials, camera, lighting,
//! sky. Audio, physics, actors, input, etc. are TODO.

#![no_std]

pub use voxlconsl_types::*;

/// Raw `extern "C"` host imports the cart links against. The host (running
/// the cart in `wasmi`) provides these.
///
/// All multi-component values are passed as a flat list of primitives —
/// WASM's ABI doesn't natively carry tuples or structs (§8.4).
mod host {
    use super::{Hit, SweepHit};
    unsafe extern "C" {
        pub fn set_voxel(x: u32, y: u32, z: u32, material: u32);
        pub fn fill_box(
            min_x: u32, min_y: u32, min_z: u32,
            max_x: u32, max_y: u32, max_z: u32,
            material: u32,
        );
        pub fn clear_world();

        pub fn scene_set_active(id: u32);
        pub fn scene_get_active() -> u32;

        pub fn material_define(slot: u32, color: u32, emission: u32, flags: u32);
        pub fn material_set_ca(
            slot: u32,
            threshold: u32,
            lifetime: u32,
            viscosity: u32,
            ignites_to: u32,
        );

        pub fn camera_set_lookat(
            ex: f32, ey: f32, ez: f32,
            tx: f32, ty: f32, tz: f32,
            ux: f32, uy: f32, uz: f32,
        );
        pub fn camera_set_fov(fov_y_deg: f32);

        pub fn light_set_sun(dx: f32, dy: f32, dz: f32, color: u32, intensity: u32);
        pub fn sky_set_gradient(top: u32, horizon: u32);

        pub fn actor_spawn() -> u32;
        pub fn actor_spawn_from(prefab_id: u32, orientation: u32) -> u32;
        pub fn actor_despawn(actor_id: u32);
        pub fn actor_count() -> u32;
        pub fn actor_set_position(actor_id: u32, x: f32, y: f32, z: f32);
        pub fn actor_get_position(actor_id: u32, out_x: *mut f32, out_y: *mut f32, out_z: *mut f32);
        pub fn actor_set_yaw(actor_id: u32, yaw: f32);
        pub fn actor_get_yaw(actor_id: u32) -> f32;
        pub fn actor_set_visible(actor_id: u32, visible: u32);
        pub fn actor_set_voxel(actor_id: u32, x: u32, y: u32, z: u32, material: u32);
        pub fn actor_fill_box(
            actor_id: u32,
            min_x: u32, min_y: u32, min_z: u32,
            max_x: u32, max_y: u32, max_z: u32,
            material: u32,
        );
        pub fn actor_clear(actor_id: u32);
        pub fn actor_set_prefab(actor_id: u32, prefab_id: u32);
        pub fn actor_set_orientation(actor_id: u32, orientation: u32);
        pub fn actor_get_orientation(actor_id: u32) -> u32;

        pub fn prefab_define(
            prefab_id: u32,
            ptr: *const u8, len: u32,
            sx: u32, sy: u32, sz: u32,
        );

        pub fn input_declare_action(kind: u32, hint: u32, name_ptr: *const u8, name_len: u32) -> u32;
        pub fn input_action_button(h: u32) -> u32;
        pub fn input_action_pressed(h: u32) -> u32;
        pub fn input_action_released(h: u32) -> u32;
        pub fn input_action_held_ms(h: u32) -> u32;
        pub fn input_action_axis1d(h: u32) -> f32;
        pub fn input_action_axis2d(h: u32, out_x: *mut f32, out_y: *mut f32);
        pub fn input_action_active(h: u32) -> u32;

        pub fn log(ptr: *const u8, len: u32);

        // Physics queries (§10.1)
        pub fn raycast(
            ox: f32, oy: f32, oz: f32,
            dx: f32, dy: f32, dz: f32,
            max_dist: f32,
            out_hit: *mut Hit,
        ) -> u32;
        pub fn raycast_world_only(
            ox: f32, oy: f32, oz: f32,
            dx: f32, dy: f32, dz: f32,
            max_dist: f32,
            out_hit: *mut Hit,
        ) -> u32;
        pub fn aabb_overlap_world(
            min_x: f32, min_y: f32, min_z: f32,
            max_x: f32, max_y: f32, max_z: f32,
        ) -> u32;
        pub fn aabb_overlap_actors(
            min_x: f32, min_y: f32, min_z: f32,
            max_x: f32, max_y: f32, max_z: f32,
        ) -> u64;
        pub fn sweep_aabb(
            min_x: f32, min_y: f32, min_z: f32,
            max_x: f32, max_y: f32, max_z: f32,
            mx: f32, my: f32, mz: f32,
            out_hit: *mut SweepHit,
        ) -> u32;
        pub fn material_at(x: u32, y: u32, z: u32) -> u32;

        // CA (§10.3)
        pub fn ca_set_budget(voxels_per_frame: u32);
        pub fn ca_get_budget() -> u32;
        pub fn ca_mark_active(x: u32, y: u32, z: u32);
        pub fn ca_active_count() -> u32;
        pub fn ca_set_global_param(param: u32, value: f32);
    }
}

// ============================================================================
// Safe Rust wrappers — cart authors call these.
// ============================================================================

/// Set a single world voxel. See SPEC.md §3.6.
pub fn set_voxel(pos: UVec3, material: u8) {
    unsafe { host::set_voxel(pos.x, pos.y, pos.z, material as u32) }
}

/// Fill an axis-aligned box of world voxels with a material. Inclusive
/// on both ends.
pub fn fill_box(min: UVec3, max: UVec3, material: u8) {
    unsafe {
        host::fill_box(min.x, min.y, min.z, max.x, max.y, max.z, material as u32)
    }
}

/// Reset the **active scene** to all-air. Other scenes are unaffected.
/// See SPEC.md §3.6.
pub fn clear_world() {
    unsafe { host::clear_world() }
}

/// Switch the active scene. All subsequent voxel reads, writes, and
/// rendering target the scene with this id. Carts may address up to
/// 256 scenes; an unallocated scene reads as uniform air and lazy-
/// allocates on first write. Materials, prefabs, actors, and audio
/// state are cart-global and survive scene switches — see SPEC.md
/// §3.6 for the full carry-over semantics.
pub fn scene_set_active(scene: SceneId) {
    unsafe { host::scene_set_active(scene.0 as u32) }
}

pub fn scene_get_active() -> SceneId {
    let v = unsafe { host::scene_get_active() };
    SceneId(v as u8)
}

/// Define one material in the cart's material table.
///
/// `color` is the packed `(ramp << 2) | shade` byte (use [`Material::pack_color`]).
/// `emission` is 0..=15. `flags` is the [`MaterialFlags`] bitfield.
pub fn material_define(slot: u8, color: u8, emission: u8, flags: MaterialFlags) {
    unsafe {
        host::material_define(slot as u32, color as u32, emission as u32, flags.0 as u32)
    }
}

/// Set the §10.3 cellular-automata tuning fields on an
/// already-defined material:
/// - `threshold`: granular angle-of-repose / flammable ignition heat
///   (0 = use platform default).
/// - `lifetime`: gas lifetime in CA ticks / fire frames before
///   burning out (0 = use platform default; fire caps at 15).
/// - `viscosity`: liquid flow rate (0 = use platform default).
/// - `ignites_to`: for flammable, the material slot this cell becomes
///   when its heat exceeds `threshold` — typically the cart's fire
///   material. 0 = vanish to air.
pub fn material_set_ca(
    slot: u8,
    threshold: u8,
    lifetime: u8,
    viscosity: u8,
    ignites_to: u8,
) {
    unsafe {
        host::material_set_ca(
            slot as u32,
            threshold as u32,
            lifetime as u32,
            viscosity as u32,
            ignites_to as u32,
        )
    }
}

/// Set the camera using look-at semantics. See SPEC.md §3.2.
pub fn camera_set_lookat(eye: Vec3, target: Vec3, up: Vec3) {
    unsafe {
        host::camera_set_lookat(
            eye.x, eye.y, eye.z,
            target.x, target.y, target.z,
            up.x, up.y, up.z,
        )
    }
}

/// Set vertical FOV (degrees) when using `Projection::Perspective`.
pub fn camera_set_fov(fov_y_deg: f32) {
    unsafe { host::camera_set_fov(fov_y_deg) }
}

/// Set the directional sun. `direction` should point *toward* the sun.
pub fn light_set_sun(direction: Vec3, color: u8, intensity: u8) {
    unsafe {
        host::light_set_sun(direction.x, direction.y, direction.z, color as u32, intensity as u32)
    }
}

/// Set the sky gradient (top and horizon palette indices).
pub fn sky_set_gradient(top: u8, horizon: u8) {
    unsafe { host::sky_set_gradient(top as u32, horizon as u32) }
}

/// Emit a debug log line to the host's console (no-op in release on
/// hardware ports without a serial console).
pub fn log(msg: &str) {
    unsafe { host::log(msg.as_ptr(), msg.len() as u32) }
}

// ============================================================================
// Actors — see SPEC.md §11.
// ============================================================================

/// Spawn a new actor. Returns `None` when the per-cart actor cap is hit.
pub fn actor_spawn() -> Option<ActorId> {
    let id = unsafe { host::actor_spawn() };
    if id == u32::MAX { None } else { Some(ActorId(id)) }
}

/// Spawn an actor instancing a prefab (§11.4). Returns `None` when the
/// prefab id is unknown or the per-cart actor cap is hit. Multiple actors
/// instancing the same `(prefab, orientation)` share one baked volume via
/// copy-on-write — see SPEC.md §11.4.
pub fn actor_spawn_from(prefab: PrefabId, orientation: Orientation) -> Option<ActorId> {
    let id = unsafe { host::actor_spawn_from(prefab.0 as u32, orientation as u32) };
    if id == u32::MAX { None } else { Some(ActorId(id)) }
}

/// Register a prefab volume with the host.
///
/// `dense` is row-major (x fastest, then y, then z), `size.x * size.y *
/// size.z` bytes. Material `0` is air. The host copies the buffer into
/// its own prefab table; the cart can drop or reuse `dense` after this
/// call returns.
///
/// v0.0.5 is a runtime API; once the §7 cart format lands prefab data
/// will load from the cart's World section before `init` runs and this
/// call becomes optional.
pub fn prefab_define(prefab: PrefabId, dense: &[u8], size: U8Vec3) {
    unsafe {
        host::prefab_define(
            prefab.0 as u32,
            dense.as_ptr(), dense.len() as u32,
            size.x as u32, size.y as u32, size.z as u32,
        )
    }
}

pub fn actor_despawn(actor: ActorId) {
    unsafe { host::actor_despawn(actor.0) }
}

pub fn actor_count() -> u32 {
    unsafe { host::actor_count() }
}

pub fn actor_set_position(actor: ActorId, pos: Vec3) {
    unsafe { host::actor_set_position(actor.0, pos.x, pos.y, pos.z) }
}

pub fn actor_get_position(actor: ActorId) -> Vec3 {
    let mut x: f32 = 0.0;
    let mut y: f32 = 0.0;
    let mut z: f32 = 0.0;
    unsafe { host::actor_get_position(actor.0, &mut x, &mut y, &mut z) };
    Vec3::new(x, y, z)
}

pub fn actor_set_yaw(actor: ActorId, yaw: f32) {
    unsafe { host::actor_set_yaw(actor.0, yaw) }
}

pub fn actor_get_yaw(actor: ActorId) -> f32 {
    unsafe { host::actor_get_yaw(actor.0) }
}

pub fn actor_set_visible(actor: ActorId, visible: bool) {
    unsafe { host::actor_set_visible(actor.0, visible as u32) }
}

pub fn actor_set_voxel(actor: ActorId, pos: U8Vec3, material: u8) {
    unsafe {
        host::actor_set_voxel(
            actor.0,
            pos.x as u32, pos.y as u32, pos.z as u32,
            material as u32,
        )
    }
}

pub fn actor_fill_box(actor: ActorId, min: U8Vec3, max: U8Vec3, material: u8) {
    unsafe {
        host::actor_fill_box(
            actor.0,
            min.x as u32, min.y as u32, min.z as u32,
            max.x as u32, max.y as u32, max.z as u32,
            material as u32,
        )
    }
}

pub fn actor_clear(actor: ActorId) {
    unsafe { host::actor_clear(actor.0) }
}

/// Swap an actor's prefab. The basis of flipbook animation (§11.9).
///
/// The actor's transform (position, yaw, orientation, anchor, visibility)
/// is preserved; only the volume reference changes. The host shares baked
/// volumes between actors instancing the same `(prefab, orientation)`
/// pair via copy-on-write, so prefab swaps are pointer-cheap.
pub fn actor_set_prefab(actor: ActorId, prefab: PrefabId) {
    unsafe { host::actor_set_prefab(actor.0, prefab.0 as u32) }
}

/// Re-orient an actor (§11.5). For prefab-shared actors this is a
/// pointer swap into the bake cache; for owned actors the host rotates
/// the dense buffer and rebuilds the SVO. Setting the same orientation
/// is a no-op.
pub fn actor_set_orientation(actor: ActorId, orientation: Orientation) {
    unsafe { host::actor_set_orientation(actor.0, orientation as u32) }
}

pub fn actor_get_orientation(actor: ActorId) -> Orientation {
    let v = unsafe { host::actor_get_orientation(actor.0) };
    match v {
        0 => Orientation::Up,
        1 => Orientation::UpRot90,
        2 => Orientation::UpRot180,
        3 => Orientation::UpRot270,
        4 => Orientation::Down,
        5 => Orientation::DownRot90,
        6 => Orientation::DownRot180,
        7 => Orientation::DownRot270,
        8 => Orientation::EastUp,
        9 => Orientation::EastUpRot90,
        10 => Orientation::EastUpRot180,
        11 => Orientation::EastUpRot270,
        12 => Orientation::WestUp,
        13 => Orientation::WestUpRot90,
        14 => Orientation::WestUpRot180,
        15 => Orientation::WestUpRot270,
        16 => Orientation::NorthUp,
        17 => Orientation::NorthUpRot90,
        18 => Orientation::NorthUpRot180,
        19 => Orientation::NorthUpRot270,
        20 => Orientation::SouthUp,
        21 => Orientation::SouthUpRot90,
        22 => Orientation::SouthUpRot180,
        23 => Orientation::SouthUpRot270,
        _ => Orientation::Up,
    }
}

pub mod animation;
pub mod ca;
pub mod physics;
pub mod text;

// ============================================================================
// Input — see SPEC.md §6.
// ============================================================================

/// Declare an action. Cart calls this once per action during `init` and
/// stores the returned handle for use during `update`/`render`.
pub fn input_declare_action(kind: ActionKind, hint: BindingHint, name: &str) -> ActionHandle {
    let raw = unsafe {
        host::input_declare_action(
            kind as u32,
            hint as u32,
            name.as_ptr(),
            name.len() as u32,
        )
    };
    ActionHandle(raw)
}

/// True if the action is currently held this frame (Button-kind only).
pub fn input_action_button(h: ActionHandle) -> bool {
    unsafe { host::input_action_button(h.0) != 0 }
}

/// Edge: action transitioned to held this frame.
pub fn input_action_pressed(h: ActionHandle) -> bool {
    unsafe { host::input_action_pressed(h.0) != 0 }
}

/// Edge: action transitioned to released this frame.
pub fn input_action_released(h: ActionHandle) -> bool {
    unsafe { host::input_action_released(h.0) != 0 }
}

/// Milliseconds this Button-kind action has been held; 0 when not held.
pub fn input_action_held_ms(h: ActionHandle) -> u32 {
    unsafe { host::input_action_held_ms(h.0) }
}

/// Axis1D action value, range -1..1 (signed) or 0..1 (unsigned by binding).
pub fn input_action_axis1d(h: ActionHandle) -> f32 {
    unsafe { host::input_action_axis1d(h.0) }
}

/// Axis2D action value (e.g., a stick or aim delta).
pub fn input_action_axis2d(h: ActionHandle) -> (f32, f32) {
    let mut x: f32 = 0.0;
    let mut y: f32 = 0.0;
    unsafe { host::input_action_axis2d(h.0, &mut x, &mut y) };
    (x, y)
}

/// True iff the action is bound to anything on the current port. Lets carts
/// gracefully omit features (e.g., pointer-only UI) on stick-only devices.
pub fn input_action_active(h: ActionHandle) -> bool {
    unsafe { host::input_action_active(h.0) != 0 }
}
