//! Material table entry — see SPEC.md §2.

/// Material flags bitfield (see SPEC.md §2 "Flags bitfield").
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct MaterialFlags(pub u16);

impl MaterialFlags {
    pub const TRANSPARENT: u16 = 1 << 0;
    pub const GLOSSY:      u16 = 1 << 1;
    pub const GRANULAR:    u16 = 1 << 2;
    pub const LIQUID:      u16 = 1 << 3;
    pub const GAS:         u16 = 1 << 4;
    pub const FLAMMABLE:   u16 = 1 << 5;
    pub const FIRE:        u16 = 1 << 6;

    pub const fn empty() -> Self { Self(0) }
    pub const fn contains(self, flag: u16) -> bool { (self.0 & flag) != 0 }
    pub const fn with(self, flag: u16) -> Self { Self(self.0 | flag) }
}

/// 16-byte material entry. Carts ship 256 of these (§2 / §7).
///
/// Slot 0 is always "air" regardless of contents.
#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct Material {
    /// `(ramp << 2) | shade`, low 6 bits used; high 2 bits reserved.
    pub color: u8,
    /// Emission level 0–15; high 4 bits reserved.
    pub emission: u8,
    pub flags: MaterialFlags,
    /// Per-material CA tuning (0 = use platform default).
    pub ca_threshold: u8,
    pub ca_lifetime: u8,
    pub ca_viscosity: u8,
    /// For `flammable` materials: which material slot this cell becomes
    /// when its heat exceeds `ca_threshold`. Typically the cart's fire
    /// material. 0 = vanish to air on ignition (no fire produced).
    /// Unused for non-flammable materials.
    pub ignites_to: u8,
    /// Reserved for v2; must be zero in v1.
    pub _reserved: [u8; 8],
}

impl Material {
    pub const AIR: Self = Self {
        color: 0,
        emission: 0,
        flags: MaterialFlags(0),
        ca_threshold: 0,
        ca_lifetime: 0,
        ca_viscosity: 0,
        ignites_to: 0,
        _reserved: [0; 8],
    };

    /// Construct a `(ramp << 2) | shade` color byte from named components.
    pub const fn pack_color(ramp: u8, shade: u8) -> u8 {
        ((ramp & 0x0F) << 2) | (shade & 0x03)
    }
}

const _: () = assert!(core::mem::size_of::<Material>() == 16);

/// One entry in the system palette (§4.3). 64 of these total.
#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct PaletteColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Symbolic ramp identifiers matching `materials.toml` (§12.6.2 / §4.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Ramp {
    Brown = 0,
    Tan,
    ForestGreen,
    GrassGreen,
    Teal,
    Cyan,
    SkyBlue,
    DeepBlue,
    Purple,
    Pink,
    Red,
    Orange,
    Yellow,
    Magenta,
    CoolGray,
    WarmGray,
}

#[cfg(feature = "bytemuck")]
mod bytemuck_impls {
    use super::*;
    use bytemuck::{Pod, Zeroable};

    unsafe impl Zeroable for MaterialFlags {}
    unsafe impl Pod for MaterialFlags {}
    unsafe impl Zeroable for Material {}
    unsafe impl Pod for Material {}
    unsafe impl Zeroable for PaletteColor {}
    unsafe impl Pod for PaletteColor {}
}
