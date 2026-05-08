//! Bundler: turns a cart project directory into a `.voxl` cart.
//!
//! TODO:
//!   - `cart.toml` parser (§12.6.1) — manifest + paths
//!   - `materials.toml` parser (§12.6.2) → 256-entry binary table (§2)
//!   - `patches.toml` parser (§12.6.3) → patch blobs (§5.1)
//!   - `colors.toml` parser (§12.6.4) for `.vox` imports
//!   - `.vxv` reader (§12.2) — all 4 encodings
//!   - `.vox` importer (§12.3) — palette mapping + Y-up coord flip
//!   - World composer: `.vxv` chunks at world positions → SVO chunk index (§13.6)
//!   - WASM build invocation (or direct path)
//!   - `.voxl` writer (§7) — section table + zstd/lz4 wrap on World

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("not yet implemented")]
    NotImplemented,
}

pub fn bundle_cart(_project_dir: &std::path::Path) -> Result<Vec<u8>, BundleError> {
    Err(BundleError::NotImplemented)
}
