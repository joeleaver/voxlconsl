//! Audio asset bundling: patches.toml + samples/ + songs/ → Audio section.
//!
//! Walks the cart's audio source tree, parses each asset, and packs
//! everything into the binary layout defined by
//! `voxlconsl_audio::audio_section`.
//!
//! Sample name resolution (SPEC.md §12.6.3): samples are indexed by
//! sample slot in lex-sorted-filename order. Sampler patches reference
//! samples by basename (no extension) and the bundler resolves those
//! names to slots before encoding patch blobs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use voxlconsl_audio::audio_section::{
    self, ENTRY_SIZE, HEADER_SIZE, KIND_PATCH, KIND_SAMPLE, KIND_SONG, MAGIC, NO_LOOP,
    SAMPLE_PREAMBLE_SIZE, VERSION,
};
use voxlconsl_audio::{
    patch_blob_save, EnvParams, FilterMode, FilterParams, KeyZone, LfoParams, LfoShape,
    LfoTarget, OscMode, OscParams, Patch, PatchKind, PATCH_BLOB_MAX, PATCH_SLOTS,
    SAMPLE_SLOTS, SONG_SLOTS,
};

use crate::wav::WavPcm;
use crate::BundleError;

/// Cart-toml `[audio]` block.
#[derive(Debug, serde::Deserialize, Default)]
pub struct AudioManifest {
    /// Path (relative to project_dir) of the `patches.toml` declaring
    /// up to 16 patch entries.
    #[serde(default)]
    pub patches: Option<PathBuf>,
    /// Directory holding `*.mid` SMF files. All are bundled in lex
    /// order into song slots 0..n.
    #[serde(default)]
    pub songs: Option<PathBuf>,
    /// Directory holding `*.wav` PCM files. All are bundled in lex
    /// order into sample slots 0..n. Sampler patches reference these
    /// by basename.
    #[serde(default)]
    pub samples: Option<PathBuf>,
}

/// Read the audio sources and produce the encoded `.voxl` Audio
/// section payload. Returns `None` when the manifest has no audio
/// directives at all (nothing to bundle).
pub fn build_audio_section(
    project_dir: &Path,
    manifest: &AudioManifest,
) -> Result<Option<Vec<u8>>, BundleError> {
    if manifest.patches.is_none() && manifest.songs.is_none() && manifest.samples.is_none() {
        return Ok(None);
    }

    // ── Samples ─────────────────────────────────────────────────────
    let mut samples: Vec<LoadedSample> = Vec::new();
    let mut sample_name_to_slot: BTreeMap<String, u8> = BTreeMap::new();
    if let Some(dir) = &manifest.samples {
        let dir = project_dir.join(dir);
        let files = list_dir_with_ext(&dir, "wav")?;
        if files.len() > SAMPLE_SLOTS {
            return Err(BundleError::Asset(format!(
                "audio/samples: {} .wav files, but only {} sample slots available",
                files.len(),
                SAMPLE_SLOTS
            )));
        }
        for (slot, path) in files.iter().enumerate() {
            let bytes = std::fs::read(path).map_err(|e| {
                BundleError::AssetIo(format!("read {}: {e}", path.display()))
            })?;
            let pcm = crate::wav::parse(&bytes).map_err(|e| {
                BundleError::AssetParse(format!("{}: {e}", path.display()))
            })?;
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| {
                    BundleError::Asset(format!("non-UTF-8 sample name: {}", path.display()))
                })?
                .to_string();
            sample_name_to_slot.insert(name, slot as u8);
            samples.push(LoadedSample { pcm });
        }
    }

    // ── Patches ─────────────────────────────────────────────────────
    let mut patch_blobs: Vec<(u8, Vec<u8>)> = Vec::new();
    if let Some(path) = &manifest.patches {
        let path = project_dir.join(path);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            BundleError::AssetIo(format!("read {}: {e}", path.display()))
        })?;
        let parsed: PatchesToml = toml::from_str(&text).map_err(|e| {
            BundleError::AssetParse(format!("{}: {e}", path.display()))
        })?;
        let mut seen_slots = [false; PATCH_SLOTS];
        for entry in &parsed.patch {
            if (entry.slot as usize) >= PATCH_SLOTS {
                return Err(BundleError::Asset(format!(
                    "patches.toml: slot {} out of range (0..{})",
                    entry.slot,
                    PATCH_SLOTS - 1
                )));
            }
            if seen_slots[entry.slot as usize] {
                return Err(BundleError::Asset(format!(
                    "patches.toml: slot {} defined twice",
                    entry.slot
                )));
            }
            seen_slots[entry.slot as usize] = true;

            let patch = build_patch(entry, &sample_name_to_slot, &samples)?;
            let mut buf = [0u8; PATCH_BLOB_MAX];
            let n = patch_blob_save(&patch, &mut buf) as usize;
            if n == 0 {
                return Err(BundleError::Asset(format!(
                    "patches.toml: patch slot {} failed to serialize",
                    entry.slot
                )));
            }
            patch_blobs.push((entry.slot, buf[..n].to_vec()));
        }
    }

    // ── Songs ───────────────────────────────────────────────────────
    let mut song_blobs: Vec<(u8, Vec<u8>)> = Vec::new();
    if let Some(dir) = &manifest.songs {
        let dir = project_dir.join(dir);
        let files = list_dir_with_ext(&dir, "mid")?;
        if files.len() > SONG_SLOTS {
            return Err(BundleError::Asset(format!(
                "audio/songs: {} .mid files, but only {} song slots available",
                files.len(),
                SONG_SLOTS
            )));
        }
        for (slot, path) in files.iter().enumerate() {
            let bytes = std::fs::read(path).map_err(|e| {
                BundleError::AssetIo(format!("read {}: {e}", path.display()))
            })?;
            // Parse-validate so we fail at bundle time on bad SMF.
            voxlconsl_audio::parse_smf(&bytes).map_err(|e| {
                BundleError::AssetParse(format!("{}: invalid SMF: {e:?}", path.display()))
            })?;
            song_blobs.push((slot as u8, bytes));
        }
    }

    Ok(Some(encode_section(&patch_blobs, &samples, &song_blobs)))
}

struct LoadedSample {
    pcm: WavPcm,
}

fn encode_section(
    patches: &[(u8, Vec<u8>)],
    samples: &[LoadedSample],
    songs: &[(u8, Vec<u8>)],
) -> Vec<u8> {
    let entry_count = patches.len() + samples.len() + songs.len();
    let mut out = vec![0u8; HEADER_SIZE + entry_count * ENTRY_SIZE];

    // Header
    out[0..4].copy_from_slice(&MAGIC);
    out[4] = VERSION;
    // flags=0, padding zero-init
    out[6..8].copy_from_slice(&(entry_count as u16).to_le_bytes());

    let mut entry_idx = 0usize;
    let put_entry = |out: &mut Vec<u8>,
                     kind: u8,
                     slot: u8,
                     payload: &[u8],
                     entry_idx: &mut usize| {
        let data_offset = out.len();
        out.extend_from_slice(payload);
        let at = HEADER_SIZE + *entry_idx * ENTRY_SIZE;
        out[at] = kind;
        out[at + 1] = slot;
        out[at + 4..at + 8].copy_from_slice(&(data_offset as u32).to_le_bytes());
        out[at + 8..at + 12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        *entry_idx += 1;
    };

    for (slot, blob) in patches {
        put_entry(&mut out, KIND_PATCH, *slot, blob, &mut entry_idx);
    }
    for (slot, sample) in samples.iter().enumerate() {
        let mut payload = Vec::with_capacity(SAMPLE_PREAMBLE_SIZE + sample.pcm.pcm.len());
        payload.extend_from_slice(&sample.pcm.sample_rate_hz.to_le_bytes());
        let (lo, hi) = match sample.pcm.loop_points {
            Some((s, e)) => (s, e),
            None => (NO_LOOP, 0),
        };
        payload.extend_from_slice(&lo.to_le_bytes());
        payload.extend_from_slice(&hi.to_le_bytes());
        payload.extend_from_slice(&sample.pcm.pcm);
        put_entry(&mut out, KIND_SAMPLE, slot as u8, &payload, &mut entry_idx);
    }
    for (slot, blob) in songs {
        put_entry(&mut out, KIND_SONG, *slot, blob, &mut entry_idx);
    }
    let _ = audio_section::entries; // silence unused-import warning in some build configs
    out
}

fn list_dir_with_ext(dir: &Path, ext: &str) -> Result<Vec<PathBuf>, BundleError> {
    if !dir.exists() {
        return Err(BundleError::Asset(format!(
            "audio directory not found: {}",
            dir.display()
        )));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| BundleError::AssetIo(format!("read_dir {}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some(ext))
        .collect();
    files.sort();
    Ok(files)
}

// ── patches.toml schema ────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct PatchesToml {
    #[serde(default)]
    patch: Vec<PatchEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct PatchEntry {
    slot: u8,
    #[serde(default)]
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>, // "synth" | "sampler"; defaults to synth
    #[serde(default)]
    osc1: Option<OscToml>,
    #[serde(default)]
    osc2: Option<OscToml>,
    #[serde(default)]
    fm: Option<FmToml>,
    #[serde(default)]
    filter: Option<FilterToml>,
    #[serde(default)]
    amp_env: Option<EnvToml>,
    #[serde(default)]
    filter_env: Option<FilterEnvToml>,
    #[serde(default)]
    lfo: Option<LfoToml>,
    #[serde(default)]
    glide: Option<GlideToml>,
    #[serde(default)]
    zone: Vec<ZoneToml>,
}

#[derive(Debug, serde::Deserialize)]
struct OscToml {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    detune_cents: i16,
    #[serde(default)]
    octave: i8,
    #[serde(default)]
    level: Option<u8>,
}

#[derive(Debug, serde::Deserialize)]
struct FmToml {
    #[serde(default)]
    ratio: Option<f32>,
    #[serde(default)]
    index: Option<f32>,
}

#[derive(Debug, serde::Deserialize)]
struct FilterToml {
    #[serde(default)]
    mode: Option<String>, // "lp" | "hp" | "bp" | "off"
    #[serde(default)]
    cutoff: Option<u16>,
    #[serde(default)]
    resonance: u8,
}

#[derive(Debug, serde::Deserialize)]
struct EnvToml {
    #[serde(default)]
    attack_ms: u16,
    #[serde(default)]
    decay_ms: u16,
    #[serde(default)]
    sustain: u8,
    #[serde(default)]
    release_ms: u16,
}

#[derive(Debug, serde::Deserialize)]
struct FilterEnvToml {
    #[serde(default)]
    attack_ms: u16,
    #[serde(default)]
    decay_ms: u16,
    #[serde(default)]
    sustain: u8,
    #[serde(default)]
    release_ms: u16,
    #[serde(default)]
    depth: i8,
}

#[derive(Debug, serde::Deserialize)]
struct LfoToml {
    #[serde(default)]
    rate_hz: Option<f32>,
    #[serde(default)]
    rate_centihz: Option<u16>,
    #[serde(default)]
    shape: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    depth: i8,
}

#[derive(Debug, serde::Deserialize)]
struct GlideToml {
    #[serde(default)]
    ms: u16,
}

#[derive(Debug, serde::Deserialize)]
struct ZoneToml {
    sample: String,
    low_note: u8,
    high_note: u8,
    root_note: u8,
    #[serde(default)]
    volume_offset: i8,
    /// When `true`, the zone loops over the sample's declared loop
    /// region. If the source `.wav` has no `smpl` chunk, the bundler
    /// falls back to `(0, pcm.len())`.
    #[serde(default, rename = "loop")]
    loop_: bool,
}

fn build_patch(
    entry: &PatchEntry,
    sample_names: &BTreeMap<String, u8>,
    samples: &[LoadedSample],
) -> Result<Patch, BundleError> {
    let mut patch = Patch::default_synth();
    let kind = match entry.kind.as_deref().unwrap_or("synth") {
        "synth" => PatchKind::Synth,
        "sampler" => PatchKind::Sampler,
        other => {
            return Err(BundleError::Asset(format!(
                "patches.toml: slot {} unknown kind {other:?}",
                entry.slot
            )));
        }
    };
    patch.kind = kind;

    if let Some(o) = &entry.osc1 {
        patch.osc[0] = decode_osc(o, entry.slot, "osc1")?;
    }
    if let Some(o) = &entry.osc2 {
        patch.osc[1] = decode_osc(o, entry.slot, "osc2")?;
    }
    if let Some(fm) = &entry.fm {
        if let Some(r) = fm.ratio {
            patch.fm_ratio = (r * 256.0).clamp(0.0, 65535.0) as u16;
        }
        if let Some(i) = fm.index {
            patch.fm_index = (i * 256.0).clamp(0.0, 65535.0) as u16;
        }
    }
    if let Some(f) = &entry.filter {
        patch.filter = decode_filter(f, entry.slot)?;
    }
    if let Some(e) = &entry.amp_env {
        patch.amp_env = decode_env(e);
    }
    if let Some(e) = &entry.filter_env {
        patch.filter_env = EnvParams {
            attack_ms: e.attack_ms,
            decay_ms: e.decay_ms,
            sustain: e.sustain,
            release_ms: e.release_ms,
        };
        patch.filter_env_depth = e.depth;
    }
    if let Some(l) = &entry.lfo {
        patch.lfo = decode_lfo(l, entry.slot)?;
    }
    if let Some(g) = &entry.glide {
        patch.glide_ms = g.ms;
    }

    if !entry.zone.is_empty() {
        if entry.zone.len() > 8 {
            return Err(BundleError::Asset(format!(
                "patches.toml: slot {} has {} zones (max 8)",
                entry.slot,
                entry.zone.len()
            )));
        }
        for (i, z) in entry.zone.iter().enumerate() {
            let sample_slot = *sample_names.get(&z.sample).ok_or_else(|| {
                BundleError::Asset(format!(
                    "patches.toml: slot {} zone {} sample {:?} not found in audio/samples/",
                    entry.slot, i, z.sample
                ))
            })?;
            let sample = &samples[sample_slot as usize];
            let (loop_start, loop_end, loop_enabled) = if z.loop_ {
                let (s, e) = sample
                    .pcm
                    .loop_points
                    .unwrap_or((0, sample.pcm.pcm.len() as u32));
                (s, e, true)
            } else {
                (0, 0, false)
            };
            patch.zones[i] = KeyZone {
                low_note: z.low_note,
                high_note: z.high_note,
                root_note: z.root_note,
                sample_slot,
                volume_offset: z.volume_offset,
                loop_start,
                loop_end,
                loop_enabled,
            };
        }
        patch.zone_count = entry.zone.len() as u8;
    }
    Ok(patch)
}

fn decode_osc(o: &OscToml, slot: u8, which: &str) -> Result<OscParams, BundleError> {
    let mode = match o.mode.as_deref().unwrap_or("sine") {
        "sine" => OscMode::Sine,
        "saw" => OscMode::Saw,
        "square" | "square_pwm" => OscMode::Square,
        "triangle" | "tri" => OscMode::Triangle,
        "noise" => OscMode::Noise,
        "fm2op" => OscMode::Fm2Op,
        other => {
            return Err(BundleError::Asset(format!(
                "patches.toml: slot {slot} {which} unknown mode {other:?}"
            )));
        }
    };
    Ok(OscParams {
        mode,
        detune_cents: o.detune_cents,
        octave: o.octave,
        level: o.level.unwrap_or(100),
    })
}

fn decode_filter(f: &FilterToml, slot: u8) -> Result<FilterParams, BundleError> {
    let mode = match f.mode.as_deref().unwrap_or("off") {
        "off" => FilterMode::Off,
        "lp" => FilterMode::LowPass,
        "hp" => FilterMode::HighPass,
        "bp" => FilterMode::BandPass,
        other => {
            return Err(BundleError::Asset(format!(
                "patches.toml: slot {slot} filter unknown mode {other:?}"
            )));
        }
    };
    Ok(FilterParams {
        mode,
        cutoff_hz: f.cutoff.unwrap_or(8000),
        resonance: f.resonance,
    })
}

fn decode_env(e: &EnvToml) -> EnvParams {
    EnvParams {
        attack_ms: e.attack_ms,
        decay_ms: e.decay_ms,
        sustain: e.sustain,
        release_ms: e.release_ms,
    }
}

fn decode_lfo(l: &LfoToml, slot: u8) -> Result<LfoParams, BundleError> {
    let rate_centihz = if let Some(c) = l.rate_centihz {
        c
    } else if let Some(hz) = l.rate_hz {
        (hz * 100.0).clamp(0.0, 65535.0) as u16
    } else {
        0
    };
    let shape = match l.shape.as_deref().unwrap_or("sine") {
        "sine" => LfoShape::Sine,
        "tri" | "triangle" => LfoShape::Triangle,
        "square" => LfoShape::Square,
        "sh" | "sample_and_hold" => LfoShape::SampleAndHold,
        other => {
            return Err(BundleError::Asset(format!(
                "patches.toml: slot {slot} lfo unknown shape {other:?}"
            )));
        }
    };
    let target = match l.target.as_deref().unwrap_or("pitch") {
        "pitch" => LfoTarget::Pitch,
        "filter" => LfoTarget::Filter,
        "amp" => LfoTarget::Amp,
        "pan" => LfoTarget::Pan,
        other => {
            return Err(BundleError::Asset(format!(
                "patches.toml: slot {slot} lfo unknown target {other:?}"
            )));
        }
    };
    Ok(LfoParams {
        rate_centihz,
        shape,
        target,
        depth: l.depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxlconsl_audio::patch_blob_load;

    #[test]
    fn synth_patch_round_trips_through_blob() {
        let toml_src = r#"
            [[patch]]
            slot = 0
            kind = "synth"

            [patch.osc1]
            mode = "saw"
            level = 110

            [patch.osc2]
            mode = "square"
            detune_cents = 7
            level = 80

            [patch.filter]
            mode = "lp"
            cutoff = 2500
            resonance = 30

            [patch.amp_env]
            attack_ms = 5
            decay_ms = 120
            sustain = 90
            release_ms = 220

            [patch.lfo]
            rate_hz = 6.0
            shape = "sine"
            target = "pitch"
            depth = 4
        "#;
        let parsed: PatchesToml = toml::from_str(toml_src).unwrap();
        let map = BTreeMap::new();
        let samples: Vec<LoadedSample> = Vec::new();
        let patch = build_patch(&parsed.patch[0], &map, &samples).unwrap();
        let mut buf = [0u8; PATCH_BLOB_MAX];
        let n = patch_blob_save(&patch, &mut buf) as usize;
        assert!(n > 0);
        let loaded = patch_blob_load(&buf[..n]).unwrap();
        assert_eq!(loaded.kind, PatchKind::Synth);
        assert_eq!(loaded.osc[0].mode, OscMode::Saw);
        assert_eq!(loaded.osc[1].mode, OscMode::Square);
        assert_eq!(loaded.osc[1].detune_cents, 7);
        assert_eq!(loaded.filter.mode, FilterMode::LowPass);
        assert_eq!(loaded.filter.cutoff_hz, 2500);
        assert_eq!(loaded.amp_env.attack_ms, 5);
        assert_eq!(loaded.lfo.rate_centihz, 600);
        assert_eq!(loaded.lfo.depth, 4);
    }

    #[test]
    fn sampler_patch_resolves_sample_names() {
        let toml_src = r#"
            [[patch]]
            slot = 3
            kind = "sampler"

            [[patch.zone]]
            sample = "beep"
            low_note = 36
            high_note = 96
            root_note = 60
            loop = true

            [patch.filter]
            mode = "lp"
            cutoff = 3200
            resonance = 20
        "#;
        let parsed: PatchesToml = toml::from_str(toml_src).unwrap();
        let mut map = BTreeMap::new();
        map.insert("beep".to_string(), 0u8);
        let sample = LoadedSample {
            pcm: WavPcm {
                sample_rate_hz: 22_050,
                pcm: vec![128u8; 4096],
                loop_points: Some((64, 4032)),
            },
        };
        let patch = build_patch(&parsed.patch[0], &map, &[sample]).unwrap();
        assert_eq!(patch.kind, PatchKind::Sampler);
        assert_eq!(patch.zone_count, 1);
        assert_eq!(patch.zones[0].sample_slot, 0);
        assert_eq!(patch.zones[0].low_note, 36);
        assert_eq!(patch.zones[0].root_note, 60);
        assert_eq!(patch.zones[0].loop_start, 64);
        assert_eq!(patch.zones[0].loop_end, 4032);
        assert!(patch.zones[0].loop_enabled);
    }

    #[test]
    fn missing_sample_name_rejected() {
        let toml_src = r#"
            [[patch]]
            slot = 0
            kind = "sampler"
            [[patch.zone]]
            sample = "nonexistent"
            low_note = 0
            high_note = 127
            root_note = 60
        "#;
        let parsed: PatchesToml = toml::from_str(toml_src).unwrap();
        let err = build_patch(&parsed.patch[0], &BTreeMap::new(), &[]).unwrap_err();
        assert!(format!("{err}").contains("nonexistent"));
    }

    #[test]
    fn fm2op_ratio_index_packed_as_q88() {
        let toml_src = r#"
            [[patch]]
            slot = 1

            [patch.osc1]
            mode = "fm2op"
            level = 127

            [patch.osc2]
            mode = "sine"
            level = 127

            [patch.fm]
            ratio = 1.5
            index = 3.0
        "#;
        let parsed: PatchesToml = toml::from_str(toml_src).unwrap();
        let patch = build_patch(&parsed.patch[0], &BTreeMap::new(), &[]).unwrap();
        assert_eq!(patch.fm_ratio, 384); // 1.5 × 256
        assert_eq!(patch.fm_index, 768); // 3.0 × 256
        assert_eq!(patch.osc[0].mode, OscMode::Fm2Op);
    }

    #[test]
    fn parses_committed_big_world_groove_mid() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/big-world/audio/songs/groove.mid");
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return,
        };
        let song = voxlconsl_audio::parse_smf(&bytes).expect("groove.mid parses");
        assert!(!song.events.is_empty());
        assert_eq!(song.ticks_per_quarter, 96);
    }

    #[test]
    fn encode_section_round_trips_via_decoder() {
        let patches = vec![(0u8, vec![1u8, 2, 3, 4])];
        let samples = vec![LoadedSample {
            pcm: WavPcm {
                sample_rate_hz: 22_050,
                pcm: vec![100, 110, 120],
                loop_points: Some((1, 2)),
            },
        }];
        let songs = vec![(0u8, vec![b'M', b'T', b'h', b'd'])];
        let bytes = encode_section(&patches, &samples, &songs);
        let decoded: Vec<_> = audio_section::entries(&bytes).collect();
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].kind, KIND_PATCH);
        assert_eq!(decoded[0].data, &[1, 2, 3, 4]);
        let s = decoded[1].as_sample().unwrap();
        assert_eq!(s.sample_rate_hz, 22_050);
        assert_eq!(s.loop_points, Some((1, 2)));
        assert_eq!(s.pcm, &[100, 110, 120]);
        assert_eq!(decoded[2].kind, KIND_SONG);
        assert_eq!(decoded[2].data, b"MThd");
    }
}
