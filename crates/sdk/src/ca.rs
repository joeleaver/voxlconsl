//! Cellular automata API — see SPEC.md §10.3.
//!
//! Carts opt into CA behavior via material flags ([`MaterialFlags::GRANULAR`],
//! `LIQUID`, `GAS`, `FLAMMABLE`, `FIRE`). v0.1.x implements **granular**
//! fully; other flags are reserved and dispatch to no-op stubs. Marking
//! a voxel active is automatic when the cart writes a CA-flagged material
//! via `set_voxel` / `fill_box`; `mark_active` is provided for the
//! occasional case where the cart needs to wake a cell explicitly.

use voxlconsl_types::{CaParam, UVec3};

use crate::host;

/// Set the per-frame voxel-drain budget. `0` disables CA entirely.
/// Defaults to the per-port reference cap from §10.3 (browser: 32,768).
pub fn set_budget(voxels_per_frame: u32) {
    unsafe { host::ca_set_budget(voxels_per_frame) }
}

/// Current per-frame budget.
pub fn get_budget() -> u32 {
    unsafe { host::ca_get_budget() }
}

/// Wake `pos` and its 6-axis neighbors. Useful when the cart synthesizes
/// a state change the host can't infer from `set_voxel` alone.
pub fn mark_active(pos: UVec3) {
    unsafe { host::ca_mark_active(pos.x, pos.y, pos.z) }
}

/// Number of voxels currently in the active set. Telemetry / debug.
pub fn active_count() -> u32 {
    unsafe { host::ca_active_count() }
}

/// Set a globally-tunable CA parameter. Reserved in v0.1.x — no params
/// are read by the simulator yet.
pub fn set_global_param(param: CaParam, value: f32) {
    unsafe { host::ca_set_global_param(param as u32, value) }
}
