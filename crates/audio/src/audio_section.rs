//! `.voxl` Audio section binary format (SPEC.md §7 section id 4).
//!
//! Bundles the cart's authored audio assets — synth patches, sample
//! bank, MIDI songs — into one self-describing blob that the host
//! pre-populates into the audio state before `init()` runs.
//!
//! ## Layout (little-endian throughout)
//!
//! ```text
//! offset  size  field
//! 0       4     "VAUD" magic
//! 4       1     version (1)
//! 5       1     flags (0)
//! 6       2     entry_count
//! 8       16×N  entry table
//! ...           data blobs (offsets refer back to start of section)
//! ```
//!
//! Each entry record is 16 bytes:
//!
//! ```text
//! offset  size  field
//! 0       1     kind (0 = patch, 1 = sample, 2 = song)
//! 1       1     slot   (patch 0..15, sample 0..63, song 0..7)
//! 2       2     reserved (0)
//! 4       4     data_offset (from start of section)
//! 8       4     data_size
//! 12      4     reserved (0)
//! ```
//!
//! Data payloads are kind-specific:
//!
//! - **Patch** — VPCH blob bytes (see `audio_patch_blob`). 48–160 bytes.
//! - **Sample** — fixed 12-byte preamble followed by PCM:
//!     - `0..4`  : sample_rate_hz (u32; 11025 or 22050)
//!     - `4..8`  : loop_start (u32; `u32::MAX` = no loop)
//!     - `8..12` : loop_end (u32)
//!     - `12..`  : 8-bit unsigned PCM, mono
//! - **Song** — raw SMF bytes (type 0 or 1, see `smf::parse`).
//!
//! The parser is permissive: unknown kinds, out-of-range slots, and
//! malformed sample headers are silently dropped so future cart
//! authors can ship extra entries that older hosts simply ignore. The
//! header magic + version + size budget are strict.

use core::convert::TryInto;

pub const MAGIC: [u8; 4] = *b"VAUD";
pub const VERSION: u8 = 1;
pub const HEADER_SIZE: usize = 8;
pub const ENTRY_SIZE: usize = 16;
pub const SAMPLE_PREAMBLE_SIZE: usize = 12;
/// Sentinel written into the `loop_start` field when a sample has no
/// loop region. The host falls back to `loop_points: None`.
pub const NO_LOOP: u32 = u32::MAX;

/// Entry kind discriminants.
pub const KIND_PATCH: u8 = 0;
pub const KIND_SAMPLE: u8 = 1;
pub const KIND_SONG: u8 = 2;

/// A decoded entry pointing back into the section bytes.
#[derive(Copy, Clone, Debug)]
pub struct Entry<'a> {
    pub kind: u8,
    pub slot: u8,
    pub data: &'a [u8],
}

impl<'a> Entry<'a> {
    /// For a `KIND_SAMPLE` entry: decode the (rate, optional loop, PCM).
    /// Returns `None` if the payload is too short or otherwise malformed.
    pub fn as_sample(&self) -> Option<SampleView<'a>> {
        if self.kind != KIND_SAMPLE || self.data.len() < SAMPLE_PREAMBLE_SIZE {
            return None;
        }
        let rate = u32::from_le_bytes(self.data[0..4].try_into().unwrap());
        let loop_start = u32::from_le_bytes(self.data[4..8].try_into().unwrap());
        let loop_end = u32::from_le_bytes(self.data[8..12].try_into().unwrap());
        let pcm = &self.data[SAMPLE_PREAMBLE_SIZE..];
        let loop_points = if loop_start == NO_LOOP {
            None
        } else {
            Some((loop_start, loop_end))
        };
        Some(SampleView {
            sample_rate_hz: rate,
            loop_points,
            pcm,
        })
    }
}

#[derive(Copy, Clone, Debug)]
pub struct SampleView<'a> {
    pub sample_rate_hz: u32,
    pub loop_points: Option<(u32, u32)>,
    pub pcm: &'a [u8],
}

/// Iterate the entries in a parsed section.
pub fn entries(section: &[u8]) -> EntryIter<'_> {
    if section.len() < HEADER_SIZE
        || section[0..4] != MAGIC
        || section[4] != VERSION
    {
        return EntryIter::empty(section);
    }
    let entry_count = u16::from_le_bytes(section[6..8].try_into().unwrap()) as usize;
    EntryIter {
        section,
        remaining: entry_count,
        next_idx: 0,
    }
}

pub struct EntryIter<'a> {
    section: &'a [u8],
    remaining: usize,
    next_idx: usize,
}

impl<'a> EntryIter<'a> {
    fn empty(section: &'a [u8]) -> Self {
        Self { section, remaining: 0, next_idx: 0 }
    }
}

impl<'a> Iterator for EntryIter<'a> {
    type Item = Entry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.remaining > 0 {
            let idx = self.next_idx;
            self.remaining -= 1;
            self.next_idx += 1;
            let entry_off = HEADER_SIZE + idx * ENTRY_SIZE;
            if entry_off + ENTRY_SIZE > self.section.len() {
                return None;
            }
            let raw = &self.section[entry_off..entry_off + ENTRY_SIZE];
            let kind = raw[0];
            let slot = raw[1];
            let data_off = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
            let data_size = u32::from_le_bytes(raw[8..12].try_into().unwrap()) as usize;
            // Skip entries pointing past the end of the section rather
            // than panicking — keeps newer-cart-on-older-host forward
            // compatibility from blowing up on a malformed offset.
            if data_off.checked_add(data_size).map_or(true, |end| end > self.section.len()) {
                continue;
            }
            return Some(Entry {
                kind,
                slot,
                data: &self.section[data_off..data_off + data_size],
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(entries: &[(u8, u8, Vec<u8>)]) -> Vec<u8> {
        let count = entries.len();
        let mut out = vec![0u8; HEADER_SIZE + count * ENTRY_SIZE];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = VERSION;
        out[6..8].copy_from_slice(&(count as u16).to_le_bytes());
        let mut offsets = Vec::with_capacity(count);
        for (_, _, data) in entries {
            offsets.push(out.len());
            out.extend_from_slice(data);
        }
        for (i, ((kind, slot, data), data_off)) in entries.iter().zip(&offsets).enumerate() {
            let at = HEADER_SIZE + i * ENTRY_SIZE;
            out[at] = *kind;
            out[at + 1] = *slot;
            out[at + 4..at + 8].copy_from_slice(&(*data_off as u32).to_le_bytes());
            out[at + 8..at + 12].copy_from_slice(&(data.len() as u32).to_le_bytes());
        }
        out
    }

    #[test]
    fn empty_section_returns_no_entries() {
        let bytes = build(&[]);
        assert_eq!(entries(&bytes).count(), 0);
    }

    #[test]
    fn bad_magic_yields_empty_iter() {
        let mut bytes = build(&[(KIND_PATCH, 0, vec![1, 2, 3])]);
        bytes[0] = b'X';
        assert_eq!(entries(&bytes).count(), 0);
    }

    #[test]
    fn iterates_patch_sample_song() {
        let mut sample_payload = vec![0u8; SAMPLE_PREAMBLE_SIZE];
        sample_payload[0..4].copy_from_slice(&22_050u32.to_le_bytes());
        sample_payload[4..8].copy_from_slice(&NO_LOOP.to_le_bytes());
        sample_payload.extend_from_slice(&[100u8, 110, 120, 130]); // PCM
        let bytes = build(&[
            (KIND_PATCH, 5, b"VPCH-stub".to_vec()),
            (KIND_SAMPLE, 3, sample_payload),
            (KIND_SONG, 1, b"MThd-stub".to_vec()),
        ]);
        let collected: Vec<_> = entries(&bytes).collect();
        assert_eq!(collected.len(), 3);
        assert_eq!((collected[0].kind, collected[0].slot), (KIND_PATCH, 5));
        assert_eq!(collected[0].data, b"VPCH-stub");
        let sample = collected[1].as_sample().expect("sample");
        assert_eq!(sample.sample_rate_hz, 22_050);
        assert_eq!(sample.loop_points, None);
        assert_eq!(sample.pcm, &[100, 110, 120, 130]);
        assert_eq!((collected[2].kind, collected[2].slot), (KIND_SONG, 1));
        assert_eq!(collected[2].data, b"MThd-stub");
    }

    #[test]
    fn sample_with_loop_points() {
        let mut sample_payload = vec![0u8; SAMPLE_PREAMBLE_SIZE];
        sample_payload[0..4].copy_from_slice(&11_025u32.to_le_bytes());
        sample_payload[4..8].copy_from_slice(&64u32.to_le_bytes());
        sample_payload[8..12].copy_from_slice(&4032u32.to_le_bytes());
        sample_payload.extend(core::iter::repeat(128u8).take(4096));
        let bytes = build(&[(KIND_SAMPLE, 0, sample_payload)]);
        let sample = entries(&bytes).next().unwrap().as_sample().unwrap();
        assert_eq!(sample.sample_rate_hz, 11_025);
        assert_eq!(sample.loop_points, Some((64, 4032)));
        assert_eq!(sample.pcm.len(), 4096);
    }

    #[test]
    fn malformed_offset_is_skipped() {
        // Build a section with one entry whose data_offset points past
        // the end. The iterator should yield no entries (skipped).
        let mut bytes = vec![0u8; HEADER_SIZE + ENTRY_SIZE];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[4] = VERSION;
        bytes[6..8].copy_from_slice(&1u16.to_le_bytes());
        // Entry: data_offset = 99 (well past end)
        bytes[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&99u32.to_le_bytes());
        bytes[HEADER_SIZE + 8..HEADER_SIZE + 12].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(entries(&bytes).count(), 0);
    }
}
