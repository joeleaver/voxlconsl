//! Bundler: turn a cart project directory into a `.voxl` cart.
//!
//! v1 ships a deliberately minimal pipeline:
//!
//!   1. Parse `cart.toml` (only the `[cart]` and `[code]` blocks are
//!      required in v1; everything else — materials, world, audio,
//!      save — is parking-lotted to later sessions).
//!   2. Either invoke `cargo build` for the cart's WASM crate, or use
//!      a pre-built `.wasm` path — whichever the manifest specifies.
//!   3. Wrap the WASM bytes in a `.voxl` carrying a Metadata section
//!      (the `[cart]` block as TOML) plus the Code section.
//!   4. Compute the CRC and emit.
//!
//! Materials, world, audio, save schema sections are not yet emitted;
//! they'll be wired in as the corresponding host subsystems land. A v1
//! cart is fully valid with just metadata + code.

use std::path::{Path, PathBuf};
use std::process::Command;

use voxlconsl_types::cart_format::{
    crc32_with_zeroed_field, CRC_FIELD_OFFSET, HEADER_SIZE, MAGIC, MAX_TOTAL_SIZE,
    SECTION_ENTRY_SIZE, SectionId, VERSION,
};

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("cart.toml not found at {0}")]
    ManifestMissing(PathBuf),
    #[error("cart.toml parse error: {0}")]
    ManifestParse(#[from] toml::de::Error),
    #[error("cart.toml validation: {0}")]
    ManifestInvalid(&'static str),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cart wasm not found at {0} (did the build step succeed?)")]
    WasmMissing(PathBuf),
    #[error("cart total size exceeds 32 MB cap: {0} bytes")]
    CartTooLarge(u64),
    #[error("cargo build failed (exit status {0:?})")]
    BuildFailed(Option<i32>),
}

#[derive(Debug, serde::Deserialize)]
struct ManifestRoot {
    cart: CartTable,
    code: CodeTable,
}

#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
pub struct CartTable {
    pub name: String,
    pub title: String,
    pub version: String,
    pub spec_version: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct CodeTable {
    /// `cargo build`-style command run from the project dir to produce
    /// the cart's `.wasm`. If absent, `wasm` must point to a pre-built
    /// blob.
    #[serde(default)]
    build: Option<String>,
    /// Path (relative to project_dir) of the `.wasm` to read after
    /// `build` runs (or directly when `build` is absent).
    #[serde(default)]
    output: Option<PathBuf>,
    #[serde(default)]
    wasm: Option<PathBuf>,
}

/// Build a cart in `project_dir`. Returns the bundled `.voxl` bytes.
pub fn bundle_cart(project_dir: &Path) -> Result<Vec<u8>, BundleError> {
    let manifest_path = project_dir.join("cart.toml");
    if !manifest_path.exists() {
        return Err(BundleError::ManifestMissing(manifest_path));
    }
    let manifest_text = std::fs::read_to_string(&manifest_path)?;
    let manifest: ManifestRoot = toml::from_str(&manifest_text)?;

    if manifest.cart.name.is_empty() {
        return Err(BundleError::ManifestInvalid("cart.name is empty"));
    }

    // Resolve the cart's WASM bytes — either run `code.build` and read
    // `code.output`, or read `code.wasm` directly.
    let wasm_path: PathBuf = match (&manifest.code.build, &manifest.code.output, &manifest.code.wasm) {
        (Some(_), Some(out), _) => project_dir.join(out),
        (None, _, Some(prebuilt)) => project_dir.join(prebuilt),
        _ => return Err(BundleError::ManifestInvalid(
            "[code] needs either `build` + `output` or `wasm`",
        )),
    };

    if let Some(build_cmd) = &manifest.code.build {
        // Run via the user's shell so multi-word commands like
        // `cargo build --target wasm32-unknown-unknown --release`
        // work without us tokenising them ourselves.
        let status = Command::new("sh")
            .arg("-c")
            .arg(build_cmd)
            .current_dir(project_dir)
            .status()?;
        if !status.success() {
            return Err(BundleError::BuildFailed(status.code()));
        }
    }

    if !wasm_path.exists() {
        return Err(BundleError::WasmMissing(wasm_path));
    }
    let wasm = std::fs::read(&wasm_path)?;

    // §7 says ≤ 1 MB recommended. We don't fail on it (the spec word
    // is "recommended"); bigger carts just print a warning.
    if wasm.len() as u64 > 1_048_576 {
        eprintln!(
            "warning: cart wasm is {} bytes, exceeds the 1 MB recommended cap",
            wasm.len()
        );
    }

    write_voxl(&manifest.cart, &wasm)
}

/// Write a `.voxl` blob from a metadata table + code bytes.
pub fn write_voxl(cart_table: &CartTable, code: &[u8]) -> Result<Vec<u8>, BundleError> {
    let metadata_toml = toml::to_string(cart_table)
        .expect("CartTable serialization is infallible");
    let metadata_bytes = metadata_toml.as_bytes();

    // 2 sections in v1: Metadata then Code.
    let section_count = 2u8;
    let table_size = section_count as usize * SECTION_ENTRY_SIZE;
    let metadata_offset = HEADER_SIZE + table_size;
    let code_offset = metadata_offset + metadata_bytes.len();
    let total = code_offset + code.len();

    if total as u64 > MAX_TOTAL_SIZE as u64 {
        return Err(BundleError::CartTooLarge(total as u64));
    }

    let mut buf = vec![0u8; total];

    // Header
    buf[..10].copy_from_slice(&MAGIC);
    buf[10..12].copy_from_slice(&VERSION.to_le_bytes());
    // flags = 0 (already zero-init)
    buf[14] = section_count;
    buf[16..20].copy_from_slice(&(total as u32).to_le_bytes());
    // CRC patched after the rest of the file is laid out.

    // Section table
    write_section_entry(
        &mut buf, HEADER_SIZE,
        SectionId::Metadata, metadata_offset, metadata_bytes.len(),
    );
    write_section_entry(
        &mut buf, HEADER_SIZE + SECTION_ENTRY_SIZE,
        SectionId::Code, code_offset, code.len(),
    );

    // Section payloads
    buf[metadata_offset..metadata_offset + metadata_bytes.len()]
        .copy_from_slice(metadata_bytes);
    buf[code_offset..code_offset + code.len()].copy_from_slice(code);

    // CRC last.
    let crc = crc32_with_zeroed_field(&buf, CRC_FIELD_OFFSET);
    buf[CRC_FIELD_OFFSET..CRC_FIELD_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());

    Ok(buf)
}

fn write_section_entry(buf: &mut [u8], at: usize, id: SectionId, offset: usize, size: usize) {
    buf[at..at + 2].copy_from_slice(&(id as u16).to_le_bytes());
    // flags = 0 (already zero)
    buf[at + 4..at + 8].copy_from_slice(&(offset as u32).to_le_bytes());
    buf[at + 8..at + 12].copy_from_slice(&(size as u32).to_le_bytes());
    buf[at + 12..at + 16].copy_from_slice(&(size as u32).to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxlconsl_types::cart_format::Cart as VoxlCart;

    #[test]
    fn round_trip_minimal() {
        let cart = CartTable {
            name: "test-cart".into(),
            title: "Test Cart".into(),
            version: "0.1.0".into(),
            spec_version: "0.1".into(),
            author: None,
            description: None,
            license: None,
        };
        let blob = write_voxl(&cart, b"\x00asm\x01\x00\x00\x00").expect("write");
        let parsed = VoxlCart::parse(&blob).expect("parse");
        assert_eq!(parsed.code(), b"\x00asm\x01\x00\x00\x00");
        let meta = parsed.metadata_toml().expect("metadata");
        assert!(meta.contains("name = \"test-cart\""));
        assert!(meta.contains("title = \"Test Cart\""));
    }
}
