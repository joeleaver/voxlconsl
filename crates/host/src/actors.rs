//! Actors — see SPEC.md §11.
//!
//! TODO:
//!   - Actor table (§11.1) — id, prefab, volume, transform, anchor, body
//!   - Caps (§11.2) — 256 actors, 32³ volume, 4 MB resident ceiling
//!   - 24-orientation bake from prefab volume (§11.3, §11.5)
//!   - Prefab CoW sharing (§11.4)
//!   - Macro-grid binning + ray composition with world (§11.6)
//!   - Volume editing API (§11.7)
