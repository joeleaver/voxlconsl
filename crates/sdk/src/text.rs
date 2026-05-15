//! Cart-side voxel text/font helpers — see SPEC.md §11.10 and §12.7.
//!
//! Text in voxlconsl is voxels: glyphs are flat 2D bitmaps stored in
//! `.vfnt` files, extruded along a third axis at paint time and written
//! into either world voxels (for permanent signs) or a caller-provided
//! dense buffer (for actor-volume HUD / dialog text).
//!
//! Three built-in fonts ship with the SDK:
//!
//! - [`FONT_TINY`] — 4×6, the smallest legible cell for the 256×144
//!   framebuffer. Uppercase + digits + punctuation only. Sidebar
//!   HUDs and dense score readouts.
//! - [`FONT_ANSI`] — 10×11, derived from the figlet "ANSI Regular" font.
//!   Clean blocky letterforms, good for HUD and dialog.
//! - [`FONT_DCP1`] — 16×18, derived from the figlet "Delta Corps Priest 1"
//!   font. Stylized chiseled-serif look, good for title signage.
//!
//! Carts can ship their own `.vfnt` blobs and parse them with
//! [`Font::from_bytes`].

use voxlconsl_types::{UVec3, U8Vec3};

use crate::{set_voxel, fill_box};

/// Which 2D plane glyphs live in. The perpendicular axis is the
/// extrusion direction along which the painter repeats the 2D bitmap
/// `depth` times.
///
/// In all variants the painted glyph reads "right-side up" in the chosen
/// plane: glyph row 0 ends up at the top (highest coord on the vertical
/// axis), glyph col 0 is at the left (lowest coord on the horizontal
/// axis), and the first extrusion slice is at `origin` on the third axis.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Glyph in XY plane, extrudes along +Z.
    XY,
    /// Glyph in XZ plane (X horizontal, Z vertical), extrudes along +Y.
    XZ,
    /// Glyph in YZ plane (Z horizontal, Y vertical), extrudes along +X.
    YZ,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FontError {
    BadMagic,
    UnsupportedVersion,
    InvalidCellSize,
    UnsupportedFlags,
    Truncated,
    IndexNotSorted,
}

/// Parsed handle to a `.vfnt` font. Zero-copy: borrows the source byte
/// slice rather than owning it.
#[derive(Copy, Clone)]
pub struct Font<'a> {
    bytes: &'a [u8],
    cell_w: u8,
    cell_h: u8,
    glyph_count: u16,
    bitmap_start: usize,
}

impl<'a> Font<'a> {
    pub const fn cell_width(&self) -> u8 { self.cell_w }
    pub const fn cell_height(&self) -> u8 { self.cell_h }
    pub const fn glyph_count(&self) -> u16 { self.glyph_count }

    /// Parse a `.vfnt` blob. Const-evaluable, so the built-in fonts can
    /// be `pub static FONT_*: Font<'static>`.
    pub const fn from_bytes(bytes: &'a [u8]) -> Result<Self, FontError> {
        if bytes.len() < 16 {
            return Err(FontError::Truncated);
        }
        if bytes[0] != b'V' || bytes[1] != b'F' || bytes[2] != b'N' || bytes[3] != b'1' {
            return Err(FontError::BadMagic);
        }
        if bytes[4] != 1 {
            return Err(FontError::UnsupportedVersion);
        }
        let cell_w = bytes[5];
        let cell_h = bytes[6];
        let flags = bytes[7];
        if cell_w == 0 || cell_w > 64 || cell_h == 0 || cell_h > 64 {
            return Err(FontError::InvalidCellSize);
        }
        if flags != 0 {
            return Err(FontError::UnsupportedFlags);
        }
        let glyph_count = u16::from_le_bytes([bytes[8], bytes[9]]);
        let index_size = (glyph_count as usize) * 8;
        let bitmap_start = 16 + index_size;
        if bytes.len() < bitmap_start {
            return Err(FontError::Truncated);
        }

        // Verify the index is strictly ascending by codepoint.
        let mut prev: i64 = -1;
        let mut i: usize = 0;
        while i < glyph_count as usize {
            let off = 16 + i * 8;
            let cp = u32::from_le_bytes([
                bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3],
            ]) as i64;
            if cp <= prev {
                return Err(FontError::IndexNotSorted);
            }
            prev = cp;
            i += 1;
        }

        Ok(Self {
            bytes,
            cell_w,
            cell_h,
            glyph_count,
            bitmap_start,
        })
    }

    /// Look up a codepoint's offset within the bitmap section, or `None`
    /// if not present in the font.
    fn glyph_bitmap_off(&self, codepoint: u32) -> Option<u32> {
        let n = self.glyph_count as usize;
        if n == 0 {
            return None;
        }
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let off = 16 + mid * 8;
            let cp = u32::from_le_bytes([
                self.bytes[off],
                self.bytes[off + 1],
                self.bytes[off + 2],
                self.bytes[off + 3],
            ]);
            if cp == codepoint {
                return Some(u32::from_le_bytes([
                    self.bytes[off + 4],
                    self.bytes[off + 5],
                    self.bytes[off + 6],
                    self.bytes[off + 7],
                ]));
            } else if cp < codepoint {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        None
    }

    /// Read a single bit of a glyph's bitmap. Returns `false` for
    /// out-of-cell coords or codepoints not in the font.
    pub fn glyph_bit(&self, codepoint: u32, x: u8, y: u8) -> bool {
        if x >= self.cell_w || y >= self.cell_h {
            return false;
        }
        let Some(rel) = self.glyph_bitmap_off(codepoint) else {
            return false;
        };
        let bit_index = y as usize * self.cell_w as usize + x as usize;
        let byte_index = self.bitmap_start + rel as usize + bit_index / 8;
        if byte_index >= self.bytes.len() {
            return false;
        }
        let mask = 1u8 << (7 - (bit_index % 8));
        (self.bytes[byte_index] & mask) != 0
    }

    /// Leftmost / rightmost inked columns of a glyph (inclusive).
    /// Returns `None` for glyphs absent from the font and for fully-empty
    /// cells (e.g. ASCII space). Used to derive proportional advance
    /// widths from a fixed-cell bitmap font.
    pub fn glyph_ink_bounds(&self, codepoint: u32) -> Option<(u8, u8)> {
        self.glyph_bitmap_off(codepoint)?;
        let mut left: Option<u8> = None;
        let mut right: u8 = 0;
        let mut col: u8 = 0;
        while col < self.cell_w {
            let mut any = false;
            let mut row: u8 = 0;
            while row < self.cell_h {
                if self.glyph_bit(codepoint, col, row) {
                    any = true;
                    break;
                }
                row += 1;
            }
            if any {
                if left.is_none() { left = Some(col); }
                right = col;
            }
            col += 1;
        }
        left.map(|l| (l, right))
    }

    /// Default inter-letter spacing in glyph cells (scale=1). Derived
    /// from the cell width so a 16-px font gets a 2-px gap and a 4-px
    /// font still gets at least 1.
    pub const fn letter_spacing(&self) -> u8 {
        let s = self.cell_w / 8;
        if s == 0 { 1 } else { s }
    }

    /// Pen advance (in glyph cells) after painting this glyph: ink
    /// width + `letter_spacing()`. Glyphs that aren't in the font (and
    /// purely-empty cells such as ASCII space) return `cell_w / 2` as a
    /// reasonable default whitespace width.
    pub fn glyph_advance(&self, codepoint: u32) -> u32 {
        match self.glyph_ink_bounds(codepoint) {
            Some((l, r)) => (r - l + 1) as u32 + self.letter_spacing() as u32,
            None         => (self.cell_w / 2).max(2) as u32,
        }
    }

    /// Cumulative pen offset (in glyph cells) before painting char index
    /// `char_idx` of `s`. Equivalent to summing `glyph_advance` over the
    /// preceding chars. Useful when a cart needs to locate the world
    /// position of a specific character after proportional layout
    /// (e.g. "where does the 'C' in INCIDENT land?").
    pub fn pen_offset(&self, s: &str, char_idx: usize) -> u32 {
        let mut off: u32 = 0;
        for ch in s.chars().take(char_idx) {
            off = off.saturating_add(self.glyph_advance(ch as u32));
        }
        off
    }
}

/// `(col_unit, vert_unit, slice_unit)` — each is a unit vector with
/// components in {0, 1}. The painter walks the glyph in (col, vert,
/// slice) space and projects into world space by linear combination.
const fn axis_units(a: Axis) -> (UVec3, UVec3, UVec3) {
    match a {
        Axis::XY => (
            UVec3::new(1, 0, 0),
            UVec3::new(0, 1, 0),
            UVec3::new(0, 0, 1),
        ),
        Axis::XZ => (
            UVec3::new(1, 0, 0),
            UVec3::new(0, 0, 1),
            UVec3::new(0, 1, 0),
        ),
        Axis::YZ => (
            UVec3::new(0, 0, 1),
            UVec3::new(0, 1, 0),
            UVec3::new(1, 0, 0),
        ),
    }
}

/// Compute the 3D extents the text will occupy when painted, in
/// `(horizontal, vertical, depth)` glyph-local order. Map to world
/// axes per your chosen [`Axis`].
///
/// Multi-line layouts are cart-side: split the string yourself and call
/// `measure` per line.
pub fn measure(font: &Font, scale: u8, depth: u32, s: &str) -> U8Vec3 {
    let scale = if scale == 0 { 1 } else { scale } as u32;
    let depth = if depth == 0 { 1 } else { depth };
    // Sum proportional advances; the last char contributes its ink
    // width with no trailing letter-spacing.
    let mut w: u32 = 0;
    let mut last_trailing: u32 = 0;
    for ch in s.chars() {
        let adv = font.glyph_advance(ch as u32);
        w = w.saturating_add(adv);
        last_trailing = match font.glyph_ink_bounds(ch as u32) {
            Some(_) => font.letter_spacing() as u32,
            None    => 0, // whitespace/missing: no trailing letter-spacing
        };
    }
    w = w.saturating_sub(last_trailing);
    let h = font.cell_h as u32 * scale;
    U8Vec3::new(
        (w * scale).min(255) as u8,
        h.min(255) as u8,
        depth.min(255) as u8,
    )
}

/// For a set glyph bit, paint the column of `depth` voxels along the
/// extrusion axis. `face_color = Some(m)` paints slice 0 (the slice at
/// `origin` on the extrusion axis) with material `m`; the remaining
/// `depth - 1` slices use `color`.
fn paint_extrusion_world(
    origin: UVec3,
    slice_u: UVec3,
    color: u8,
    face_color: Option<u8>,
    depth: u32,
) {
    match face_color {
        Some(m) => {
            // Slice 0 — the face.
            set_voxel(origin, m);
            if depth > 1 {
                let rest_min = UVec3::new(
                    origin.x + slice_u.x,
                    origin.y + slice_u.y,
                    origin.z + slice_u.z,
                );
                let rest_max = UVec3::new(
                    origin.x + slice_u.x * (depth - 1),
                    origin.y + slice_u.y * (depth - 1),
                    origin.z + slice_u.z * (depth - 1),
                );
                fill_box(rest_min, rest_max, color);
            }
        }
        None => {
            let max = UVec3::new(
                origin.x + slice_u.x * (depth - 1),
                origin.y + slice_u.y * (depth - 1),
                origin.z + slice_u.z * (depth - 1),
            );
            if depth == 1 {
                set_voxel(origin, color);
            } else {
                fill_box(origin, max, color);
            }
        }
    }
}

/// Paint text into world voxels via [`set_voxel`] / [`fill_box`].
///
/// `origin` is the bottom-left-front corner of the painted volume in the
/// chosen plane; the text grows toward `+col`, `+vert` (the top of the
/// glyph is at the highest vertical coord), and the extrusion runs along
/// `+slice`. `scale` is a per-axis voxel multiplier in the painted plane
/// (1 = one voxel per glyph bit). `depth >= 1` slices are painted along
/// the third axis. Codepoints not present in the font are skipped
/// silently — the caller advances past them as if they were a space-width
/// blank.
pub fn paint_world(
    font: &Font,
    origin: UVec3,
    axis: Axis,
    color: u8,
    face_color: Option<u8>,
    scale: u8,
    depth: u32,
    s: &str,
) {
    let (col_u, vert_u, slice_u) = axis_units(axis);
    let cell_h = font.cell_h as u32;
    let scale = if scale == 0 { 1 } else { scale } as u32;
    let depth = if depth == 0 { 1 } else { depth };

    let mut pen: u32 = 0;
    for ch in s.chars() {
        let cp = ch as u32;
        let (left, right) = match font.glyph_ink_bounds(cp) {
            Some(b) => b,
            None    => {
                // Whitespace / missing glyph: advance, paint nothing.
                pen = pen.saturating_add(font.glyph_advance(cp));
                continue;
            }
        };
        for row in 0..cell_h {
            for col in (left as u32)..=(right as u32) {
                if !font.glyph_bit(cp, col as u8, row as u8) {
                    continue;
                }
                // Shift the glyph leftward by `left` so each glyph's
                // leftmost inked column lands at the pen position.
                // Top-of-glyph (row 0) lands at the highest vertical
                // coord; convert by inverting through the vertical span.
                let base_col = (pen + (col - left as u32)) * scale;
                let base_vert_top = (cell_h - 1 - row) * scale;
                for sx in 0..scale {
                    for sy in 0..scale {
                        let w_col = base_col + sx;
                        let w_vert = base_vert_top + sy;
                        let voxel_origin = UVec3::new(
                            origin.x
                                + w_col * col_u.x
                                + w_vert * vert_u.x,
                            origin.y
                                + w_col * col_u.y
                                + w_vert * vert_u.y,
                            origin.z
                                + w_col * col_u.z
                                + w_vert * vert_u.z,
                        );
                        paint_extrusion_world(
                            voxel_origin,
                            slice_u,
                            color,
                            face_color,
                            depth,
                        );
                    }
                }
            }
        }
        pen = pen.saturating_add(font.glyph_advance(cp));
    }
}

/// Rasterize text into a caller-provided dense buffer. `buf` is laid out
/// row-major (x fastest, then y, then z) — the same layout
/// [`crate::prefab_define`] expects.
///
/// Returns the `(x, y, z)` extents actually written, suitable for passing
/// to `prefab_define`. Voxels outside the buffer are silently clipped.
pub fn rasterize_into(
    font: &Font,
    buf: &mut [u8],
    buf_size: U8Vec3,
    axis: Axis,
    color: u8,
    face_color: Option<u8>,
    scale: u8,
    depth: u32,
    s: &str,
) -> U8Vec3 {
    let (col_u, vert_u, slice_u) = axis_units(axis);
    let cell_h = font.cell_h as u32;
    let scale = if scale == 0 { 1 } else { scale } as u32;
    let depth = if depth == 0 { 1 } else { depth };

    let bx = buf_size.x as u32;
    let by = buf_size.y as u32;
    let bz = buf_size.z as u32;

    let mut max_x: u32 = 0;
    let mut max_y: u32 = 0;
    let mut max_z: u32 = 0;

    let mut write = |x: u32, y: u32, z: u32, m: u8| {
        if x >= bx || y >= by || z >= bz {
            return;
        }
        let idx = ((z * by) + y) * bx + x;
        let i = idx as usize;
        if i < buf.len() {
            buf[i] = m;
            if x + 1 > max_x { max_x = x + 1; }
            if y + 1 > max_y { max_y = y + 1; }
            if z + 1 > max_z { max_z = z + 1; }
        }
    };

    let mut pen: u32 = 0;
    for ch in s.chars() {
        let cp = ch as u32;
        let (left, right) = match font.glyph_ink_bounds(cp) {
            Some(b) => b,
            None    => {
                pen = pen.saturating_add(font.glyph_advance(cp));
                continue;
            }
        };
        for row in 0..cell_h {
            for col in (left as u32)..=(right as u32) {
                if !font.glyph_bit(cp, col as u8, row as u8) {
                    continue;
                }
                let base_col = (pen + (col - left as u32)) * scale;
                let base_vert_top = (cell_h - 1 - row) * scale;
                for sx in 0..scale {
                    for sy in 0..scale {
                        let w_col = base_col + sx;
                        let w_vert = base_vert_top + sy;
                        for d in 0..depth {
                            let m = match face_color {
                                Some(fc) if d == 0 => fc,
                                _ => color,
                            };
                            let x = w_col * col_u.x
                                + w_vert * vert_u.x
                                + d * slice_u.x;
                            let y = w_col * col_u.y
                                + w_vert * vert_u.y
                                + d * slice_u.y;
                            let z = w_col * col_u.z
                                + w_vert * vert_u.z
                                + d * slice_u.z;
                            write(x, y, z, m);
                        }
                    }
                }
            }
        }
        pen = pen.saturating_add(font.glyph_advance(cp));
    }

    U8Vec3::new(
        max_x.min(255) as u8,
        max_y.min(255) as u8,
        max_z.min(255) as u8,
    )
}

// ============================================================================
// Built-in fonts.
//
// The .vfnt blobs are generated by `scripts/flf_to_vfnt.py` from the
// figlet sources in `scripts/`. Re-run the script when changing fonts.
// ============================================================================

const FONT_ANSI_BYTES: &[u8] = include_bytes!("fonts/ansi_regular.vfnt");
const FONT_DCP1_BYTES: &[u8] = include_bytes!("fonts/delta_corps_priest_1.vfnt");
const FONT_TINY_BYTES: &[u8] = include_bytes!("fonts/tiny.vfnt");

/// Built-in 10×11 font derived from the figlet "ANSI Regular" face.
/// Clean blocky letterforms; a sensible default for HUD and dialog.
pub static FONT_ANSI: Font<'static> = match Font::from_bytes(FONT_ANSI_BYTES) {
    Ok(f) => f,
    Err(_) => panic!("invalid built-in ANSI Regular .vfnt"),
};

/// Built-in 16×18 font derived from the figlet "Delta Corps Priest 1"
/// face. Stylized chiseled-serif look; suits title signage and stone-
/// carved messaging.
pub static FONT_DCP1: Font<'static> = match Font::from_bytes(FONT_DCP1_BYTES) {
    Ok(f) => f,
    Err(_) => panic!("invalid built-in Delta Corps Priest 1 .vfnt"),
};

/// Built-in 4×6 font — the smallest legible voxel cell for the
/// console's 256×144 framebuffer. Uppercase + digits + punctuation
/// only (lowercase codepoints are absent and render as blanks; carts
/// should uppercase their text before painting). Use this for
/// sidebar HUDs, score readouts, and any text that needs to share a
/// screen with the world. Source spec is `scripts/build_tiny_font.py`.
pub static FONT_TINY: Font<'static> = match Font::from_bytes(FONT_TINY_BYTES) {
    Ok(f) => f,
    Err(_) => panic!("invalid built-in tiny .vfnt"),
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-built minimal valid .vfnt: cell 3×2, glyph 'A' = 0b101_010
    /// ("#.#" over ".#."). 17 bytes total.
    const TINY: [u8; 25] = [
        b'V', b'F', b'N', b'1',     // magic
        1,                           // version
        3, 2, 0,                     // cell_w, cell_h, flags
        1, 0,                        // glyph_count = 1
        0, 0, 0, 0, 0, 0,            // reserved
        b'A', 0, 0, 0,               // codepoint 'A'
        0, 0, 0, 0,                  // bitmap_off 0
        0b101_010_00,                // 6 bits, padded to a byte
    ];

    #[test]
    fn parses_minimal_font() {
        let f = Font::from_bytes(&TINY).expect("parse");
        assert_eq!(f.cell_width(), 3);
        assert_eq!(f.cell_height(), 2);
        assert_eq!(f.glyph_count(), 1);
        assert!(f.glyph_bit(b'A' as u32, 0, 0));
        assert!(!f.glyph_bit(b'A' as u32, 1, 0));
        assert!(f.glyph_bit(b'A' as u32, 2, 0));
        assert!(!f.glyph_bit(b'A' as u32, 0, 1));
        assert!(f.glyph_bit(b'A' as u32, 1, 1));
        assert!(!f.glyph_bit(b'A' as u32, 2, 1));
        // Out-of-cell and missing-codepoint reads → false, no panic.
        assert!(!f.glyph_bit(b'A' as u32, 3, 0));
        assert!(!f.glyph_bit(b'B' as u32, 0, 0));
    }

    fn err_of(r: Result<Font<'_>, FontError>) -> Option<FontError> {
        r.err()
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = TINY;
        bytes[0] = b'X';
        assert_eq!(err_of(Font::from_bytes(&bytes)), Some(FontError::BadMagic));
    }

    #[test]
    fn rejects_bad_version() {
        let mut bytes = TINY;
        bytes[4] = 99;
        assert_eq!(err_of(Font::from_bytes(&bytes)), Some(FontError::UnsupportedVersion));
    }

    #[test]
    fn rejects_truncated() {
        assert_eq!(err_of(Font::from_bytes(&TINY[..10])), Some(FontError::Truncated));
    }

    #[test]
    fn rejects_unsorted_index() {
        // Two glyphs, codepoints 'B' then 'A' — descending → reject.
        let mut full = [0u8; 16 + 16 + 2];
        full[..4].copy_from_slice(b"VFN1");
        full[4] = 1;
        full[5] = 1;
        full[6] = 1;
        full[7] = 0;
        full[8] = 2;
        full[16..20].copy_from_slice(&(b'B' as u32).to_le_bytes());
        full[20..24].copy_from_slice(&0u32.to_le_bytes());
        full[24..28].copy_from_slice(&(b'A' as u32).to_le_bytes());
        full[28..32].copy_from_slice(&1u32.to_le_bytes());
        full[32] = 0x80;
        full[33] = 0x80;
        assert_eq!(err_of(Font::from_bytes(&full)), Some(FontError::IndexNotSorted));
    }

    #[test]
    fn builtin_fonts_parse() {
        // Just touching the statics asserts the const parse succeeded.
        assert_eq!(FONT_ANSI.cell_width(), 10);
        assert_eq!(FONT_ANSI.cell_height(), 11);
        assert_eq!(FONT_ANSI.glyph_count(), 95);
        assert_eq!(FONT_DCP1.cell_width(), 16);
        assert_eq!(FONT_DCP1.cell_height(), 18);
        assert_eq!(FONT_DCP1.glyph_count(), 95);
        // Spot-check that 'A' has *some* set bits in both fonts.
        let mut any_ansi = false;
        for x in 0..FONT_ANSI.cell_width() {
            for y in 0..FONT_ANSI.cell_height() {
                if FONT_ANSI.glyph_bit(b'A' as u32, x, y) {
                    any_ansi = true;
                }
            }
        }
        assert!(any_ansi, "ANSI 'A' should have set bits");
    }

    #[test]
    fn measure_extents() {
        // Proportional advance: width depends on each glyph's actual
        // inked columns + letter_spacing between glyphs. Height/depth
        // stay fixed-cell.
        let m = measure(&FONT_ANSI, 1, 1, "Hi");
        let expected_w = {
            let (hl, hr) = FONT_ANSI.glyph_ink_bounds('H' as u32).unwrap();
            let (il, ir) = FONT_ANSI.glyph_ink_bounds('i' as u32).unwrap();
            (hr - hl + 1) as u32 + FONT_ANSI.letter_spacing() as u32 + (ir - il + 1) as u32
        };
        assert_eq!(m.x as u32, expected_w);
        assert_eq!(m.y, 11);
        assert_eq!(m.z, 1);

        let m = measure(&FONT_DCP1, 2, 8, "X");
        let (xl, xr) = FONT_DCP1.glyph_ink_bounds('X' as u32).unwrap();
        assert_eq!(m.x as u32, (xr - xl + 1) as u32 * 2); // single glyph, no trailing spacing
        assert_eq!(m.y, 36);
        assert_eq!(m.z, 8);
    }

    #[test]
    fn glyph_ink_bounds_basic() {
        // FONT_TINY's 'A' has inked columns within its 4-col cell.
        let (l, r) = FONT_TINY.glyph_ink_bounds('A' as u32).expect("A has ink");
        assert!(l <= r);
        assert!(r < FONT_TINY.cell_width());
        // Space is absent / empty → None.
        assert!(FONT_TINY.glyph_ink_bounds(' ' as u32).is_none());
    }

    #[test]
    fn pen_offset_matches_advance_sum() {
        // pen_offset(s, k) should equal the sum of glyph_advance for
        // chars 0..k.
        let s = "ABC";
        let manual: u32 = s.chars().take(2).map(|c| FONT_ANSI.glyph_advance(c as u32)).sum();
        assert_eq!(FONT_ANSI.pen_offset(s, 2), manual);
        assert_eq!(FONT_ANSI.pen_offset(s, 0), 0);
    }

    #[test]
    fn rasterize_writes_into_buf() {
        // Use the tiny font; rasterize "A" with scale=1, depth=1 in XY.
        // Buffer 3×2×1.
        let f = Font::from_bytes(&TINY).expect("parse");
        let mut buf = [0u8; 6];
        let extents = rasterize_into(
            &f,
            &mut buf,
            U8Vec3::new(3, 2, 1),
            Axis::XY,
            42,
            None,
            1,
            1,
            "A",
        );
        assert_eq!(extents, U8Vec3::new(3, 2, 1));
        // Glyph: row 0 (top, y=1) = "#.#", row 1 (bottom, y=0) = ".#."
        // Buf is row-major (x fastest, then y, then z).
        assert_eq!(buf[idx(0, 1, 0)], 42);
        assert_eq!(buf[idx(1, 1, 0)], 0);
        assert_eq!(buf[idx(2, 1, 0)], 42);
        assert_eq!(buf[idx(0, 0, 0)], 0);
        assert_eq!(buf[idx(1, 0, 0)], 42);
        assert_eq!(buf[idx(2, 0, 0)], 0);
    }

    fn idx(x: usize, y: usize, z: usize) -> usize {
        // 3×2×1 buf
        (z * 2 + y) * 3 + x
    }
}
