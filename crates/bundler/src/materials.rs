//! `materials.toml` → 256 × 16-byte Material section blob.
//!
//! See SPEC.md §12.6.2 for the source format and §2.4 / §7 for the
//! on-disk layout. Slot 0 is reserved for air; the bundler refuses
//! any non-default entry pointing at it. Slots not listed default to
//! all-zero ("air-like — material exists but is empty"), which matches
//! the runtime's `Material::AIR`.

use std::path::Path;

use voxlconsl_types::{Material, MaterialFlags};

use crate::BundleError;

/// Read a `materials.toml` from disk and produce the 4096-byte section
/// payload. The result is `256 × 16` bytes of `repr(C) Material`
/// structs in slot order.
pub fn build_materials_section(path: &Path) -> Result<Vec<u8>, BundleError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        BundleError::AssetIo(format!("read {}: {e}", path.display()))
    })?;
    let parsed: MaterialsToml = toml::from_str(&text).map_err(|e| {
        BundleError::AssetParse(format!("{}: {e}", path.display()))
    })?;
    encode_materials(&parsed.material)
}

#[derive(Debug, serde::Deserialize)]
struct MaterialsToml {
    #[serde(default)]
    material: Vec<MaterialEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct MaterialEntry {
    slot: u8,
    #[serde(default)]
    #[allow(dead_code)] // documentation-only; not embedded in the cart
    name: Option<String>,
    color: ColorRef,
    #[serde(default)]
    emission: u8,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    ca_threshold: u8,
    #[serde(default)]
    ca_lifetime: u8,
    #[serde(default)]
    ca_viscosity: u8,
    #[serde(default)]
    ignites_to: u8,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum ColorRef {
    Named(String),
    Raw(u8),
}

fn encode_materials(entries: &[MaterialEntry]) -> Result<Vec<u8>, BundleError> {
    let mut table = [Material::AIR; 256];
    let mut seen = [false; 256];

    for entry in entries {
        if entry.slot == 0 {
            return Err(BundleError::Asset(
                "materials.toml: slot 0 is reserved for air".into(),
            ));
        }
        if seen[entry.slot as usize] {
            return Err(BundleError::Asset(format!(
                "materials.toml: slot {} defined twice",
                entry.slot
            )));
        }
        seen[entry.slot as usize] = true;

        let color = decode_color(&entry.color)?;
        if entry.emission > 15 {
            return Err(BundleError::Asset(format!(
                "materials.toml: slot {} emission {} > 15",
                entry.slot, entry.emission
            )));
        }
        let mut flags = MaterialFlags::empty();
        for f in &entry.flags {
            flags = flags.with(decode_flag(f)?);
        }

        table[entry.slot as usize] = Material {
            color,
            emission: entry.emission,
            flags,
            ca_threshold: entry.ca_threshold,
            ca_lifetime: entry.ca_lifetime,
            ca_viscosity: entry.ca_viscosity,
            ignites_to: entry.ignites_to,
            _reserved: [0; 8],
        };
    }

    // Each Material is `repr(C)` 16 bytes; bytemuck Pod is gated on the
    // `bytemuck` feature which the bundler crate already enables.
    let bytes = bytemuck::cast_slice::<Material, u8>(&table);
    debug_assert_eq!(bytes.len(), 256 * 16);
    Ok(bytes.to_vec())
}

fn decode_color(c: &ColorRef) -> Result<u8, BundleError> {
    match c {
        ColorRef::Raw(v) => {
            if *v > 63 {
                return Err(BundleError::Asset(format!(
                    "materials.toml: color {v} > 63"
                )));
            }
            Ok(*v)
        }
        ColorRef::Named(s) => parse_named_color(s),
    }
}

fn parse_named_color(s: &str) -> Result<u8, BundleError> {
    let (ramp_name, shade_str) = s.split_once(':').ok_or_else(|| {
        BundleError::Asset(format!(
            "materials.toml: color {s:?} not in 'ramp:shade' form"
        ))
    })?;
    let shade: u8 = shade_str.parse().map_err(|_| {
        BundleError::Asset(format!(
            "materials.toml: color {s:?} shade {shade_str:?} not 0..=3"
        ))
    })?;
    if shade > 3 {
        return Err(BundleError::Asset(format!(
            "materials.toml: color {s:?} shade {shade} > 3"
        )));
    }
    let ramp = ramp_from_name(ramp_name).ok_or_else(|| {
        BundleError::Asset(format!("materials.toml: unknown ramp {ramp_name:?}"))
    })?;
    Ok(Material::pack_color(ramp, shade))
}

fn ramp_from_name(name: &str) -> Option<u8> {
    Some(match name {
        "brown" => 0,
        "tan" => 1,
        "forest_green" => 2,
        "grass_green" => 3,
        "teal" => 4,
        "cyan" => 5,
        "sky_blue" => 6,
        "deep_blue" => 7,
        "purple" => 8,
        "pink" => 9,
        "red" => 10,
        "orange" => 11,
        "yellow" => 12,
        "magenta" => 13,
        "cool_gray" => 14,
        "warm_gray" => 15,
        _ => return None,
    })
}

fn decode_flag(name: &str) -> Result<u16, BundleError> {
    Ok(match name {
        "transparent" => MaterialFlags::TRANSPARENT,
        "glossy" => MaterialFlags::GLOSSY,
        "granular" => MaterialFlags::GRANULAR,
        "liquid" => MaterialFlags::LIQUID,
        "gas" => MaterialFlags::GAS,
        "flammable" => MaterialFlags::FLAMMABLE,
        "fire" => MaterialFlags::FIRE,
        other => {
            return Err(BundleError::Asset(format!(
                "materials.toml: unknown flag {other:?}"
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slot: u8, color: ColorRef) -> MaterialEntry {
        MaterialEntry {
            slot,
            name: None,
            color,
            emission: 0,
            flags: vec![],
            ca_threshold: 0,
            ca_lifetime: 0,
            ca_viscosity: 0,
            ignites_to: 0,
        }
    }

    #[test]
    fn empty_table_is_all_zeros() {
        let bytes = encode_materials(&[]).unwrap();
        assert_eq!(bytes.len(), 4096);
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn named_color_packs_ramp_and_shade() {
        let b = parse_named_color("cool_gray:1").unwrap();
        assert_eq!(b, Material::pack_color(14, 1));
    }

    #[test]
    fn raw_color_passes_through() {
        let bytes = encode_materials(&[entry(1, ColorRef::Raw(42))]).unwrap();
        assert_eq!(bytes[16], 42); // slot 1, color byte
    }

    #[test]
    fn flag_combinations_round_trip() {
        let m = MaterialEntry {
            slot: 5,
            name: None,
            color: ColorRef::Raw(10),
            emission: 0,
            flags: vec!["liquid".into(), "flammable".into()],
            ca_threshold: 30,
            ca_lifetime: 0,
            ca_viscosity: 6,
            ignites_to: 13,
        };
        let bytes = encode_materials(&[m]).unwrap();
        let materials: &[Material] = bytemuck::cast_slice(&bytes);
        let mat = &materials[5];
        assert_eq!(mat.color, 10);
        assert!(mat.flags.contains(MaterialFlags::LIQUID));
        assert!(mat.flags.contains(MaterialFlags::FLAMMABLE));
        assert_eq!(mat.ca_threshold, 30);
        assert_eq!(mat.ca_viscosity, 6);
        assert_eq!(mat.ignites_to, 13);
    }

    #[test]
    fn slot_zero_rejected() {
        let err = encode_materials(&[entry(0, ColorRef::Raw(0))]).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("slot 0"));
    }

    #[test]
    fn duplicate_slot_rejected() {
        let err = encode_materials(&[
            entry(3, ColorRef::Raw(0)),
            entry(3, ColorRef::Raw(0)),
        ])
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("twice"));
    }

    #[test]
    fn unknown_ramp_rejected() {
        let err = parse_named_color("nonexistent:0").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("nonexistent"));
    }

    #[test]
    fn parses_full_toml() {
        let src = r#"
            [[material]]
            slot = 1
            name = "stone"
            color = "cool_gray:1"

            [[material]]
            slot = 4
            color = "brown:0"
            flags = ["flammable"]
            ca_threshold = 90
            ignites_to = 13
        "#;
        let parsed: MaterialsToml = toml::from_str(src).unwrap();
        let bytes = encode_materials(&parsed.material).unwrap();
        let materials: &[Material] = bytemuck::cast_slice(&bytes);
        assert_eq!(materials[1].color, Material::pack_color(14, 1));
        assert!(materials[4].flags.contains(MaterialFlags::FLAMMABLE));
        assert_eq!(materials[4].ca_threshold, 90);
        assert_eq!(materials[4].ignites_to, 13);
    }
}
