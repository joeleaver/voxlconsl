//! `.voxl` cart format — see SPEC.md §7.
//!
//! A `.voxl` is the cart binary the host loads. v1 is intentionally
//! minimal: a 32-byte header carrying a section table, followed by the
//! sections themselves in any on-disk order. Only the Code section
//! (raw WASM) is required; everything else is optional and gets pulled
//! in as the corresponding subsystems land in the host.
//!
//! This module is pure parser + format constants. The bundler (write
//! side) lives in `voxlconsl-bundler`; the host (read side) calls
//! [`Cart::parse`] from its sandbox loader.

pub const MAGIC: [u8; 10] = *b"VOXLCONSL\0";
pub const VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 32;
pub const SECTION_ENTRY_SIZE: usize = 16;
pub const MAX_SECTIONS: usize = 16;
/// Hard cap on `.voxl` file size (§7).
pub const MAX_TOTAL_SIZE: u32 = 32 * 1024 * 1024;

/// Byte offset of the CRC-32 field inside the header. The CRC is
/// computed over the whole file with these 4 bytes zeroed.
pub const CRC_FIELD_OFFSET: usize = 20;

/// Well-known section ids (v1). Unknown ids in newer carts are
/// tolerated by the parser and returned via [`Cart::sections`].
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SectionId {
    Metadata   = 0,
    Code       = 1,
    Materials  = 2,
    World      = 3,
    Audio      = 4,
    SaveSchema = 5,
}

impl SectionId {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0 => Self::Metadata,
            1 => Self::Code,
            2 => Self::Materials,
            3 => Self::World,
            4 => Self::Audio,
            5 => Self::SaveSchema,
            _ => return None,
        })
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SectionEntry {
    pub id: u16,
    pub flags: u16,
    pub offset: u32,
    pub size: u32,
    pub uncompressed_size: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CartError {
    Truncated,
    BadMagic,
    UnsupportedVersion,
    UnsupportedFlags,
    BadSectionCount,
    BadTotalSize,
    BadCrc,
    SectionOutOfBounds,
    DuplicateSection,
    MissingCode,
}

/// Parsed view over a `.voxl` byte slice. Zero-copy: borrows the input.
pub struct Cart<'a> {
    bytes: &'a [u8],
    /// Table entries in the order they appeared on disk; `None` for the
    /// trailing slots up to [`MAX_SECTIONS`].
    sections: [Option<SectionEntry>; MAX_SECTIONS],
    section_count: usize,
}

impl<'a> Cart<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, CartError> {
        if bytes.len() < HEADER_SIZE {
            return Err(CartError::Truncated);
        }
        if bytes[..10] != MAGIC {
            return Err(CartError::BadMagic);
        }
        let version = u16::from_le_bytes([bytes[10], bytes[11]]);
        if version != VERSION {
            return Err(CartError::UnsupportedVersion);
        }
        let flags = u16::from_le_bytes([bytes[12], bytes[13]]);
        if flags != 0 {
            return Err(CartError::UnsupportedFlags);
        }
        let section_count = bytes[14] as usize;
        if section_count > MAX_SECTIONS {
            return Err(CartError::BadSectionCount);
        }
        let total_size = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        if total_size as usize != bytes.len() || total_size > MAX_TOTAL_SIZE {
            return Err(CartError::BadTotalSize);
        }
        let stored_crc = u32::from_le_bytes([
            bytes[CRC_FIELD_OFFSET],
            bytes[CRC_FIELD_OFFSET + 1],
            bytes[CRC_FIELD_OFFSET + 2],
            bytes[CRC_FIELD_OFFSET + 3],
        ]);

        let table_end = HEADER_SIZE + section_count * SECTION_ENTRY_SIZE;
        if bytes.len() < table_end {
            return Err(CartError::Truncated);
        }

        let mut sections: [Option<SectionEntry>; MAX_SECTIONS] = [None; MAX_SECTIONS];
        let mut seen_ids: u64 = 0;
        let mut has_code = false;
        for i in 0..section_count {
            let off = HEADER_SIZE + i * SECTION_ENTRY_SIZE;
            let id = u16::from_le_bytes([bytes[off], bytes[off + 1]]);
            let s_flags = u16::from_le_bytes([bytes[off + 2], bytes[off + 3]]);
            if s_flags != 0 {
                return Err(CartError::UnsupportedFlags);
            }
            let s_offset = u32::from_le_bytes([
                bytes[off + 4], bytes[off + 5], bytes[off + 6], bytes[off + 7],
            ]);
            let s_size = u32::from_le_bytes([
                bytes[off + 8], bytes[off + 9], bytes[off + 10], bytes[off + 11],
            ]);
            let s_uncompressed = u32::from_le_bytes([
                bytes[off + 12], bytes[off + 13], bytes[off + 14], bytes[off + 15],
            ]);

            // Section data must lie inside the file and not overlap the
            // header / table.
            let end = (s_offset as u64).saturating_add(s_size as u64);
            if end > bytes.len() as u64 || (s_offset as usize) < table_end {
                return Err(CartError::SectionOutOfBounds);
            }
            if id < 64 {
                let bit = 1u64 << id;
                if seen_ids & bit != 0 {
                    return Err(CartError::DuplicateSection);
                }
                seen_ids |= bit;
            }
            if id == SectionId::Code as u16 {
                has_code = true;
            }
            sections[i] = Some(SectionEntry {
                id,
                flags: s_flags,
                offset: s_offset,
                size: s_size,
                uncompressed_size: s_uncompressed,
            });
        }

        if !has_code {
            return Err(CartError::MissingCode);
        }

        let crc = crc32_with_zeroed_field(bytes, CRC_FIELD_OFFSET);
        if crc != stored_crc {
            return Err(CartError::BadCrc);
        }

        Ok(Self {
            bytes,
            sections,
            section_count,
        })
    }

    /// Iterate the section table in on-disk order.
    pub fn sections(&self) -> impl Iterator<Item = &SectionEntry> + '_ {
        self.sections[..self.section_count]
            .iter()
            .filter_map(|e| e.as_ref())
    }

    pub fn section_bytes(&self, id: SectionId) -> Option<&'a [u8]> {
        let id_u16 = id as u16;
        for entry in self.sections().filter(|e| e.id == id_u16) {
            let start = entry.offset as usize;
            let end = start + entry.size as usize;
            return Some(&self.bytes[start..end]);
        }
        None
    }

    /// Raw WASM module bytes from the Code section. Always present —
    /// `parse` rejects carts without a Code section.
    pub fn code(&self) -> &'a [u8] {
        // Code is validated in parse().
        self.section_bytes(SectionId::Code).unwrap_or(&[])
    }

    /// UTF-8 TOML metadata, or `None` if absent / not valid UTF-8.
    pub fn metadata_toml(&self) -> Option<&'a str> {
        self.section_bytes(SectionId::Metadata)
            .and_then(|b| core::str::from_utf8(b).ok())
    }
}

// ============================================================================
// CRC-32/ISO-HDLC (poly 0xEDB88320). Table-on-the-fly to avoid bringing in
// a 1 KB static. The cart-load path runs once per boot, so a few KB of bytes
// at boot is fine.
// ============================================================================

fn crc32_byte(crc: u32, byte: u8) -> u32 {
    let mut c = crc ^ byte as u32;
    let mut i = 0;
    while i < 8 {
        c = (c >> 1) ^ if c & 1 != 0 { 0xEDB88320 } else { 0 };
        i += 1;
    }
    c
}

/// CRC-32/ISO-HDLC over `bytes`.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    let mut i = 0;
    while i < bytes.len() {
        crc = crc32_byte(crc, bytes[i]);
        i += 1;
    }
    !crc
}

/// CRC-32 over `bytes` with the 4 bytes starting at `field_offset`
/// treated as zeros — used both at parse (validate) and at write time
/// (bundler computes this against the buffer with field = 0, then patches
/// the field with the result).
pub fn crc32_with_zeroed_field(bytes: &[u8], field_offset: usize) -> u32 {
    let mut crc = !0u32;
    let mut i = 0;
    let field_end = field_offset + 4;
    while i < bytes.len() {
        let b = if i >= field_offset && i < field_end { 0 } else { bytes[i] };
        crc = crc32_byte(crc, b);
        i += 1;
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid cart: just the header, a 1-section table, and a
    /// single-byte Code section. CRC computed below.
    fn build_minimal_cart(code: &[u8]) -> alloc::vec::Vec<u8> {
        extern crate alloc;
        let total = HEADER_SIZE + SECTION_ENTRY_SIZE + code.len();
        let mut buf = alloc::vec![0u8; total];
        buf[..10].copy_from_slice(&MAGIC);
        buf[10..12].copy_from_slice(&VERSION.to_le_bytes());
        buf[14] = 1; // section_count
        buf[16..20].copy_from_slice(&(total as u32).to_le_bytes());
        // section entry
        let off = HEADER_SIZE;
        buf[off..off + 2].copy_from_slice(&(SectionId::Code as u16).to_le_bytes());
        let data_off = HEADER_SIZE + SECTION_ENTRY_SIZE;
        buf[off + 4..off + 8].copy_from_slice(&(data_off as u32).to_le_bytes());
        buf[off + 8..off + 12].copy_from_slice(&(code.len() as u32).to_le_bytes());
        buf[off + 12..off + 16].copy_from_slice(&(code.len() as u32).to_le_bytes());
        // payload
        buf[data_off..data_off + code.len()].copy_from_slice(code);
        // CRC last
        let crc = crc32_with_zeroed_field(&buf, CRC_FIELD_OFFSET);
        buf[CRC_FIELD_OFFSET..CRC_FIELD_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        buf
    }

    extern crate alloc;

    #[test]
    fn parses_minimal_cart() {
        let cart_bytes = build_minimal_cart(b"\x00\x61\x73\x6dwasm-stub");
        let cart = Cart::parse(&cart_bytes).expect("parse");
        assert_eq!(cart.code(), b"\x00\x61\x73\x6dwasm-stub");
        assert!(cart.metadata_toml().is_none());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut cart_bytes = build_minimal_cart(b"x");
        cart_bytes[0] = b'X';
        assert_eq!(Cart::parse(&cart_bytes).err(), Some(CartError::BadMagic));
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut cart_bytes = build_minimal_cart(b"x");
        cart_bytes[10] = 99;
        // CRC will also be wrong now; test only the version path by
        // re-CRCing.
        let crc = crc32_with_zeroed_field(&cart_bytes, CRC_FIELD_OFFSET);
        cart_bytes[CRC_FIELD_OFFSET..CRC_FIELD_OFFSET + 4]
            .copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            Cart::parse(&cart_bytes).err(),
            Some(CartError::UnsupportedVersion)
        );
    }

    #[test]
    fn rejects_bad_total_size() {
        let mut cart_bytes = build_minimal_cart(b"x");
        // Lie about total_size; re-CRC.
        cart_bytes[16] = 0xff;
        let crc = crc32_with_zeroed_field(&cart_bytes, CRC_FIELD_OFFSET);
        cart_bytes[CRC_FIELD_OFFSET..CRC_FIELD_OFFSET + 4]
            .copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            Cart::parse(&cart_bytes).err(),
            Some(CartError::BadTotalSize)
        );
    }

    #[test]
    fn rejects_truncated() {
        let cart_bytes = build_minimal_cart(b"x");
        assert_eq!(
            Cart::parse(&cart_bytes[..10]).err(),
            Some(CartError::Truncated)
        );
    }

    #[test]
    fn rejects_missing_code() {
        // Hand-built cart with section_count=0 → no Code → reject.
        let total = HEADER_SIZE;
        let mut buf = alloc::vec![0u8; total];
        buf[..10].copy_from_slice(&MAGIC);
        buf[10..12].copy_from_slice(&VERSION.to_le_bytes());
        buf[14] = 0;
        buf[16..20].copy_from_slice(&(total as u32).to_le_bytes());
        let crc = crc32_with_zeroed_field(&buf, CRC_FIELD_OFFSET);
        buf[CRC_FIELD_OFFSET..CRC_FIELD_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(Cart::parse(&buf).err(), Some(CartError::MissingCode));
    }

    #[test]
    fn rejects_bad_crc() {
        let mut cart_bytes = build_minimal_cart(b"x");
        cart_bytes[CRC_FIELD_OFFSET] ^= 0xff;
        assert_eq!(Cart::parse(&cart_bytes).err(), Some(CartError::BadCrc));
    }

    #[test]
    fn crc32_known_vector() {
        // CRC-32/ISO-HDLC of "123456789" is 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }
}
