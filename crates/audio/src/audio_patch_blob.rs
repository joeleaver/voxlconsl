//! Patch serialization blob format — see SPEC.md §5.7.
//!
//! Stage 6c (v0.1.16). Defines a stable little-endian byte layout
//! for the `Patch` struct so carts can round-trip patches through
//! their save block (§7). The blob is versioned with a 4-byte magic
//! + version byte; future format revisions can extend it
//! backwards-compatibly by tacking on a tail (older hosts ignore
//! trailing bytes, newer hosts read them).
//!
//! ## Layout (little-endian throughout)
//!
//! ```text
//! offset  size  field
//! 0       4     "VPCH" magic
//! 4       1     version (1)
//! 5       1     kind (0 = Synth, 1 = Sampler)
//! 6       1     reserved (0)
//! 7       1     osc[0].mode
//! 8       2     osc[0].detune_cents (i16)
//! 10      1     osc[0].octave (i8)
//! 11      1     osc[0].level
//! 12      1     osc[1].mode
//! 13      2     osc[1].detune_cents
//! 15      1     osc[1].octave
//! 16      1     osc[1].level
//! 17      1     filter.mode
//! 18      2     filter.cutoff_hz
//! 20      1     filter.resonance
//! 21      2     amp_env.attack_ms
//! 23      2     amp_env.decay_ms
//! 25      1     amp_env.sustain
//! 26      2     amp_env.release_ms
//! 28      2     filter_env.attack_ms
//! 30      2     filter_env.decay_ms
//! 32      1     filter_env.sustain
//! 33      2     filter_env.release_ms
//! 35      1     filter_env_depth (i8)
//! 36      2     lfo.rate_centihz
//! 38      1     lfo.shape
//! 39      1     lfo.target
//! 40      1     lfo.depth (i8)
//! 41      2     glide_ms
//! 43      2     fm_ratio (Q8.8)
//! 45      2     fm_index (Q8.8)
//! 47      1     zone_count (0..=8)
//! 48      14×N  zones[0..N], each:
//!                 +0  low_note
//!                 +1  high_note
//!                 +2  root_note
//!                 +3  sample_slot
//!                 +4  volume_offset (i8)
//!                 +5  loop_start (u32)
//!                 +9  loop_end (u32)
//!                 +13 loop_enabled (0/1)
//! ```
//!
//! Total size: 48 bytes (synth, zone_count = 0) up to 160 bytes
//! (sampler with 8 zones).

use crate::audio::{
    EnvParams, FilterMode, FilterParams, KeyZone, LfoParams, LfoShape, LfoTarget, OscMode,
    OscParams, Patch, PatchKind,
};

const MAGIC: &[u8; 4] = b"VPCH";
const VERSION: u8 = 1;

/// Fixed-prefix size in bytes — everything except the zone tail.
pub const PATCH_HEADER_BYTES: usize = 48;

/// Per-zone size in bytes.
pub const PATCH_ZONE_BYTES: usize = 14;

/// Maximum blob size: header + 8 fully-populated zones.
pub const PATCH_BLOB_MAX: usize = PATCH_HEADER_BYTES + PATCH_ZONE_BYTES * 8;

/// Serialize `patch` into `out`. Writes header + `patch.zone_count`
/// zone entries. Returns the number of bytes written, or 0 if `out`
/// is too small.
pub fn save(patch: &Patch, out: &mut [u8]) -> u32 {
    let zones_to_write = (patch.zone_count as usize).min(patch.zones.len());
    let needed = PATCH_HEADER_BYTES + zones_to_write * PATCH_ZONE_BYTES;
    if out.len() < needed {
        return 0;
    }
    out[0..4].copy_from_slice(MAGIC);
    out[4] = VERSION;
    out[5] = patch_kind_code(patch.kind);
    out[6] = 0;
    write_osc(&mut out[7..12], &patch.osc[0]);
    write_osc(&mut out[12..17], &patch.osc[1]);
    write_filter(&mut out[17..21], &patch.filter);
    write_env(&mut out[21..28], &patch.amp_env);
    write_env(&mut out[28..35], &patch.filter_env);
    out[35] = patch.filter_env_depth as u8;
    write_lfo(&mut out[36..41], &patch.lfo);
    out[41..43].copy_from_slice(&patch.glide_ms.to_le_bytes());
    out[43..45].copy_from_slice(&patch.fm_ratio.to_le_bytes());
    out[45..47].copy_from_slice(&patch.fm_index.to_le_bytes());
    out[47] = zones_to_write as u8;

    let mut off = PATCH_HEADER_BYTES;
    for i in 0..zones_to_write {
        write_zone(&mut out[off..off + PATCH_ZONE_BYTES], &patch.zones[i]);
        off += PATCH_ZONE_BYTES;
    }
    off as u32
}

/// Parse `src` into a Patch. Returns None on bad magic, version
/// mismatch, truncated input, or unrecognized enum codes.
pub fn load(src: &[u8]) -> Option<Patch> {
    if src.len() < PATCH_HEADER_BYTES {
        return None;
    }
    if &src[0..4] != MAGIC {
        return None;
    }
    if src[4] != VERSION {
        return None;
    }
    let kind = match src[5] {
        0 => PatchKind::Synth,
        1 => PatchKind::Sampler,
        _ => return None,
    };

    let osc0 = read_osc(&src[7..12]);
    let osc1 = read_osc(&src[12..17]);
    let filter = read_filter(&src[17..21]);
    let amp_env = read_env(&src[21..28]);
    let filter_env = read_env(&src[28..35]);
    let filter_env_depth = src[35] as i8;
    let lfo = read_lfo(&src[36..41]);
    let glide_ms = u16::from_le_bytes([src[41], src[42]]);
    let fm_ratio = u16::from_le_bytes([src[43], src[44]]);
    let fm_index = u16::from_le_bytes([src[45], src[46]]);
    let zone_count_field = src[47].min(8);

    let need = PATCH_HEADER_BYTES + (zone_count_field as usize) * PATCH_ZONE_BYTES;
    if src.len() < need {
        return None;
    }

    let mut zones = [KeyZone::empty(); 8];
    for i in 0..(zone_count_field as usize) {
        let base = PATCH_HEADER_BYTES + i * PATCH_ZONE_BYTES;
        zones[i] = read_zone(&src[base..base + PATCH_ZONE_BYTES]);
    }

    Some(Patch {
        kind,
        osc: [osc0, osc1],
        filter,
        amp_env,
        filter_env,
        filter_env_depth,
        lfo,
        glide_ms,
        fm_ratio,
        fm_index,
        zones,
        zone_count: zone_count_field,
    })
}

// ── Code conversions (reverse of from_code()) ─────────────────────

fn patch_kind_code(k: PatchKind) -> u8 {
    match k {
        PatchKind::Synth => 0,
        PatchKind::Sampler => 1,
    }
}

fn osc_mode_code(m: OscMode) -> u8 {
    match m {
        OscMode::Sine => 0,
        OscMode::Saw => 1,
        OscMode::Square => 2,
        OscMode::Triangle => 3,
        OscMode::Noise => 4,
        OscMode::Fm2Op => 5,
    }
}

fn filter_mode_code(m: FilterMode) -> u8 {
    match m {
        FilterMode::Off => 0,
        FilterMode::LowPass => 1,
        FilterMode::HighPass => 2,
        FilterMode::BandPass => 3,
    }
}

fn lfo_shape_code(s: LfoShape) -> u8 {
    match s {
        LfoShape::Sine => 0,
        LfoShape::Triangle => 1,
        LfoShape::Square => 2,
        LfoShape::SampleAndHold => 3,
    }
}

fn lfo_target_code(t: LfoTarget) -> u8 {
    match t {
        LfoTarget::Pitch => 0,
        LfoTarget::Filter => 1,
        LfoTarget::Amp => 2,
        LfoTarget::Pan => 3,
    }
}

// ── Field-level helpers ─────────────────────────────────────────────

fn write_osc(buf: &mut [u8], o: &OscParams) {
    buf[0] = osc_mode_code(o.mode);
    buf[1..3].copy_from_slice(&o.detune_cents.to_le_bytes());
    buf[3] = o.octave as u8;
    buf[4] = o.level;
}

fn read_osc(buf: &[u8]) -> OscParams {
    OscParams {
        mode: OscMode::from_code(buf[0]),
        detune_cents: i16::from_le_bytes([buf[1], buf[2]]),
        octave: buf[3] as i8,
        level: buf[4],
    }
}

fn write_filter(buf: &mut [u8], f: &FilterParams) {
    buf[0] = filter_mode_code(f.mode);
    buf[1..3].copy_from_slice(&f.cutoff_hz.to_le_bytes());
    buf[3] = f.resonance;
}

fn read_filter(buf: &[u8]) -> FilterParams {
    FilterParams {
        mode: FilterMode::from_code(buf[0]),
        cutoff_hz: u16::from_le_bytes([buf[1], buf[2]]),
        resonance: buf[3],
    }
}

fn write_env(buf: &mut [u8], e: &EnvParams) {
    buf[0..2].copy_from_slice(&e.attack_ms.to_le_bytes());
    buf[2..4].copy_from_slice(&e.decay_ms.to_le_bytes());
    buf[4] = e.sustain;
    buf[5..7].copy_from_slice(&e.release_ms.to_le_bytes());
}

fn read_env(buf: &[u8]) -> EnvParams {
    EnvParams {
        attack_ms: u16::from_le_bytes([buf[0], buf[1]]),
        decay_ms: u16::from_le_bytes([buf[2], buf[3]]),
        sustain: buf[4],
        release_ms: u16::from_le_bytes([buf[5], buf[6]]),
    }
}

fn write_lfo(buf: &mut [u8], l: &LfoParams) {
    buf[0..2].copy_from_slice(&l.rate_centihz.to_le_bytes());
    buf[2] = lfo_shape_code(l.shape);
    buf[3] = lfo_target_code(l.target);
    buf[4] = l.depth as u8;
}

fn read_lfo(buf: &[u8]) -> LfoParams {
    LfoParams {
        rate_centihz: u16::from_le_bytes([buf[0], buf[1]]),
        shape: LfoShape::from_code(buf[2]),
        target: LfoTarget::from_code(buf[3]),
        depth: buf[4] as i8,
    }
}

fn write_zone(buf: &mut [u8], z: &KeyZone) {
    buf[0] = z.low_note;
    buf[1] = z.high_note;
    buf[2] = z.root_note;
    buf[3] = z.sample_slot;
    buf[4] = z.volume_offset as u8;
    buf[5..9].copy_from_slice(&z.loop_start.to_le_bytes());
    buf[9..13].copy_from_slice(&z.loop_end.to_le_bytes());
    buf[13] = if z.loop_enabled { 1 } else { 0 };
}

fn read_zone(buf: &[u8]) -> KeyZone {
    KeyZone {
        low_note: buf[0],
        high_note: buf[1],
        root_note: buf[2],
        sample_slot: buf[3],
        volume_offset: buf[4] as i8,
        loop_start: u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]),
        loop_end: u32::from_le_bytes([buf[9], buf[10], buf[11], buf[12]]),
        loop_enabled: buf[13] != 0,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioState;

    #[test]
    fn synth_patch_round_trip() {
        let mut a = AudioState::new_silent();
        a.patch_set_osc(3, 0, OscMode::Saw, /*detune*/-7, /*octave*/1, /*level*/110);
        a.patch_set_osc(3, 1, OscMode::Square, /*detune*/12, /*octave*/0, /*level*/64);
        a.patch_set_filter(3, FilterMode::HighPass, 1500, 80);
        a.patch_set_amp_env(3, 12, 80, 100, 250);
        a.patch_set_filter_env(3, 5, 200, 30, 150, -50);
        a.patch_set_lfo(3, 750, LfoShape::Triangle, LfoTarget::Filter, 30);
        a.patch_set_glide(3, 40);
        a.patch_set_fm(3, 768, 1500);

        let mut buf = [0u8; PATCH_BLOB_MAX];
        let n = a.patch_save(3, &mut buf);
        assert!(n > 0);
        assert_eq!(n as usize, PATCH_HEADER_BYTES);  // synth = no zones

        // Wipe patch 3 to a known-clean state, then reload from the blob.
        a.patch_reset(3);
        assert!(a.patch_load(3, &buf[..n as usize]));

        // Spot-check a few fields end-to-end.
        let patches = a.patches_for_test();
        let p = &patches[3];
        assert_eq!(p.kind, PatchKind::Synth);
        assert_eq!(p.osc[0].mode, OscMode::Saw);
        assert_eq!(p.osc[0].detune_cents, -7);
        assert_eq!(p.osc[0].octave, 1);
        assert_eq!(p.osc[0].level, 110);
        assert_eq!(p.osc[1].mode, OscMode::Square);
        assert_eq!(p.osc[1].detune_cents, 12);
        assert_eq!(p.filter.mode, FilterMode::HighPass);
        assert_eq!(p.filter.cutoff_hz, 1500);
        assert_eq!(p.filter.resonance, 80);
        assert_eq!(p.amp_env.attack_ms, 12);
        assert_eq!(p.amp_env.release_ms, 250);
        assert_eq!(p.filter_env_depth, -50);
        assert_eq!(p.lfo.shape, LfoShape::Triangle);
        assert_eq!(p.lfo.target, LfoTarget::Filter);
        assert_eq!(p.glide_ms, 40);
        assert_eq!(p.fm_ratio, 768);
        assert_eq!(p.fm_index, 1500);
        assert_eq!(p.zone_count, 0);
    }

    #[test]
    fn sampler_patch_round_trip_with_zones() {
        let mut a = AudioState::new_silent();
        a.patch_set_kind(5, PatchKind::Sampler);
        a.patch_set_zone(5, 0, KeyZone {
            low_note: 36, high_note: 59, root_note: 48,
            sample_slot: 2, volume_offset: -10,
            loop_start: 100, loop_end: 4000, loop_enabled: true,
        });
        a.patch_set_zone(5, 1, KeyZone {
            low_note: 60, high_note: 96, root_note: 72,
            sample_slot: 3, volume_offset: 5,
            loop_start: 0, loop_end: 0, loop_enabled: false,
        });
        a.patch_set_zone_count(5, 2);

        let mut buf = [0u8; PATCH_BLOB_MAX];
        let n = a.patch_save(5, &mut buf);
        assert_eq!(n as usize, PATCH_HEADER_BYTES + 2 * PATCH_ZONE_BYTES);

        a.patch_reset(5);
        assert!(a.patch_load(5, &buf[..n as usize]));

        let patches = a.patches_for_test();
        let p = &patches[5];
        assert_eq!(p.kind, PatchKind::Sampler);
        assert_eq!(p.zone_count, 2);
        assert_eq!(p.zones[0].low_note, 36);
        assert_eq!(p.zones[0].high_note, 59);
        assert_eq!(p.zones[0].root_note, 48);
        assert_eq!(p.zones[0].volume_offset, -10);
        assert_eq!(p.zones[0].loop_start, 100);
        assert_eq!(p.zones[0].loop_end, 4000);
        assert!(p.zones[0].loop_enabled);
        assert_eq!(p.zones[1].root_note, 72);
        assert_eq!(p.zones[1].volume_offset, 5);
        assert!(!p.zones[1].loop_enabled);
    }

    #[test]
    fn load_rejects_bad_magic() {
        let mut a = AudioState::new_silent();
        let mut buf = [0u8; PATCH_HEADER_BYTES];
        let n = a.patch_save(0, &mut buf);
        assert!(n > 0);
        buf[0] = b'X';  // corrupt magic
        assert!(!a.patch_load(0, &buf));
    }

    #[test]
    fn load_rejects_wrong_version() {
        let mut a = AudioState::new_silent();
        let mut buf = [0u8; PATCH_HEADER_BYTES];
        a.patch_save(0, &mut buf);
        buf[4] = 99;  // unsupported version
        assert!(!a.patch_load(0, &buf));
    }

    #[test]
    fn load_rejects_truncated_blob() {
        let mut a = AudioState::new_silent();
        let mut buf = [0u8; PATCH_HEADER_BYTES];
        let n = a.patch_save(0, &mut buf) as usize;
        assert!(!a.patch_load(0, &buf[..n - 1]));
    }

    #[test]
    fn save_into_too_small_buffer_returns_zero() {
        let a = AudioState::new_silent();
        let mut buf = [0u8; 4];  // far too small
        assert_eq!(a.patch_save(0, &mut buf), 0);
    }

    #[test]
    fn out_of_range_slot_returns_zero() {
        let a = AudioState::new_silent();
        let mut buf = [0u8; PATCH_BLOB_MAX];
        assert_eq!(a.patch_save(99, &mut buf), 0);
    }
}
