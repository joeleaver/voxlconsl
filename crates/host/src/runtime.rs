//! Cart runtime: load `.voxl`, set up sandbox, drive the per-frame loop.
//!
//! Cross-cuts the spec — references §3, §5, §6, §7, §10, §11.
//!
//! TODO:
//!   - `.voxl` parser (§7) — header, section table, metadata
//!   - `wasmi` cart sandbox (§9)
//!   - Per-frame loop driver (§10 "per-frame loop"):
//!         poll inputs → cart.update(dt) → integrate L2 → tick L3 → cart.render() → ray-march
//!   - Save block read/write (§7, §8.3)
//!   - RNG seed + replay capture (§10.5)
