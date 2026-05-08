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
    unsafe extern "C" {
        pub fn set_voxel(x: u32, y: u32, z: u32, material: u32);
        pub fn fill_box(
            min_x: u32, min_y: u32, min_z: u32,
            max_x: u32, max_y: u32, max_z: u32,
            material: u32,
        );
        pub fn clear_world();

        pub fn material_define(slot: u32, color: u32, emission: u32, flags: u32);

        pub fn camera_set_lookat(
            ex: f32, ey: f32, ez: f32,
            tx: f32, ty: f32, tz: f32,
            ux: f32, uy: f32, uz: f32,
        );
        pub fn camera_set_fov(fov_y_deg: f32);

        pub fn light_set_sun(dx: f32, dy: f32, dz: f32, color: u32, intensity: u32);
        pub fn sky_set_gradient(top: u32, horizon: u32);

        pub fn input_declare_action(kind: u32, hint: u32, name_ptr: *const u8, name_len: u32) -> u32;
        pub fn input_action_button(h: u32) -> u32;
        pub fn input_action_pressed(h: u32) -> u32;
        pub fn input_action_released(h: u32) -> u32;
        pub fn input_action_held_ms(h: u32) -> u32;
        pub fn input_action_axis1d(h: u32) -> f32;
        pub fn input_action_axis2d(h: u32, out_x: *mut f32, out_y: *mut f32);
        pub fn input_action_active(h: u32) -> u32;

        pub fn log(ptr: *const u8, len: u32);
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

/// Reset the world to all-air. See SPEC.md §3.6.
pub fn clear_world() {
    unsafe { host::clear_world() }
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
