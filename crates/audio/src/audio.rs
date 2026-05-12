//! Audio engine — see SPEC.md §5.
//!
//! Stage 1 (v0.1.8): mixer + sample bank + one-shot SFX.
//! Stage 2 (v0.1.9): subtractive synth voices — 2 osc + filter + amp
//!                   ADSR + filter ADSR + LFO + glide. Temporary
//!                   `voice_trigger / voice_release` API stands in for
//!                   the MIDI event surface until Stage 3.
//!
//! ## Architecture (browser MVP)
//!
//! - Mixer runs on the **main thread** alongside the cart. No worklet,
//!   no SharedArrayBuffer, no contention.
//! - Cart calls `sfx_play` / `voice_trigger` via host imports, which
//!   mutate the voice pool directly. New voices start at the next
//!   mixer block boundary (~3 ms latency).
//! - Browser shim pulls blocks each `requestAnimationFrame` to keep
//!   ~3 chunks queued ahead of `AudioContext.currentTime` (~35 ms
//!   output latency).
//!
//! Future MCU ports will move the mixer to its own task with a SPSC
//! ring between cart task and audio task. The portable Rust mixer
//! doesn't care which threading model is in use.

extern crate alloc;

use alloc::vec::Vec;

/// Mixer output sample rate, Hz (§5.8).
pub const SAMPLE_RATE: u32 = 22_050;

/// Frames per mixer block (§5.8). 64 frames @ 22.05 kHz ≈ 2.9 ms.
pub const BLOCK_FRAMES: usize = 64;

/// Interleaved-stereo samples per block (L,R,L,R,...).
pub const BLOCK_SAMPLES: usize = BLOCK_FRAMES * 2;

/// Maximum simultaneous voices (§5.2). Shared across MIDI notes + SFX.
pub const VOICE_POOL_SIZE: usize = 16;

/// Maximum sample slots per cart (§5.4).
pub const SAMPLE_SLOTS: usize = 64;

/// Maximum patches per cart (§5.1).
pub const PATCH_SLOTS: usize = 16;

/// Maximum SMF song slots per cart (§5.3).
pub const SONG_SLOTS: usize = 8;

/// Default SMF tempo when no SetTempo meta has been seen — 120 BPM
/// per the MIDI spec.
pub const DEFAULT_TEMPO_US_PER_QN: u32 = 500_000;

/// Pitch bend at full deflection (±8192) covers ±2 semitones per the
/// §5.2 default.
const PITCH_BEND_SEMITONES: f32 = 2.0;

/// MIDI channels (§5.2). 16 channels, identity-mapped to patches by
/// default (channel N → patch N % PATCH_SLOTS). MIDI channel "10"
/// follows General MIDI convention as the drum channel; in our
/// 0-indexed API that's channel index 9.
pub const MIDI_CHANNELS: usize = 16;

/// 0-indexed MIDI channel that defaults to drum-kit playback.
/// `program_change` on this channel switches it to a normal synth
/// patch and disables the drum bypass.
pub const DRUM_CHANNEL: u8 = 9;

/// Number of slots in the host-private drum bank. Covers GM
/// percussion notes 35..=81; the slot for a given note is
/// `note - 35`. Not all slots are populated — only the recipes in
/// `DRUM_RECIPES` fill in their corresponding slots.
pub const DRUM_BANK_SIZE: usize = 47;

/// Sentinel `channel` value for `SynthVoiceState` created via the
/// `voice_trigger` primitive (not via `note_on`). These voices don't
/// match any MIDI channel for `note_off` / `all_notes_off` lookups.
const NO_CHANNEL: u8 = 0xFF;

// MIDI CC numbers we recognize (§5.2). Other CCs are silently ignored.
pub const CC_MOD_WHEEL: u8 = 1;
pub const CC_VOLUME: u8 = 7;
pub const CC_PAN: u8 = 10;
pub const CC_EXPRESSION: u8 = 11;
pub const CC_SUSTAIN: u8 = 64;
pub const CC_REVERB_SEND: u8 = 91;
/// Per §5.2 GM "chorus send" is mapped to delay in our fixed FX bus.
pub const CC_DELAY_SEND: u8 = 93;

/// Each voice mixes its contribution as f32 in roughly ±1 range into
/// a per-block accumulator. The master stage soft-clips the sum
/// through `tanh` (which smoothly compresses anything above ±1 toward
/// ±1) and then quantizes to i16. With this design the per-voice
/// scale is "natural" — voices output normalized f32 like every
/// DSP-paper formula assumes, polyphony just adds together, and the
/// soft clipper gracefully handles whatever total amplitude shows
/// up. No manual headroom math, no constants per voice type.

// ===========================================================================
// Sample bank (Stage 1) — unchanged.
// ===========================================================================

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SampleRate {
    Khz11_025,
    Khz22_05,
}

impl SampleRate {
    pub fn from_code(c: u8) -> Self {
        match c {
            0 => Self::Khz11_025,
            _ => Self::Khz22_05,
        }
    }

    pub fn hz(self) -> u32 {
        match self {
            Self::Khz11_025 => 11_025,
            Self::Khz22_05 => 22_050,
        }
    }
}

#[derive(Clone)]
pub struct Sample {
    pub data: Vec<u8>,
    pub rate: SampleRate,
    pub loop_points: Option<(u32, u32)>,
}

// ===========================================================================
// Patch types (Stage 2)
// ===========================================================================

/// Oscillator waveform (§5.1). FM2OP is a coupled-pair mode — when
/// osc A's mode is `Fm2Op`, osc B becomes the modulator (its freq is
/// `carrier_freq × fm_ratio` and its sine output adds to the carrier's
/// phase argument scaled by `fm_index`). osc B's own `mode` is
/// ignored in this configuration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OscMode {
    Sine,
    Saw,
    Square,
    Triangle,
    Noise,
    Fm2Op,
}

impl OscMode {
    pub fn from_code(c: u8) -> Self {
        match c {
            0 => Self::Sine,
            1 => Self::Saw,
            2 => Self::Square,
            3 => Self::Triangle,
            4 => Self::Noise,
            5 => Self::Fm2Op,
            _ => Self::Sine,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FilterMode {
    Off,
    LowPass,
    HighPass,
    BandPass,
}

impl FilterMode {
    pub fn from_code(c: u8) -> Self {
        match c {
            1 => Self::LowPass,
            2 => Self::HighPass,
            3 => Self::BandPass,
            _ => Self::Off,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LfoShape {
    Sine,
    Triangle,
    Square,
    SampleAndHold,
}

impl LfoShape {
    pub fn from_code(c: u8) -> Self {
        match c {
            1 => Self::Triangle,
            2 => Self::Square,
            3 => Self::SampleAndHold,
            _ => Self::Sine,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LfoTarget {
    Pitch,
    Filter,
    Amp,
    Pan,
}

impl LfoTarget {
    pub fn from_code(c: u8) -> Self {
        match c {
            1 => Self::Filter,
            2 => Self::Amp,
            3 => Self::Pan,
            _ => Self::Pitch,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct OscParams {
    pub mode: OscMode,
    pub detune_cents: i16,
    pub octave: i8,
    /// 0..=127, mix level for this oscillator.
    pub level: u8,
}

#[derive(Copy, Clone, Debug)]
pub struct FilterParams {
    pub mode: FilterMode,
    /// 20..=10000 typical; clamped to [20, SAMPLE_RATE/2 - 100] at use.
    pub cutoff_hz: u16,
    /// 0..=127. Higher → more peak at cutoff; capped to avoid SVF
    /// self-oscillation at extreme values.
    pub resonance: u8,
}

#[derive(Copy, Clone, Debug)]
pub struct EnvParams {
    pub attack_ms: u16,
    pub decay_ms: u16,
    /// 0..=127, sustain level as fraction of peak.
    pub sustain: u8,
    pub release_ms: u16,
}

#[derive(Copy, Clone, Debug)]
pub struct LfoParams {
    /// Hundredths of a Hz: 100 = 1 Hz, 1000 = 10 Hz, 1 = 0.01 Hz.
    pub rate_centihz: u16,
    pub shape: LfoShape,
    pub target: LfoTarget,
    /// -127..=127 signed. Positive vs negative changes the sign of the
    /// routed modulation.
    pub depth: i8,
}

/// Patch source family (§5.1). The non-source portion (filter, amp
/// env, filter env, LFO, glide) is shared between both kinds; only
/// what generates the raw waveform differs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PatchKind {
    /// 2-osc subtractive engine — what `voice_trigger` has always done.
    /// Optionally FM2OP via osc[0].mode = `Fm2Op`.
    Synth,
    /// Sample bank source with per-zone key mapping (§5.1 sampler).
    /// `note_on` finds the matching key zone, pitches the sample by
    /// `note - root_note` semitones, and feeds it through the patch's
    /// filter + envelopes + LFO.
    Sampler,
}

impl PatchKind {
    pub fn from_code(c: u8) -> Self {
        match c {
            1 => Self::Sampler,
            _ => Self::Synth,
        }
    }
}

/// One of up to 8 key zones in a sampler patch (§5.1).
///
/// A note matches zone `z` when `z.low_note <= note <= z.high_note`.
/// Zones are checked in order, so for overlapping ranges the first
/// hit wins. `root_note` is the note at which the sample plays at
/// its declared sample rate (i.e. no pitch shift); other notes are
/// resampled `(note - root_note)` semitones up/down.
#[derive(Copy, Clone, Debug)]
pub struct KeyZone {
    pub low_note: u8,
    pub high_note: u8,
    pub root_note: u8,
    /// Cart sample-bank slot (0..=63). Drum bank is not addressable
    /// from sampler patches — the drum kit is host-private.
    pub sample_slot: u8,
    /// -64..=63. Adds/subtracts from the per-note velocity at trigger
    /// time (negative attenuates that zone).
    pub volume_offset: i8,
    pub loop_start: u32,
    pub loop_end: u32,
    pub loop_enabled: bool,
}

impl KeyZone {
    pub const fn empty() -> Self {
        Self {
            low_note: 0,
            high_note: 0,
            root_note: 60,
            sample_slot: 0,
            volume_offset: 0,
            loop_start: 0,
            loop_end: 0,
            loop_enabled: false,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct Patch {
    pub kind: PatchKind,
    pub osc: [OscParams; 2],
    pub filter: FilterParams,
    pub amp_env: EnvParams,
    pub filter_env: EnvParams,
    /// Signed depth of filter-env modulation on cutoff. Range
    /// -127..=127, scaled internally to ±8 kHz of cutoff shift.
    pub filter_env_depth: i8,
    pub lfo: LfoParams,
    /// Portamento time, ms. 0 = no glide (instant pitch change on
    /// retrigger).
    pub glide_ms: u16,
    /// FM2OP modulator-to-carrier frequency ratio, Q8.8 fixed-point
    /// (raw / 256.0). Common settings: 256 = 1.0 (unison), 512 = 2.0
    /// (octave up modulator → square-ish harmonics), 384 = 1.5
    /// (perfect fifth → metallic). Only consulted when
    /// `osc[0].mode == Fm2Op`.
    pub fm_ratio: u16,
    /// FM2OP modulation index, Q8.8. Peak phase deflection in
    /// radians (so 256 = 1.0 rad ≈ 57°). Realistic: 256–2560 (1–10);
    /// above ~5 the signal goes into bell / noise territory.
    pub fm_index: u16,
    /// Sampler key zones (§5.1). Only the first `zone_count` entries
    /// are checked at trigger time; the rest are inert placeholders.
    pub zones: [KeyZone; 8],
    pub zone_count: u8,
}

impl Patch {
    /// Default patch: single pure sine, no filter, snappy amp ADSR.
    /// Safe starting point for a cart that hasn't configured any
    /// patches yet.
    pub const fn default_synth() -> Self {
        Self {
            osc: [
                OscParams { mode: OscMode::Sine, detune_cents: 0, octave: 0, level: 127 },
                OscParams { mode: OscMode::Sine, detune_cents: 0, octave: 0, level: 0 },
            ],
            filter: FilterParams { mode: FilterMode::Off, cutoff_hz: 8000, resonance: 0 },
            amp_env: EnvParams { attack_ms: 5, decay_ms: 50, sustain: 100, release_ms: 80 },
            filter_env: EnvParams { attack_ms: 1, decay_ms: 100, sustain: 0, release_ms: 100 },
            filter_env_depth: 0,
            lfo: LfoParams { rate_centihz: 500, shape: LfoShape::Sine, target: LfoTarget::Pitch, depth: 0 },
            glide_ms: 0,
            fm_ratio: 256,  // 1.0
            fm_index: 256,  // 1.0 rad — gentle FM if the cart switches mode to Fm2Op
            zones: [KeyZone::empty(); 8],
            zone_count: 0,
            kind: PatchKind::Synth,
        }
    }
}

// ===========================================================================
// VoiceId — cart-facing voice handle, unchanged from Stage 1.
// ===========================================================================

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct VoiceId(pub u32);

impl VoiceId {
    pub const NONE: Self = Self(0);

    fn pack(slot: usize, generation: u8) -> Self {
        Self(((generation as u32) << 8) | (slot as u32 & 0xFF))
    }

    fn slot(self) -> usize {
        (self.0 & 0xFF) as usize
    }

    fn generation(self) -> u8 {
        ((self.0 >> 8) & 0xFF) as u8
    }
}

// ===========================================================================
// Voice — internal state. Either idle, an SFX one-shot, or a synth note.
// ===========================================================================

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum EnvStage {
    Attack,
    Decay,
    Sustain,
    Release,
    Done,
}

#[derive(Copy, Clone, Debug)]
struct EnvelopeState {
    stage: EnvStage,
    value: f32,
}

/// State-variable filter using the **topology-preserving transform**
/// (Vadim Zavalishin / Andy Simper / Cytomic). Unconditionally stable
/// across the entire audible cutoff range — no clamping or
/// belt-and-suspenders state limiting required. The two integrator
/// states `ic1eq` / `ic2eq` are reset to 0 on `voice_trigger`.
#[derive(Copy, Clone, Debug, Default)]
struct SvfState {
    ic1eq: f32,
    ic2eq: f32,
}

/// Which sample bank an SFX voice draws from. Cart samples (the
/// 64-slot bank populated by `sample_register`) and host drum samples
/// (the 47-slot bank synthesized at boot for channel-10 percussion)
/// share the same SFX mix path — they only differ in lookup table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SampleSource {
    Cart(u8),
    Drum(u8),
}

#[derive(Copy, Clone, Debug)]
struct SfxVoiceState {
    source: SampleSource,
    /// Fractional position in sample data. f64 keeps drift negligible
    /// over multi-second samples.
    position: f64,
    /// Source samples per output frame.
    pitch_ratio: f64,
    loop_: bool,
}

#[derive(Copy, Clone, Debug)]
struct SynthVoiceState {
    patch: u8,
    /// 0..=15 MIDI channel that triggered the voice. Used by
    /// `note_off` / `all_notes_off` / sustain to look up which
    /// voice(s) to release.
    channel: u8,
    /// MIDI note number — `note_off` matches voices by `(channel,
    /// note)`.
    note: u8,
    velocity: u8,
    /// Per-oscillator phase, [0, 1).
    osc_phase: [f32; 2],
    amp_env: EnvelopeState,
    filter_env: EnvelopeState,
    filter: SvfState,
    /// LFO phase, [0, 1).
    lfo_phase: f32,
    /// Sample-and-hold latched value; updated on each S&H phase wrap.
    sh_value: f32,
    /// PRNG state for noise oscillator + S&H sampling.
    rng: u32,
    /// Frequency the voice currently outputs. Glides toward
    /// `target_freq` over `glide_ms`.
    cur_freq: f32,
    /// Frequency the most recent note_on requested.
    target_freq: f32,
    /// Note has been released — env enters Release stage.
    released: bool,
}

/// Live state for a sampler-kind voice (§5.1). Sample bank slot +
/// resampler position + the same envelope/filter/LFO chain as the
/// synth voice — only the source of the waveform differs.
#[derive(Copy, Clone, Debug)]
struct SamplerVoiceState {
    patch: u8,
    /// MIDI channel that triggered the voice (NO_CHANNEL when
    /// triggered via `voice_trigger` directly, which doesn't bind to
    /// a channel). Used for `note_off` / `all_notes_off` matching
    /// and pitch-bend live-read.
    channel: u8,
    note: u8,
    velocity: u8,
    /// Pre-resolved at trigger time: the sample-bank slot this zone
    /// pointed at. Kept on the voice so subsequent zone edits to
    /// the patch don't yank the rug out from under live voices.
    sample_slot: u8,
    /// Sample read head, fractional.
    position: f64,
    /// Resampling ratio without the live pitch bend factor — the
    /// product of `sample_rate / mixer_rate` and `2^((note - root)/12)`.
    /// Pitch bend is read each block and multiplied in for the
    /// effective per-sample step.
    base_pitch_ratio: f64,
    loop_start: u32,
    loop_end: u32,
    loop_enabled: bool,
    /// Per-zone volume offset, signed -64..=63. Scales the voice amp
    /// by `1 + offset/127` (so -64 → -50%, 0 → unity, +63 → +50%).
    volume_offset: i8,
    amp_env: EnvelopeState,
    filter_env: EnvelopeState,
    filter: SvfState,
    lfo_phase: f32,
    sh_value: f32,
    rng: u32,
    released: bool,
}

#[derive(Copy, Clone, Debug)]
enum VoiceKind {
    Idle,
    Sfx(SfxVoiceState),
    Synth(SynthVoiceState),
    Sampler(SamplerVoiceState),
}

#[derive(Copy, Clone, Debug)]
struct Voice {
    /// 0..=127 (MIDI velocity-style channel volume). Synth voices
    /// have this default to 127; Stage 3's CC 7 will modulate it.
    volume: u8,
    /// -64..=63.
    pan: i8,
    /// Per-voice reverb send (§5.5). Snapshot from `channels[ch].reverb_send`
    /// at trigger time so the wet contribution stays stable across the
    /// voice's lifetime. SFX voices (no source channel) default to 0.
    reverb_send: u8,
    /// Per-voice delay send (§5.5). Snapshot from `channels[ch].delay_send`.
    delay_send: u8,
    /// Generation counter incremented each time the slot is freed.
    /// Always ≥ 1 while live so `VoiceId::NONE == VoiceId(0)` never
    /// matches a real voice.
    generation: u8,
    start_frame: u64,
    kind: VoiceKind,
}

impl Voice {
    const fn idle() -> Self {
        Self {
            volume: 0,
            pan: 0,
            reverb_send: 0,
            delay_send: 0,
            generation: 1,
            start_frame: 0,
            kind: VoiceKind::Idle,
        }
    }

    fn active(&self) -> bool {
        !matches!(self.kind, VoiceKind::Idle)
    }

    fn deactivate(&mut self) {
        self.kind = VoiceKind::Idle;
        self.generation = bump_generation(self.generation);
    }
}

#[inline]
fn bump_generation(g: u8) -> u8 {
    let n = g.wrapping_add(1);
    if n == 0 { 1 } else { n }
}

// ===========================================================================
// MIDI channel state (Stage 3)
// ===========================================================================

/// Per-channel state for the §5.2 MIDI surface. Values are typed
/// as MIDI-native ranges; conversion to mixer-native happens at use.
#[derive(Copy, Clone, Debug)]
struct Channel {
    /// Which patch this channel currently plays. `program_change`
    /// updates this and also flips `is_drum` to false.
    patch_idx: u8,
    /// True for channel 9 (MIDI 10) by default — `note_on` then
    /// bypasses the patch system and triggers a drum sample from the
    /// host-private drum bank. Cleared on `program_change`.
    is_drum: bool,
    /// CC 7. 0..=127.
    volume: u8,
    /// CC 10. 0..=127; 0 = full L, 64 = center, 127 = full R.
    pan: u8,
    /// CC 11. 0..=127. Multiplies with volume per §5.2.
    expression: u8,
    /// CC 1. 0..=127. Adds to the patch's LFO depth.
    mod_wheel: u8,
    /// Pitch bend wheel. -8192..=8191; ±2 semitones at full scale by
    /// default. Per-channel range customization is reserved.
    pitch_bend: i16,
    /// CC 64. While true, note_off adds the note number to
    /// `sustained_notes` instead of releasing the voice. When sustain
    /// transitions false, every sustained note gets released.
    sustain_held: bool,
    /// Bitmap of MIDI note numbers held by the sustain pedal.
    sustained_lo: u64,
    sustained_hi: u64,
    /// CC 91. Send amount into the global reverb bus (§5.5). 0..=127.
    reverb_send: u8,
    /// CC 93 ("chorus send" → delay in our fixed FX bus). 0..=127.
    delay_send: u8,
}

impl Channel {
    fn new(idx: usize) -> Self {
        Self {
            // Identity default mapping per §5.2: channel N → patch
            // N % 16. Cart can `program_change` immediately to remap.
            patch_idx: (idx % PATCH_SLOTS) as u8,
            is_drum: idx == DRUM_CHANNEL as usize,
            volume: 100,
            pan: 64,
            expression: 127,
            mod_wheel: 0,
            pitch_bend: 0,
            sustain_held: false,
            sustained_lo: 0,
            sustained_hi: 0,
            reverb_send: 0,
            delay_send: 0,
        }
    }

    fn mark_sustained(&mut self, note: u8) {
        if note < 64 {
            self.sustained_lo |= 1u64 << note;
        } else if note < 128 {
            self.sustained_hi |= 1u64 << (note - 64);
        }
    }

    fn clear_sustained(&mut self) {
        self.sustained_lo = 0;
        self.sustained_hi = 0;
    }
}

// ===========================================================================
// Drum kit — synthesized at host boot from §5.1's synth engine.
// ===========================================================================
//
// The §5.2 default for channel 10 is a "drum-kit patch" that maps
// note numbers to percussion samples. Rather than ship those samples
// as PCM blobs in the host firmware, we describe each drum as a
// recipe (patch + note + velocity + duration) and render it once at
// `AudioState::new()`. The rendered audio lives in a host-private
// 47-slot bank — separate from the cart's 64-slot sample bank, so
// carts retain their full bank.
//
// The recipes are deliberately minimal (~10 GM drums). Without the
// FM oscillator / pitch envelope shipping in later stages, the kick
// and toms can't do the classic 808 pitch sweep — so they speak
// their fundamental directly. That gives a cleaner "fantasy console"
// drum aesthetic than chasing exact-replica realism.

struct DrumRecipe {
    /// GM percussion note number this drum responds to. The render
    /// is stored at `drum_bank[gm_note - 35]`.
    gm_note: u8,
    /// Patch describing the synthesis chain for this drum.
    patch: Patch,
    /// MIDI note number used to trigger the synth voice. For pitched
    /// drums (kick, tom) this sets the fundamental; for noise-based
    /// drums it's irrelevant.
    synth_note: u8,
    /// Rendered sample length, ms. Rounded up to the next mixer
    /// block (BLOCK_FRAMES = 64 frames @ 22.05 kHz ≈ 2.9 ms).
    duration_ms: u16,
}

const DRUM_RECIPES: &[DrumRecipe] = &[
    // Acoustic Bass Drum + Bass Drum 1 — same render, two GM notes.
    DrumRecipe {
        gm_note: 35,
        patch: drum_patch_kick(),
        synth_note: 36, // C2 = 65 Hz
        duration_ms: 220,
    },
    DrumRecipe {
        gm_note: 36,
        patch: drum_patch_kick(),
        synth_note: 36,
        duration_ms: 220,
    },
    // Acoustic + Electric Snare
    DrumRecipe {
        gm_note: 38,
        patch: drum_patch_snare(),
        synth_note: 48,
        duration_ms: 160,
    },
    DrumRecipe {
        gm_note: 40,
        patch: drum_patch_snare(),
        synth_note: 48,
        duration_ms: 160,
    },
    // Hand Clap
    DrumRecipe {
        gm_note: 39,
        patch: drum_patch_clap(),
        synth_note: 60,
        duration_ms: 120,
    },
    // Closed Hat / Pedal Hat / Open Hat — same patch, different decay.
    DrumRecipe {
        gm_note: 42,
        patch: drum_patch_hat_closed(),
        synth_note: 84,
        duration_ms: 80,
    },
    DrumRecipe {
        gm_note: 44,
        patch: drum_patch_hat_closed(),
        synth_note: 84,
        duration_ms: 100,
    },
    DrumRecipe {
        gm_note: 46,
        patch: drum_patch_hat_open(),
        synth_note: 84,
        duration_ms: 350,
    },
    // Low / Mid / High Tom
    DrumRecipe {
        gm_note: 41,
        patch: drum_patch_tom(),
        synth_note: 42, // F#2 = 92.5 Hz
        duration_ms: 320,
    },
    DrumRecipe {
        gm_note: 47,
        patch: drum_patch_tom(),
        synth_note: 50, // D3 = 146.8 Hz
        duration_ms: 280,
    },
    DrumRecipe {
        gm_note: 50,
        patch: drum_patch_tom(),
        synth_note: 57, // A3 = 220 Hz
        duration_ms: 240,
    },
    // Crash + Ride — broad HP noise with different decays.
    DrumRecipe {
        gm_note: 49,
        patch: drum_patch_crash(),
        synth_note: 84,
        duration_ms: 900,
    },
    DrumRecipe {
        gm_note: 51,
        patch: drum_patch_ride(),
        synth_note: 84,
        duration_ms: 1200,
    },
];

// All drum patches use a 1 ms amp-env attack rather than 0. With an
// instantaneous attack, the very first rendered PCM sample is silence
// (env value 0) and the next is at peak amplitude — a hard step that
// reads as a click/pop at sample-1 in the baked PCM. 1 ms ramp keeps
// the perceived "punch" while smoothing the discontinuity.

const fn drum_patch_kick() -> Patch {
    Patch {
        osc: [
            OscParams { mode: OscMode::Sine,  detune_cents: 0, octave: 0, level: 127 },
            OscParams { mode: OscMode::Noise, detune_cents: 0, octave: 0, level: 15 },
        ],
        filter: FilterParams { mode: FilterMode::LowPass, cutoff_hz: 200, resonance: 0 },
        amp_env: EnvParams { attack_ms: 1, decay_ms: 200, sustain: 0, release_ms: 0 },
        filter_env: EnvParams { attack_ms: 1, decay_ms: 30, sustain: 0, release_ms: 0 },
        filter_env_depth: 100,
        lfo: LfoParams { rate_centihz: 0, shape: LfoShape::Sine, target: LfoTarget::Pitch, depth: 0 },
        glide_ms: 0,
        fm_ratio: 256,
        fm_index: 0,
        zones: [KeyZone::empty(); 8],
        zone_count: 0,
        kind: PatchKind::Synth,
    }
}

const fn drum_patch_snare() -> Patch {
    Patch {
        osc: [
            // Body: sine at ~130 Hz.
            OscParams { mode: OscMode::Sine,  detune_cents: 0, octave: 0, level: 70 },
            // Snare buzz: noise dominant.
            OscParams { mode: OscMode::Noise, detune_cents: 0, octave: 0, level: 127 },
        ],
        filter: FilterParams { mode: FilterMode::HighPass, cutoff_hz: 800, resonance: 0 },
        amp_env: EnvParams { attack_ms: 1, decay_ms: 140, sustain: 0, release_ms: 0 },
        filter_env: EnvParams { attack_ms: 1, decay_ms: 60, sustain: 0, release_ms: 0 },
        filter_env_depth: 60,
        lfo: LfoParams { rate_centihz: 0, shape: LfoShape::Sine, target: LfoTarget::Pitch, depth: 0 },
        glide_ms: 0,
        fm_ratio: 256,
        fm_index: 0,
        zones: [KeyZone::empty(); 8],
        zone_count: 0,
        kind: PatchKind::Synth,
    }
}

const fn drum_patch_clap() -> Patch {
    Patch {
        osc: [
            OscParams { mode: OscMode::Noise, detune_cents: 0, octave: 0, level: 127 },
            OscParams { mode: OscMode::Sine,  detune_cents: 0, octave: 0, level: 0 },
        ],
        // Bandpass around ~1.5 kHz gives clap's tonal character.
        filter: FilterParams { mode: FilterMode::BandPass, cutoff_hz: 1500, resonance: 40 },
        amp_env: EnvParams { attack_ms: 1, decay_ms: 100, sustain: 0, release_ms: 0 },
        filter_env: EnvParams { attack_ms: 1, decay_ms: 40, sustain: 0, release_ms: 0 },
        filter_env_depth: 30,
        lfo: LfoParams { rate_centihz: 0, shape: LfoShape::Sine, target: LfoTarget::Pitch, depth: 0 },
        glide_ms: 0,
        fm_ratio: 256,
        fm_index: 0,
        zones: [KeyZone::empty(); 8],
        zone_count: 0,
        kind: PatchKind::Synth,
    }
}

const fn drum_patch_hat_closed() -> Patch {
    Patch {
        osc: [
            OscParams { mode: OscMode::Noise, detune_cents: 0, octave: 0, level: 127 },
            OscParams { mode: OscMode::Sine,  detune_cents: 0, octave: 0, level: 0 },
        ],
        filter: FilterParams { mode: FilterMode::HighPass, cutoff_hz: 6000, resonance: 20 },
        amp_env: EnvParams { attack_ms: 1, decay_ms: 60, sustain: 0, release_ms: 0 },
        filter_env: EnvParams { attack_ms: 1, decay_ms: 30, sustain: 0, release_ms: 0 },
        filter_env_depth: 0,
        lfo: LfoParams { rate_centihz: 0, shape: LfoShape::Sine, target: LfoTarget::Pitch, depth: 0 },
        glide_ms: 0,
        fm_ratio: 256,
        fm_index: 0,
        zones: [KeyZone::empty(); 8],
        zone_count: 0,
        kind: PatchKind::Synth,
    }
}

const fn drum_patch_hat_open() -> Patch {
    Patch {
        osc: [
            OscParams { mode: OscMode::Noise, detune_cents: 0, octave: 0, level: 127 },
            OscParams { mode: OscMode::Sine,  detune_cents: 0, octave: 0, level: 0 },
        ],
        filter: FilterParams { mode: FilterMode::HighPass, cutoff_hz: 6000, resonance: 20 },
        amp_env: EnvParams { attack_ms: 1, decay_ms: 300, sustain: 0, release_ms: 0 },
        filter_env: EnvParams { attack_ms: 1, decay_ms: 100, sustain: 0, release_ms: 0 },
        filter_env_depth: 0,
        lfo: LfoParams { rate_centihz: 0, shape: LfoShape::Sine, target: LfoTarget::Pitch, depth: 0 },
        glide_ms: 0,
        fm_ratio: 256,
        fm_index: 0,
        zones: [KeyZone::empty(); 8],
        zone_count: 0,
        kind: PatchKind::Synth,
    }
}

const fn drum_patch_tom() -> Patch {
    Patch {
        osc: [
            OscParams { mode: OscMode::Sine,  detune_cents: 0, octave: 0, level: 127 },
            OscParams { mode: OscMode::Noise, detune_cents: 0, octave: 0, level: 10 },
        ],
        filter: FilterParams { mode: FilterMode::LowPass, cutoff_hz: 800, resonance: 0 },
        amp_env: EnvParams { attack_ms: 1, decay_ms: 260, sustain: 0, release_ms: 0 },
        filter_env: EnvParams { attack_ms: 1, decay_ms: 80, sustain: 0, release_ms: 0 },
        filter_env_depth: 40,
        lfo: LfoParams { rate_centihz: 0, shape: LfoShape::Sine, target: LfoTarget::Pitch, depth: 0 },
        glide_ms: 0,
        fm_ratio: 256,
        fm_index: 0,
        zones: [KeyZone::empty(); 8],
        zone_count: 0,
        kind: PatchKind::Synth,
    }
}

const fn drum_patch_crash() -> Patch {
    Patch {
        osc: [
            OscParams { mode: OscMode::Noise, detune_cents: 0, octave: 0, level: 127 },
            OscParams { mode: OscMode::Sine,  detune_cents: 0, octave: 0, level: 0 },
        ],
        filter: FilterParams { mode: FilterMode::HighPass, cutoff_hz: 4000, resonance: 0 },
        amp_env: EnvParams { attack_ms: 2, decay_ms: 800, sustain: 0, release_ms: 0 },
        filter_env: EnvParams { attack_ms: 0, decay_ms: 400, sustain: 0, release_ms: 0 },
        filter_env_depth: 0,
        lfo: LfoParams { rate_centihz: 0, shape: LfoShape::Sine, target: LfoTarget::Pitch, depth: 0 },
        glide_ms: 0,
        fm_ratio: 256,
        fm_index: 0,
        zones: [KeyZone::empty(); 8],
        zone_count: 0,
        kind: PatchKind::Synth,
    }
}

const fn drum_patch_ride() -> Patch {
    Patch {
        osc: [
            OscParams { mode: OscMode::Noise, detune_cents: 0, octave: 0, level: 127 },
            OscParams { mode: OscMode::Sine,  detune_cents: 0, octave: 0, level: 0 },
        ],
        filter: FilterParams { mode: FilterMode::HighPass, cutoff_hz: 5000, resonance: 30 },
        amp_env: EnvParams { attack_ms: 1, decay_ms: 1100, sustain: 0, release_ms: 0 },
        filter_env: EnvParams { attack_ms: 0, decay_ms: 500, sustain: 0, release_ms: 0 },
        filter_env_depth: 0,
        lfo: LfoParams { rate_centihz: 0, shape: LfoShape::Sine, target: LfoTarget::Pitch, depth: 0 },
        glide_ms: 0,
        fm_ratio: 256,
        fm_index: 0,
        zones: [KeyZone::empty(); 8],
        zone_count: 0,
        kind: PatchKind::Synth,
    }
}

/// Render one drum recipe through a throwaway mixer instance. Output
/// is mono 8-bit unsigned PCM at 22.05 kHz — the canonical sample
/// format defined in §5.4 — so the drum samples are indistinguishable
/// from cart-authored ones once they land in the bank.
fn synthesize_drum_sample(recipe: &DrumRecipe) -> Vec<u8> {
    let mut tmp = AudioState::new_silent();
    tmp.patches[0] = recipe.patch;
    tmp.voice_trigger(0, recipe.synth_note, 127);

    let num_frames =
        (recipe.duration_ms as usize) * (SAMPLE_RATE as usize) / 1000;
    let num_blocks = num_frames.div_ceil(BLOCK_FRAMES);
    let mut output = Vec::with_capacity(num_blocks * BLOCK_FRAMES);
    let mut block = vec![0i16; BLOCK_SAMPLES];

    for _ in 0..num_blocks {
        tmp.render_block(&mut block);
        for i in 0..BLOCK_FRAMES {
            let l = block[i * 2] as i32;
            let r = block[i * 2 + 1] as i32;
            let mono = (l + r) / 2;
            let byte = (((mono + 32768) / 256).clamp(0, 255)) as u8;
            output.push(byte);
        }
    }

    output.truncate(num_frames);
    output
}

// ===========================================================================
// AudioState — public mixer API.
// ===========================================================================

pub struct AudioState {
    samples: Vec<Option<Sample>>,
    /// Host-private drum samples synthesized at boot. Indexed by
    /// `gm_note - 35`; covers GM percussion notes 35..=81 with
    /// unfilled slots (None) for drums we don't synthesize.
    drum_bank: Vec<Option<Sample>>,
    patches: Vec<Patch>,
    voices: Vec<Voice>,
    channels: Vec<Channel>,
    /// SMF song slots (§5.3). Loaded from cart bytes; can be played
    /// back via `music_play`. Only one slot can be active at a time
    /// (single global playhead).
    songs: Vec<Option<crate::smf::Song>>,
    music: MusicPlayer,
    /// Global reverb send bus (§5.5). One Schroeder reverb shared
    /// across all voices; each voice's contribution scales by
    /// `voice.reverb_send / 127`. Cart sets `room_size` + `damping`
    /// via `reverb_set`.
    reverb: crate::audio_fx::ReverbState,
    /// Global delay send bus (§5.5). Stereo cross-feedback ping-
    /// pong delay. Cart sets `time_ms` + `feedback` via `delay_set`.
    delay: crate::audio_fx::DelayState,
    frame_counter: u64,
    /// Telemetry: incremented when `sfx_play` / `voice_trigger` had to
    /// steal a voice.
    pub voices_stolen: u32,
}

/// State for the single global music player (§5.3). At most one song
/// plays at a time. The playhead is in tick units (fractional during
/// partial-block advances) so tempo changes don't drift the clock.
#[derive(Debug, Clone)]
struct MusicPlayer {
    active_slot: Option<u8>,
    loop_: bool,
    cursor_ticks: f64,
    event_index: usize,
    us_per_qn: u32,
    tempo_scale: f32,
}

impl MusicPlayer {
    fn new() -> Self {
        Self {
            active_slot: None,
            loop_: false,
            cursor_ticks: 0.0,
            event_index: 0,
            us_per_qn: DEFAULT_TEMPO_US_PER_QN,
            tempo_scale: 1.0,
        }
    }

    fn reset_playhead(&mut self) {
        self.cursor_ticks = 0.0;
        self.event_index = 0;
        self.us_per_qn = DEFAULT_TEMPO_US_PER_QN;
    }
}

impl AudioState {
    /// Full constructor — initializes the host drum kit at boot.
    /// Adds ~150 ms of synthesis work at construction time (release
    /// build); tests that don't need percussion can use `new_silent`
    /// to skip it.
    pub fn new() -> Self {
        let mut state = Self::new_silent();
        state.init_drum_kit();
        state
    }

    /// Bare constructor — no drum kit synthesized. Use this from
    /// tests where percussion isn't under test, and from the drum-
    /// kit renderer itself (to avoid recursion).
    pub fn new_silent() -> Self {
        let mut samples = Vec::with_capacity(SAMPLE_SLOTS);
        samples.resize_with(SAMPLE_SLOTS, || None);
        let mut drum_bank = Vec::with_capacity(DRUM_BANK_SIZE);
        drum_bank.resize_with(DRUM_BANK_SIZE, || None);
        let mut patches = Vec::with_capacity(PATCH_SLOTS);
        patches.resize(PATCH_SLOTS, Patch::default_synth());
        let voices = (0..VOICE_POOL_SIZE).map(|_| Voice::idle()).collect();
        let channels = (0..MIDI_CHANNELS).map(Channel::new).collect();
        let mut songs = Vec::with_capacity(SONG_SLOTS);
        songs.resize_with(SONG_SLOTS, || None);
        Self {
            samples,
            drum_bank,
            patches,
            voices,
            channels,
            songs,
            music: MusicPlayer::new(),
            reverb: crate::audio_fx::ReverbState::new(),
            delay: crate::audio_fx::DelayState::new(),
            frame_counter: 0,
            voices_stolen: 0,
        }
    }

    fn init_drum_kit(&mut self) {
        for recipe in DRUM_RECIPES {
            let pcm = synthesize_drum_sample(recipe);
            if (recipe.gm_note as usize) >= 35
                && (recipe.gm_note as usize - 35) < DRUM_BANK_SIZE
            {
                let slot = recipe.gm_note as usize - 35;
                self.drum_bank[slot] = Some(Sample {
                    data: pcm,
                    rate: SampleRate::Khz22_05,
                    loop_points: None,
                });
            }
        }
    }

    // ── Sample bank (Stage 1) ──────────────────────────────────────

    pub fn register_sample(&mut self, slot: u8, sample: Sample) {
        if (slot as usize) >= SAMPLE_SLOTS {
            return;
        }
        self.samples[slot as usize] = Some(sample);
    }

    pub fn clear_sample(&mut self, slot: u8) {
        if (slot as usize) >= SAMPLE_SLOTS {
            return;
        }
        self.samples[slot as usize] = None;
    }

    pub fn sfx_play(
        &mut self,
        slot: u8,
        volume: u8,
        pan: i8,
        pitch_cents: i16,
        loop_: bool,
    ) -> VoiceId {
        let src_hz = match self.samples.get(slot as usize).and_then(|s| s.as_ref()) {
            Some(s) => s.rate.hz() as f64,
            None => return VoiceId::NONE,
        };
        let pitch_ratio = compute_pitch_ratio(src_hz, pitch_cents);
        let frame = self.frame_counter;
        let idx = self.allocate_voice();
        let v = &mut self.voices[idx];
        v.volume = volume.min(127);
        v.pan = pan.clamp(-64, 63);
        // `sfx_play` is channel-less — sends default to 0. Carts that
        // want wet SFX should drive samples through a sampler-mode
        // patch on a real MIDI channel (Stage 6 brings sampler kind).
        v.reverb_send = 0;
        v.delay_send = 0;
        v.start_frame = frame;
        v.kind = VoiceKind::Sfx(SfxVoiceState {
            source: SampleSource::Cart(slot),
            position: 0.0,
            pitch_ratio,
            loop_,
        });
        VoiceId::pack(idx, v.generation)
    }

    pub fn sfx_stop(&mut self, id: VoiceId) {
        if let Some(idx) = self.lookup_voice(id) {
            self.voices[idx].deactivate();
        }
    }

    pub fn sfx_set_volume(&mut self, id: VoiceId, volume: u8) {
        if let Some(idx) = self.lookup_voice(id) {
            self.voices[idx].volume = volume.min(127);
        }
    }

    pub fn sfx_set_pitch(&mut self, id: VoiceId, pitch_cents: i16) {
        let idx = match self.lookup_voice(id) {
            Some(i) => i,
            None => return,
        };
        if let VoiceKind::Sfx(state) = &mut self.voices[idx].kind {
            let src_hz = match state.source {
                SampleSource::Cart(slot) => {
                    match self.samples.get(slot as usize).and_then(|s| s.as_ref()) {
                        Some(s) => s.rate.hz() as f64,
                        None => return,
                    }
                }
                SampleSource::Drum(slot) => {
                    match self.drum_bank.get(slot as usize).and_then(|s| s.as_ref()) {
                        Some(s) => s.rate.hz() as f64,
                        None => return,
                    }
                }
            };
            state.pitch_ratio = compute_pitch_ratio(src_hz, pitch_cents);
        }
    }

    // ── Patch editing (Stage 2) ────────────────────────────────────

    pub fn patch_set_osc(
        &mut self,
        slot: u8,
        osc_idx: u8,
        mode: OscMode,
        detune_cents: i16,
        octave: i8,
        level: u8,
    ) {
        let Some(patch) = self.patch_mut(slot) else { return };
        let Some(osc) = patch.osc.get_mut(osc_idx as usize) else { return };
        osc.mode = mode;
        osc.detune_cents = detune_cents;
        osc.octave = octave;
        osc.level = level.min(127);
    }

    pub fn patch_set_filter(
        &mut self,
        slot: u8,
        mode: FilterMode,
        cutoff_hz: u16,
        resonance: u8,
    ) {
        let Some(patch) = self.patch_mut(slot) else { return };
        patch.filter.mode = mode;
        patch.filter.cutoff_hz = cutoff_hz;
        patch.filter.resonance = resonance.min(127);
    }

    pub fn patch_set_amp_env(
        &mut self,
        slot: u8,
        attack_ms: u16,
        decay_ms: u16,
        sustain: u8,
        release_ms: u16,
    ) {
        let Some(patch) = self.patch_mut(slot) else { return };
        patch.amp_env = EnvParams { attack_ms, decay_ms, sustain: sustain.min(127), release_ms };
    }

    pub fn patch_set_filter_env(
        &mut self,
        slot: u8,
        attack_ms: u16,
        decay_ms: u16,
        sustain: u8,
        release_ms: u16,
        depth: i8,
    ) {
        let Some(patch) = self.patch_mut(slot) else { return };
        patch.filter_env = EnvParams { attack_ms, decay_ms, sustain: sustain.min(127), release_ms };
        patch.filter_env_depth = depth;
    }

    pub fn patch_set_lfo(
        &mut self,
        slot: u8,
        rate_centihz: u16,
        shape: LfoShape,
        target: LfoTarget,
        depth: i8,
    ) {
        let Some(patch) = self.patch_mut(slot) else { return };
        patch.lfo = LfoParams { rate_centihz, shape, target, depth };
    }

    pub fn patch_set_glide(&mut self, slot: u8, ms: u16) {
        let Some(patch) = self.patch_mut(slot) else { return };
        patch.glide_ms = ms;
    }

    /// Set the FM2OP ratio + index for a patch. Both are Q8.8 raw
    /// (raw / 256.0 = the float value). Only takes effect when
    /// `osc[0].mode == Fm2Op`; otherwise the cart-side osc[0]/osc[1]
    /// renders normally as parallel oscillators.
    pub fn patch_set_fm(&mut self, slot: u8, ratio_q88: u16, index_q88: u16) {
        let Some(patch) = self.patch_mut(slot) else { return };
        patch.fm_ratio = ratio_q88;
        patch.fm_index = index_q88;
    }

    /// Switch a patch between Synth and Sampler kinds (§5.1).
    /// Switching kinds preserves the shared filter / envelopes / LFO
    /// / glide / FX-send fields; only the *source* of the waveform
    /// changes. The cart must populate `zones` separately when
    /// switching to Sampler.
    pub fn patch_set_kind(&mut self, slot: u8, kind: PatchKind) {
        let Some(patch) = self.patch_mut(slot) else { return };
        patch.kind = kind;
    }

    /// Configure one key zone in a sampler patch (§5.1). `zone_idx`
    /// 0..=7. The first `zone_count` zones (set via
    /// `patch_set_zone_count`) are consulted at trigger time; later
    /// entries are inert. Out-of-range slot / index silently ignored.
    pub fn patch_set_zone(&mut self, slot: u8, zone_idx: u8, zone: KeyZone) {
        let Some(patch) = self.patch_mut(slot) else { return };
        let Some(z) = patch.zones.get_mut(zone_idx as usize) else { return };
        *z = zone;
    }

    /// How many of the patch's 8 zone slots are valid. Defaults to 0
    /// (all sampler triggers fail) so the cart must opt in.
    pub fn patch_set_zone_count(&mut self, slot: u8, count: u8) {
        let Some(patch) = self.patch_mut(slot) else { return };
        patch.zone_count = count.min(patch.zones.len() as u8);
    }

    /// Serialize a patch into `out` per the §5.7 blob format. Returns
    /// the number of bytes written, or 0 on out-of-range slot or when
    /// `out` is too small.
    pub fn patch_save(&self, slot: u8, out: &mut [u8]) -> u32 {
        let Some(patch) = self.patches.get(slot as usize) else { return 0 };
        crate::audio_patch_blob::save(patch, out)
    }

    /// Parse a §5.7 patch blob and write it into `slot`. Returns
    /// `true` on success, `false` on bad magic / version /
    /// truncation / out-of-range slot. On failure the slot is left
    /// untouched.
    pub fn patch_load(&mut self, slot: u8, src: &[u8]) -> bool {
        if (slot as usize) >= PATCH_SLOTS {
            return false;
        }
        let Some(patch) = crate::audio_patch_blob::load(src) else {
            return false;
        };
        self.patches[slot as usize] = patch;
        true
    }

    #[cfg(test)]
    pub(crate) fn patches_for_test(&self) -> &[Patch] {
        &self.patches
    }

    pub fn patch_reset(&mut self, slot: u8) {
        let Some(patch) = self.patch_mut(slot) else { return };
        *patch = Patch::default_synth();
    }

    pub fn patch_copy(&mut self, src: u8, dst: u8) {
        if (src as usize) >= PATCH_SLOTS || (dst as usize) >= PATCH_SLOTS {
            return;
        }
        self.patches[dst as usize] = self.patches[src as usize];
    }

    pub fn patch(&self, slot: u8) -> Option<&Patch> {
        self.patches.get(slot as usize)
    }

    fn patch_mut(&mut self, slot: u8) -> Option<&mut Patch> {
        self.patches.get_mut(slot as usize)
    }

    // ── Synth voice trigger / release (Stage 2 temporary API) ─────

    /// Start a synth note. `note` is MIDI note number (60 = middle C).
    /// `velocity` 0..=127 scales output amplitude. Returns NONE if the
    /// patch slot is out of range. The voice plays the patch's envelope
    /// until `voice_release` is called or it's stolen — for a hold-to-
    /// sustain feel, call `voice_release` on key-up.
    pub fn voice_trigger(&mut self, patch_slot: u8, note: u8, velocity: u8) -> VoiceId {
        if (patch_slot as usize) >= PATCH_SLOTS {
            return VoiceId::NONE;
        }
        match self.patches[patch_slot as usize].kind {
            PatchKind::Synth => self.voice_trigger_synth(patch_slot, note, velocity),
            PatchKind::Sampler => self.voice_trigger_sampler(patch_slot, note, velocity),
        }
    }

    fn voice_trigger_synth(&mut self, patch_slot: u8, note: u8, velocity: u8) -> VoiceId {
        let frame = self.frame_counter;
        let target_freq = note_to_freq(note);
        let glide_ms = self.patches[patch_slot as usize].glide_ms;
        let idx = self.allocate_voice();

        // Glide source: if we just stole a synth voice on the same
        // patch, glide from its current freq; otherwise jump straight
        // to target. Stage 2 doesn't track per-patch "last note" yet,
        // so we always start at target_freq for new triggers.
        let cur_freq = if glide_ms > 0 { target_freq * 0.5 } else { target_freq };
        let _ = cur_freq; // (placeholder — proper glide from last freq is Stage 3 work)

        let v = &mut self.voices[idx];
        v.volume = 127;
        v.pan = 0;
        // voice_trigger is the channel-less primitive — sends default
        // to 0 so test-side `voice_trigger` calls produce dry output
        // even if the previous voice in this slot had wet sends.
        v.reverb_send = 0;
        v.delay_send = 0;
        v.start_frame = frame;
        v.kind = VoiceKind::Synth(SynthVoiceState {
            patch: patch_slot,
            // `voice_trigger` is the under-MIDI primitive — voices
            // created this way aren't bound to a MIDI channel and
            // won't match `note_off` lookups. Use the MIDI surface
            // (`note_on` / `note_off`) for channel-aware notes.
            channel: NO_CHANNEL,
            note,
            velocity: velocity.min(127),
            osc_phase: [0.0, 0.0],
            amp_env: EnvelopeState { stage: EnvStage::Attack, value: 0.0 },
            filter_env: EnvelopeState { stage: EnvStage::Attack, value: 0.0 },
            filter: SvfState::default(),
            lfo_phase: 0.0,
            sh_value: 0.0,
            // Mix the voice index into the seed so simultaneous noise
            // voices don't sound identical.
            rng: 0x9E37_79B9 ^ ((idx as u32) << 16) ^ (frame as u32),
            cur_freq: target_freq,
            target_freq,
            released: false,
        });
        VoiceId::pack(idx, v.generation)
    }

    fn voice_trigger_sampler(&mut self, patch_slot: u8, note: u8, velocity: u8) -> VoiceId {
        let patch = self.patches[patch_slot as usize];
        let zone = match find_zone(&patch, note) {
            Some(z) => z,
            None => return VoiceId::NONE,
        };
        let sample = match self.samples.get(zone.sample_slot as usize).and_then(|s| s.as_ref()) {
            Some(s) => s,
            None => return VoiceId::NONE,
        };
        let src_hz = sample.rate.hz() as f64;
        let semitones = note as f64 - zone.root_note as f64;
        let pitch_factor = (2.0_f64).powf(semitones / 12.0);
        let base_pitch_ratio = (src_hz / SAMPLE_RATE as f64) * pitch_factor;

        let frame = self.frame_counter;
        let idx = self.allocate_voice();
        let v = &mut self.voices[idx];
        v.volume = 127;
        v.pan = 0;
        v.reverb_send = 0;
        v.delay_send = 0;
        v.start_frame = frame;
        v.kind = VoiceKind::Sampler(SamplerVoiceState {
            patch: patch_slot,
            channel: NO_CHANNEL,
            note,
            velocity: velocity.min(127),
            sample_slot: zone.sample_slot,
            position: 0.0,
            base_pitch_ratio,
            loop_start: zone.loop_start,
            loop_end: zone.loop_end,
            loop_enabled: zone.loop_enabled,
            volume_offset: zone.volume_offset,
            amp_env: EnvelopeState { stage: EnvStage::Attack, value: 0.0 },
            filter_env: EnvelopeState { stage: EnvStage::Attack, value: 0.0 },
            filter: SvfState::default(),
            lfo_phase: 0.0,
            sh_value: 0.0,
            rng: 0x9E37_79B9 ^ ((idx as u32) << 16) ^ (frame as u32),
            released: false,
        });
        VoiceId::pack(idx, v.generation)
    }

    /// Move a synth voice's amp/filter envelopes into the Release
    /// stage. The voice keeps mixing until release completes, then
    /// auto-frees. No-op for SFX voices or stale ids.
    pub fn voice_release(&mut self, id: VoiceId) {
        let Some(idx) = self.lookup_voice(id) else { return };
        match &mut self.voices[idx].kind {
            VoiceKind::Synth(state) => {
                if !state.released {
                    state.released = true;
                    state.amp_env.stage = EnvStage::Release;
                    state.filter_env.stage = EnvStage::Release;
                }
            }
            VoiceKind::Sampler(state) => {
                if !state.released {
                    state.released = true;
                    state.amp_env.stage = EnvStage::Release;
                    state.filter_env.stage = EnvStage::Release;
                }
            }
            _ => {}
        }
    }

    // ── MIDI surface (Stage 3) ─────────────────────────────────────

    /// Start a note on a MIDI channel. Channel 9 (MIDI 10) by default
    /// bypasses the patch system and triggers a sample from the
    /// host-private drum bank (slot = `note - 35`); other channels
    /// route to the channel's current patch via `voice_trigger`.
    ///
    /// `velocity == 0` is a common MIDI convention for "note off" —
    /// in §5.2 we route it through `note_off(channel, note)` for that
    /// reason. Returns `VoiceId::NONE` on out-of-range channel /
    /// note, missing drum sample, or missing patch.
    pub fn note_on(&mut self, channel: u8, note: u8, velocity: u8) -> VoiceId {
        if (channel as usize) >= MIDI_CHANNELS || note >= 128 {
            return VoiceId::NONE;
        }
        if velocity == 0 {
            self.note_off(channel, note);
            return VoiceId::NONE;
        }
        let ch = &self.channels[channel as usize];
        if ch.is_drum {
            let reverb_send = ch.reverb_send;
            let delay_send = ch.delay_send;
            return self.trigger_drum_voice(note, velocity, ch.pan, reverb_send, delay_send);
        }
        let patch = ch.patch_idx;
        let (vol, pan) = (ch.volume, ch.pan);
        let reverb_send = ch.reverb_send;
        let delay_send = ch.delay_send;
        let id = self.voice_trigger(patch, note, velocity);
        if id != VoiceId::NONE {
            if let Some(idx) = self.lookup_voice(id) {
                self.voices[idx].volume = midi_volume_combined(vol, self.channels[channel as usize].expression);
                self.voices[idx].pan = midi_pan_to_signed(pan);
                self.voices[idx].reverb_send = reverb_send;
                self.voices[idx].delay_send = delay_send;
                match &mut self.voices[idx].kind {
                    VoiceKind::Synth(state) => { state.channel = channel; }
                    VoiceKind::Sampler(state) => { state.channel = channel; }
                    _ => {}
                }
            }
        }
        id
    }

    /// Release voices that match `(channel, note)`. If sustain is
    /// held on the channel, the note number is recorded and the
    /// release is deferred until sustain transitions off.
    pub fn note_off(&mut self, channel: u8, note: u8) {
        if (channel as usize) >= MIDI_CHANNELS || note >= 128 {
            return;
        }
        if self.channels[channel as usize].sustain_held {
            self.channels[channel as usize].mark_sustained(note);
            return;
        }
        self.release_matching_voices(channel, Some(note));
    }

    /// Set the pitch bend for a channel. Stage 4a wires the channel's
    /// 14-bit bend value into the synth oscillator pitch via
    /// `bend_factor` in `mix_synth_voice`; full deflection (±8192) is
    /// ±`PITCH_BEND_SEMITONES` semitones. Drum-channel bypass voices
    /// (channel 9 by default) don't read this — they play at their
    /// recorded source rate.
    pub fn pitch_bend(&mut self, channel: u8, value: i16) {
        if (channel as usize) >= MIDI_CHANNELS {
            return;
        }
        self.channels[channel as usize].pitch_bend = value.clamp(-8192, 8191);
    }

    /// Handle a recognized control change. Other CCs are silently
    /// ignored per §5.2's fixed table.
    pub fn cc(&mut self, channel: u8, controller: u8, value: u8) {
        if (channel as usize) >= MIDI_CHANNELS {
            return;
        }
        let value = value.min(127);
        let ch = &mut self.channels[channel as usize];
        match controller {
            CC_MOD_WHEEL => ch.mod_wheel = value,
            CC_VOLUME => ch.volume = value,
            CC_PAN => ch.pan = value,
            CC_EXPRESSION => ch.expression = value,
            CC_REVERB_SEND => ch.reverb_send = value,
            CC_DELAY_SEND => ch.delay_send = value,
            CC_SUSTAIN => {
                let was_held = ch.sustain_held;
                let now_held = value >= 64; // MIDI convention: ≥64 = on
                ch.sustain_held = now_held;
                if was_held && !now_held {
                    let lo = ch.sustained_lo;
                    let hi = ch.sustained_hi;
                    ch.clear_sustained();
                    // Release each note that was held by sustain.
                    for n in 0..64u8 {
                        if lo & (1u64 << n) != 0 {
                            self.release_matching_voices(channel, Some(n));
                        }
                    }
                    for n in 0..64u8 {
                        if hi & (1u64 << n) != 0 {
                            self.release_matching_voices(channel, Some(n + 64));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Bind a channel to a different patch. Always clears the
    /// drum-bypass flag — even programming channel 10 to a synth
    /// patch (per §5.2 the cart's option to reclaim it for melodic
    /// content).
    pub fn program_change(&mut self, channel: u8, patch: u8) {
        if (channel as usize) >= MIDI_CHANNELS || (patch as usize) >= PATCH_SLOTS {
            return;
        }
        let ch = &mut self.channels[channel as usize];
        ch.patch_idx = patch;
        ch.is_drum = false;
    }

    /// Release every voice on this channel, regardless of note.
    /// Standard panic / scene-cleanup primitive.
    pub fn all_notes_off(&mut self, channel: u8) {
        if (channel as usize) >= MIDI_CHANNELS {
            return;
        }
        self.channels[channel as usize].clear_sustained();
        self.release_matching_voices(channel, None);
    }

    /// Read the patch a channel is currently bound to.
    pub fn channel_patch(&self, channel: u8) -> Option<u8> {
        self.channels
            .get(channel as usize)
            .map(|c| c.patch_idx)
    }

    // ── FX bus globals (Stage 5, §5.5) ─────────────────────────────

    /// Set the global reverb parameters. `room_size` 0..=127 maps to
    /// comb feedback in [0.70, 0.98] (longer tails as size grows);
    /// `damping` 0..=127 sets the in-feedback lowpass coefficient
    /// (more damping = darker tail). Default at boot is medium room,
    /// medium damping — so cart-side `cc(ch, CC_REVERB_SEND, n)`
    /// alone produces an audible effect without further setup.
    pub fn reverb_set(&mut self, room_size: u8, damping: u8) {
        self.reverb.set_params(room_size, damping);
    }

    /// Set the global delay parameters. `time_ms` is the per-tap
    /// delay (1..=2000); `feedback` 0..=127 maps to gain in [0, 0.95]
    /// for the stereo cross-feedback path. Default at boot is 250 ms
    /// / feedback 50 — already audible if a cart sets a delay send.
    pub fn delay_set(&mut self, time_ms: u16, feedback: u8) {
        self.delay.set_time_ms(time_ms);
        self.delay.set_feedback(feedback);
    }

    // ── Music / SMF playback (Stage 4a, §5.3) ─────────────────────

    /// Load and parse an SMF byte stream into `slot`. Returns `true`
    /// on success, `false` on out-of-range slot or parse failure.
    /// A successful load replaces whatever song was previously in the
    /// slot; if the slot was the active playback target, playback is
    /// stopped.
    pub fn load_song(&mut self, slot: u8, bytes: &[u8]) -> bool {
        if (slot as usize) >= SONG_SLOTS {
            return false;
        }
        let song = match crate::smf::parse(bytes) {
            Ok(s) => s,
            Err(_) => return false,
        };
        if self.music.active_slot == Some(slot) {
            self.music_stop();
        }
        self.songs[slot as usize] = Some(song);
        true
    }

    /// Drop a song slot.
    pub fn clear_song(&mut self, slot: u8) {
        if (slot as usize) >= SONG_SLOTS {
            return;
        }
        if self.music.active_slot == Some(slot) {
            self.music_stop();
        }
        self.songs[slot as usize] = None;
    }

    /// Start playback of `slot` from the top. If `loop_` is true, the
    /// player wraps to tick 0 + default tempo after the last event;
    /// at the wrap boundary every channel gets an `all_notes_off` so
    /// no released-but-not-yet-decayed notes hang across the seam.
    /// No-op on empty or out-of-range slot.
    pub fn music_play(&mut self, slot: u8, loop_: bool) {
        if (slot as usize) >= SONG_SLOTS || self.songs[slot as usize].is_none() {
            return;
        }
        // Silence anything currently playing on the previous song so
        // we don't leak voices across songs.
        if self.music.active_slot.is_some() {
            for ch in 0..MIDI_CHANNELS as u8 {
                self.all_notes_off(ch);
            }
        }
        self.music = MusicPlayer {
            active_slot: Some(slot),
            loop_,
            cursor_ticks: 0.0,
            event_index: 0,
            us_per_qn: DEFAULT_TEMPO_US_PER_QN,
            tempo_scale: self.music.tempo_scale,
        };
    }

    /// Stop playback. Releases any voices that came from the song.
    pub fn music_stop(&mut self) {
        if self.music.active_slot.is_none() {
            return;
        }
        for ch in 0..MIDI_CHANNELS as u8 {
            self.all_notes_off(ch);
        }
        self.music.active_slot = None;
        self.music.reset_playhead();
    }

    /// Scale the song's authored tempo. 1.0 = as authored, 2.0 = double
    /// speed, 0.5 = half speed. Clamped to a sane range so the cart
    /// can't divide by zero in the playback math.
    pub fn music_set_tempo_scale(&mut self, scale: f32) {
        self.music.tempo_scale = scale.clamp(0.01, 100.0);
    }

    /// Current playhead in quarter-note beats from song start. Returns
    /// 0.0 when no song is playing.
    pub fn music_position_beats(&self) -> f32 {
        let Some(slot) = self.music.active_slot else {
            return 0.0;
        };
        let Some(song) = self.songs.get(slot as usize).and_then(|s| s.as_ref()) else {
            return 0.0;
        };
        (self.music.cursor_ticks / song.ticks_per_quarter.max(1) as f64) as f32
    }

    /// Whether any song is currently active (playing or paused — we
    /// don't expose pause yet, so this means "actively scheduling").
    pub fn music_is_playing(&self) -> bool {
        self.music.active_slot.is_some()
    }

    /// Advance the music playhead by one mixer block and dispatch any
    /// events whose absolute tick fell inside this block. Called from
    /// the top of `render_block`. Tempo changes (SetTempo meta events)
    /// take effect immediately within the same block — the loop walks
    /// events sequentially so the tick-to-microsecond conversion uses
    /// the right `us_per_qn` for each segment.
    fn tick_music(&mut self) {
        let Some(slot) = self.music.active_slot else {
            return;
        };
        // Pre-fetch the slot we need; release the borrow before
        // dispatching so we can mutate self freely.
        let mut dispatched: [Option<crate::smf::MidiEvent>; 64] = [None; 64];
        let mut n_dispatched: usize = 0;
        let mut loop_wrap = false;
        let mut song_finished = false;
        let mut overflow_us = 0.0_f64;

        {
            let song = match self.songs.get(slot as usize).and_then(|s| s.as_ref()) {
                Some(s) => s,
                None => {
                    // Slot got dropped under us — stop gracefully.
                    self.music.active_slot = None;
                    return;
                }
            };
            let tpq = song.ticks_per_quarter.max(1) as f64;
            let block_us =
                (BLOCK_FRAMES as f64 / SAMPLE_RATE as f64) * 1_000_000.0;
            let mut remaining_us = block_us * self.music.tempo_scale as f64;

            while remaining_us > 0.0 {
                if self.music.event_index >= song.events.len() {
                    if self.music.loop_ {
                        loop_wrap = true;
                        // Carry any unused block time into the looped
                        // playback so tempo-scaled songs don't gain a
                        // free silent block at the wrap boundary.
                        overflow_us = remaining_us;
                    } else {
                        song_finished = true;
                    }
                    break;
                }
                let next = &song.events[self.music.event_index];
                let delta_ticks = next.tick as f64 - self.music.cursor_ticks;
                let delta_ticks = if delta_ticks < 0.0 { 0.0 } else { delta_ticks };
                let us_per_tick = self.music.us_per_qn as f64 / tpq;
                let delta_us = delta_ticks * us_per_tick;

                if delta_us <= remaining_us {
                    remaining_us -= delta_us;
                    self.music.cursor_ticks = next.tick as f64;
                    if let crate::smf::MidiEvent::SetTempo { us_per_qn } = next.event {
                        self.music.us_per_qn = us_per_qn;
                    } else if n_dispatched < dispatched.len() {
                        dispatched[n_dispatched] = Some(next.event);
                        n_dispatched += 1;
                    }
                    // If the dispatched buffer is exhausted, we silently
                    // drop the rest of this block's events — they'll be
                    // picked up next block. This only matters if a song
                    // packs > 64 events in a 2.9 ms block (≈22 kHz event
                    // rate), which is well outside any realistic SMF.
                    self.music.event_index += 1;
                } else {
                    let ticks_advance = remaining_us / us_per_tick;
                    self.music.cursor_ticks += ticks_advance;
                    break;
                }
            }
        }

        for ev in dispatched.iter().take(n_dispatched) {
            if let Some(ev) = ev {
                self.dispatch_midi_event(*ev);
            }
        }

        if loop_wrap {
            for ch in 0..MIDI_CHANNELS as u8 {
                self.all_notes_off(ch);
            }
            self.music.reset_playhead();
            // Consume `overflow_us` against the freshly-reset song so a
            // very short song doesn't visibly stall at every loop point.
            if overflow_us > 0.0 {
                self.tick_music_overflow(slot, overflow_us);
            }
        } else if song_finished {
            self.music.active_slot = None;
        }
    }

    /// Consume `us` microseconds of song time at the current playhead,
    /// dispatching any events that fall in the window. Used after a
    /// loop wrap to drain unused block time. Re-uses the same logic
    /// shape as `tick_music` without the block-time wrapper.
    fn tick_music_overflow(&mut self, slot: u8, mut remaining_us: f64) {
        let mut dispatched: [Option<crate::smf::MidiEvent>; 64] = [None; 64];
        let mut n: usize = 0;
        {
            let song = match self.songs.get(slot as usize).and_then(|s| s.as_ref()) {
                Some(s) => s,
                None => return,
            };
            let tpq = song.ticks_per_quarter.max(1) as f64;
            while remaining_us > 0.0 {
                if self.music.event_index >= song.events.len() {
                    break;
                }
                let next = &song.events[self.music.event_index];
                let delta_ticks =
                    (next.tick as f64 - self.music.cursor_ticks).max(0.0);
                let us_per_tick = self.music.us_per_qn as f64 / tpq;
                let delta_us = delta_ticks * us_per_tick;
                if delta_us <= remaining_us {
                    remaining_us -= delta_us;
                    self.music.cursor_ticks = next.tick as f64;
                    if let crate::smf::MidiEvent::SetTempo { us_per_qn } = next.event {
                        self.music.us_per_qn = us_per_qn;
                    } else if n < dispatched.len() {
                        dispatched[n] = Some(next.event);
                        n += 1;
                    }
                    self.music.event_index += 1;
                } else {
                    let ticks_advance = remaining_us / us_per_tick;
                    self.music.cursor_ticks += ticks_advance;
                    break;
                }
            }
        }
        for ev in dispatched.iter().take(n) {
            if let Some(ev) = ev {
                self.dispatch_midi_event(*ev);
            }
        }
    }

    fn dispatch_midi_event(&mut self, ev: crate::smf::MidiEvent) {
        match ev {
            crate::smf::MidiEvent::NoteOn { channel, note, velocity } => {
                self.note_on(channel, note, velocity);
            }
            crate::smf::MidiEvent::NoteOff { channel, note } => {
                self.note_off(channel, note);
            }
            crate::smf::MidiEvent::PitchBend { channel, value } => {
                self.pitch_bend(channel, value);
            }
            crate::smf::MidiEvent::Cc { channel, controller, value } => {
                self.cc(channel, controller, value);
            }
            crate::smf::MidiEvent::ProgramChange { channel, patch } => {
                self.program_change(channel, patch);
            }
            crate::smf::MidiEvent::SetTempo { .. } => {
                // Handled inline in `tick_music` — never dispatched here.
            }
        }
    }

    /// Trigger an SFX-style voice from the host-private drum bank
    /// without going through `sfx_play` (which targets the cart's
    /// sample bank). Drum samples are mono 8-bit PCM at 22.05 kHz,
    /// indexed by `gm_note - 35`.
    fn trigger_drum_voice(
        &mut self,
        note: u8,
        velocity: u8,
        channel_pan: u8,
        reverb_send: u8,
        delay_send: u8,
    ) -> VoiceId {
        if note < 35 {
            return VoiceId::NONE;
        }
        let slot = (note - 35) as usize;
        if slot >= DRUM_BANK_SIZE || self.drum_bank[slot].is_none() {
            return VoiceId::NONE;
        }
        let src_hz = self.drum_bank[slot]
            .as_ref()
            .map(|s| s.rate.hz() as f64)
            .unwrap_or(22_050.0);
        let pitch_ratio = compute_pitch_ratio(src_hz, 0);
        let frame = self.frame_counter;
        let idx = self.allocate_voice();
        let v = &mut self.voices[idx];
        v.volume = velocity.min(127);
        v.pan = midi_pan_to_signed(channel_pan);
        v.reverb_send = reverb_send;
        v.delay_send = delay_send;
        v.start_frame = frame;
        v.kind = VoiceKind::Sfx(SfxVoiceState {
            source: SampleSource::Drum(slot as u8),
            position: 0.0,
            pitch_ratio,
            loop_: false,
        });
        VoiceId::pack(idx, v.generation)
    }

    /// Walk the voice pool and release every synth voice that was
    /// triggered by `note_on(channel, note)`. If `note` is `None`,
    /// release every voice on the channel regardless of note.
    fn release_matching_voices(&mut self, channel: u8, note: Option<u8>) {
        for i in 0..self.voices.len() {
            match &mut self.voices[i].kind {
                VoiceKind::Synth(state) => {
                    if state.channel == channel
                        && note.map_or(true, |n| state.note == n)
                        && !state.released
                    {
                        state.released = true;
                        state.amp_env.stage = EnvStage::Release;
                        state.filter_env.stage = EnvStage::Release;
                    }
                }
                VoiceKind::Sampler(state) => {
                    if state.channel == channel
                        && note.map_or(true, |n| state.note == n)
                        && !state.released
                    {
                        state.released = true;
                        state.amp_env.stage = EnvStage::Release;
                        state.filter_env.stage = EnvStage::Release;
                    }
                }
                _ => {}
            }
        }
    }

    // ── Render ─────────────────────────────────────────────────────

    /// Render one block of stereo interleaved samples. `out` must have
    /// length `BLOCK_SAMPLES`.
    ///
    /// Voices mix their contributions in normalized f32 (≈ ±1 per
    /// voice) into an internal accumulator. The master stage soft-
    /// clips the sum through `tanh` and quantizes to i16. tanh gives
    /// smooth, monotonic compression: single voice stays near unity
    /// gain (~-2.4 dB), polyphony gradually compresses, and the
    /// output is bounded by ±1 at the output regardless of how many
    /// voices add up — so the i16 cast can never overflow.
    pub fn render_block(&mut self, out: &mut [i16]) {
        debug_assert_eq!(out.len(), BLOCK_SAMPLES);

        // Advance SMF playback first so any events that fall in this
        // block reach the voice pool before mixing begins.
        self.tick_music();

        let mut acc = [0.0_f32; BLOCK_SAMPLES];
        // Wet send buses — voices write scaled copies of their dry
        // contribution into these per their CC 91 / 93 sends, then
        // the FX blocks process them in place and we sum back into
        // `acc` before the master soft-clip.
        let mut reverb_bus = [0.0_f32; BLOCK_SAMPLES];
        let mut delay_bus = [0.0_f32; BLOCK_SAMPLES];

        // Split borrow so each voice can read the (immutable) sample
        // banks + patch table while we mutate the voice itself.
        let samples = &self.samples;
        let drum_bank = &self.drum_bank;
        let patches = &self.patches;
        let channels = &self.channels;
        for voice in self.voices.iter_mut() {
            match &mut voice.kind {
                VoiceKind::Idle => {}
                VoiceKind::Sfx(_) => mix_sfx_voice(
                    voice, samples, drum_bank, &mut acc, &mut reverb_bus, &mut delay_bus,
                ),
                VoiceKind::Synth(_) => mix_synth_voice(
                    voice, patches, channels, &mut acc, &mut reverb_bus, &mut delay_bus,
                ),
                VoiceKind::Sampler(_) => mix_sampler_voice(
                    voice, samples, patches, channels, &mut acc, &mut reverb_bus, &mut delay_bus,
                ),
            }
        }

        // Process effect buses in place. The wet buffers now hold
        // the post-FX signal (reverb tail / delayed taps); add them
        // back into the dry accumulator at unity (per-voice send
        // already scaled the input).
        self.reverb.process_block(&mut reverb_bus);
        self.delay.process_block(&mut delay_bus);
        for i in 0..BLOCK_SAMPLES {
            acc[i] += reverb_bus[i] + delay_bus[i];
        }

        for (i, &s) in acc.iter().enumerate() {
            let clipped = s.tanh();
            out[i] = (clipped * i16::MAX as f32) as i16;
        }

        self.frame_counter = self.frame_counter.wrapping_add(BLOCK_FRAMES as u64);
    }

    fn lookup_voice(&self, id: VoiceId) -> Option<usize> {
        if id == VoiceId::NONE {
            return None;
        }
        let idx = id.slot();
        if idx >= self.voices.len() {
            return None;
        }
        let v = &self.voices[idx];
        if v.active() && v.generation == id.generation() {
            Some(idx)
        } else {
            None
        }
    }

    fn allocate_voice(&mut self) -> usize {
        if let Some((i, _)) = self.voices.iter().enumerate().find(|(_, v)| !v.active()) {
            return i;
        }
        // Stage 2 policy: oldest-first regardless of held/released.
        // Stage 3 will refine to "oldest released, then oldest held"
        // per §5.2 when we have proper note-on/off semantics.
        let (i, _) = self
            .voices
            .iter()
            .enumerate()
            .min_by_key(|(_, v)| v.start_frame)
            .expect("voice pool is empty");
        self.voices_stolen = self.voices_stolen.saturating_add(1);
        self.voices[i].deactivate();
        i
    }

    pub fn active_voice_count(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }

    pub fn frame_counter(&self) -> u64 {
        self.frame_counter
    }
}

impl Default for AudioState {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Helpers — common to SFX + Synth.
// ===========================================================================

/// Look up the first matching key zone for `note` in a sampler patch.
/// Returns `None` if the patch isn't sampler-kind or no zone covers
/// the note. Zones are checked in declaration order — overlapping
/// ranges prefer the earlier zone.
fn find_zone(patch: &Patch, note: u8) -> Option<KeyZone> {
    if patch.kind != PatchKind::Sampler {
        return None;
    }
    let n = (patch.zone_count as usize).min(patch.zones.len());
    for i in 0..n {
        let z = patch.zones[i];
        if z.low_note <= note && note <= z.high_note {
            return Some(z);
        }
    }
    None
}

#[inline]
fn compute_pitch_ratio(src_hz: f64, pitch_cents: i16) -> f64 {
    let factor = (2.0_f64).powf(pitch_cents as f64 / 1200.0);
    (src_hz / SAMPLE_RATE as f64) * factor
}

#[inline]
fn pan_gains(pan: i8) -> (f32, f32) {
    let p = pan as f32 / 64.0;
    if p <= 0.0 {
        (1.0, 1.0 + p)
    } else {
        (1.0 - p, 1.0)
    }
}

/// MIDI note → frequency (Hz). A4 (note 69) = 440 Hz, equal-tempered.
#[inline]
pub fn note_to_freq(note: u8) -> f32 {
    let n = note as f32 - 69.0;
    440.0 * (2.0_f32).powf(n / 12.0)
}

/// Combine CC 7 (channel volume) and CC 11 (expression) into the
/// single 0..=127 volume the voice mixer accepts. Per §5.2 these
/// multiply: `(volume × expression) / 127`.
#[inline]
fn midi_volume_combined(volume: u8, expression: u8) -> u8 {
    let v = volume as u16 * expression as u16 / 127;
    v.min(127) as u8
}

/// MIDI pan (0..=127, 64 = center) → mixer pan (-64..=63, 0 = center).
#[inline]
fn midi_pan_to_signed(pan: u8) -> i8 {
    (pan as i16 - 64).clamp(-64, 63) as i8
}

// ===========================================================================
// SFX mix path (Stage 1) — unchanged behavior.
// ===========================================================================

fn mix_sfx_voice(
    voice: &mut Voice,
    samples: &[Option<Sample>],
    drum_bank: &[Option<Sample>],
    out: &mut [f32],
    reverb_in: &mut [f32],
    delay_in: &mut [f32],
) {
    let state = match &mut voice.kind {
        VoiceKind::Sfx(s) => s,
        _ => return,
    };
    let sample = match state.source {
        SampleSource::Cart(slot) => samples.get(slot as usize).and_then(|s| s.as_ref()),
        SampleSource::Drum(slot) => drum_bank.get(slot as usize).and_then(|s| s.as_ref()),
    };
    let sample = match sample {
        Some(s) => s,
        None => {
            voice.deactivate();
            return;
        }
    };
    let data = &sample.data;
    if data.is_empty() {
        voice.deactivate();
        return;
    }

    let (gl, gr) = pan_gains(voice.pan);
    let vol = voice.volume as f32 / 127.0;
    let amp_l = vol * gl;
    let amp_r = vol * gr;
    let reverb_amt = voice.reverb_send as f32 / 127.0;
    let delay_amt = voice.delay_send as f32 / 127.0;

    let data_len = data.len();
    let loop_region = sample.loop_points.and_then(|(ls, le)| {
        let ls = (ls as usize).min(data_len.saturating_sub(1));
        let le = (le as usize).min(data_len);
        if le > ls { Some((ls, le)) } else { None }
    });

    let mut deactivated = false;
    for i in 0..BLOCK_FRAMES {
        if state.loop_ {
            let end = loop_region.map(|(_, le)| le).unwrap_or(data_len);
            let start = loop_region.map(|(ls, _)| ls).unwrap_or(0);
            if state.position >= end as f64 {
                let span = (end - start) as f64;
                if span > 0.0 {
                    state.position = start as f64 + (state.position - start as f64).rem_euclid(span);
                } else {
                    state.position = start as f64;
                }
            }
        } else if state.position + 1.0 >= data_len as f64 {
            deactivated = true;
            break;
        }

        let s0 = state.position.floor() as usize;
        let frac = (state.position - s0 as f64) as f32;
        let a = data.get(s0).copied().unwrap_or(128);
        let b = data.get(s0 + 1).copied().unwrap_or(a);
        // 8-bit unsigned PCM (128 = silence) → normalized f32 in ±1.
        let f0 = ((a as i32 - 128) as f32) * (1.0 / 128.0);
        let f1 = ((b as i32 - 128) as f32) * (1.0 / 128.0);
        let s = f0 + (f1 - f0) * frac;

        let oi = i * 2;
        let l = s * amp_l;
        let r = s * amp_r;
        out[oi] += l;
        out[oi + 1] += r;
        // Wet sends inherit the per-voice pan + volume; the FX stage
        // is the only thing scaling further by its global params.
        reverb_in[oi]     += l * reverb_amt;
        reverb_in[oi + 1] += r * reverb_amt;
        delay_in[oi]      += l * delay_amt;
        delay_in[oi + 1]  += r * delay_amt;

        state.position += state.pitch_ratio;
    }

    if deactivated {
        voice.deactivate();
    }
}

// ===========================================================================
// Synth mix path (Stage 2)
// ===========================================================================

#[inline]
fn advance_envelope(env: &mut EnvelopeState, params: &EnvParams, released: bool, dt: f32) -> f32 {
    // Linear ADSR — each segment ramps `value` toward the segment
    // target over `segment_ms`. Cheap and predictable; exponential
    // curves can come later if a cart needs them.
    let sustain_level = params.sustain as f32 / 127.0;
    match env.stage {
        EnvStage::Attack => {
            let rate = if params.attack_ms == 0 {
                f32::INFINITY
            } else {
                1.0 / (params.attack_ms as f32 * 0.001)
            };
            env.value += rate * dt;
            if env.value >= 1.0 {
                env.value = 1.0;
                env.stage = EnvStage::Decay;
            }
        }
        EnvStage::Decay => {
            let rate = if params.decay_ms == 0 {
                f32::INFINITY
            } else {
                (1.0 - sustain_level) / (params.decay_ms as f32 * 0.001)
            };
            env.value -= rate * dt;
            if env.value <= sustain_level {
                env.value = sustain_level;
                env.stage = EnvStage::Sustain;
            }
        }
        EnvStage::Sustain => {
            env.value = sustain_level;
        }
        EnvStage::Release => {
            let rate = if params.release_ms == 0 {
                f32::INFINITY
            } else {
                env.value.max(0.0001) / (params.release_ms as f32 * 0.001)
            };
            env.value -= rate * dt;
            if env.value <= 0.0 {
                env.value = 0.0;
                env.stage = EnvStage::Done;
            }
        }
        EnvStage::Done => {
            env.value = 0.0;
        }
    }
    // Hard-release override: if `released` was just set, the stages
    // above have already been switched to Release by `voice_release`,
    // so nothing extra to do here.
    let _ = released;
    env.value
}

#[inline]
fn lfo_value(
    phase: f32,
    phase_prev: f32,
    rng: &mut u32,
    sh_value: &mut f32,
    shape: LfoShape,
) -> f32 {
    use core::f32::consts::TAU;
    match shape {
        LfoShape::Sine => (phase * TAU).sin(),
        LfoShape::Triangle => {
            if phase < 0.5 {
                4.0 * phase - 1.0
            } else {
                3.0 - 4.0 * phase
            }
        }
        LfoShape::Square => if phase < 0.5 { 1.0 } else { -1.0 },
        LfoShape::SampleAndHold => {
            // Wrap = new sample. We detect a wrap by seeing
            // phase_prev > phase (because phase decreased by 1.0).
            if phase_prev > phase {
                *rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
                let raw = (*rng >> 16) as i16;
                *sh_value = (raw as f32) / 32768.0;
            }
            *sh_value
        }
    }
}

#[inline]
fn next_noise(rng: &mut u32) -> f32 {
    *rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
    let raw = (*rng >> 16) as i16;
    (raw as f32) / 32768.0
}

/// Process one input sample through the TPT state-variable filter.
///
/// Reference: Vadim Zavalishin, *The Art of VA Filter Design* (2012,
/// ch. 5); Andy Simper / Cytomic ("Linear Trapezoidal SVF", 2013).
/// Pre-warped cutoff coefficient `g = tan(π·fc/fs)` + zero-delay
/// feedback via the trapezoidal integrator pair gives unconditional
/// stability across `[20, fs/2)` Hz — no cutoff clamp, no integrator
/// safety nets. Resonance 0..127 maps to Q in [0.5, 10] (0.707 ≈
/// Butterworth, no peak; 5+ is strongly resonant; 10 approaches
/// self-oscillation but never reaches it).
#[inline]
fn svf_process(
    state: &mut SvfState,
    input: f32,
    cutoff_hz: f32,
    resonance: u8,
    mode: FilterMode,
) -> f32 {
    if matches!(mode, FilterMode::Off) {
        return input;
    }
    // Cutoff clamp is just to keep `tan` from exploding at Nyquist
    // — not a stability bound. The TPT form is stable across this
    // whole range.
    let cutoff = cutoff_hz.clamp(20.0, SAMPLE_RATE as f32 * 0.49);
    let g = (core::f32::consts::PI * cutoff / SAMPLE_RATE as f32).tan();
    let q = 0.5 + (resonance as f32 / 127.0) * 9.5;
    let k = 1.0 / q;
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    let a3 = g * a2;
    let v3 = input - state.ic2eq;
    let v1 = a1 * state.ic1eq + a2 * v3;
    let v2 = state.ic2eq + a2 * state.ic1eq + a3 * v3;
    state.ic1eq = 2.0 * v1 - state.ic1eq;
    state.ic2eq = 2.0 * v2 - state.ic2eq;
    match mode {
        FilterMode::LowPass => v2,
        FilterMode::BandPass => v1,
        FilterMode::HighPass => input - k * v1 - v2,
        FilterMode::Off => input, // unreachable
    }
}

/// Polynomial bandlimited step (polyBLEP) correction. Added to a naïve
/// oscillator sample at and around its discontinuities, this kills the
/// alias-induced transient overshoot a raw saw / square exhibits at
/// the wrap point. The discontinuity must have amplitude 2.0 (e.g.
/// saw jumps -1 → +1 at p=1, square jumps ±2 at p=0 and p=0.5).
///
/// `t` is the phase in [0, 1); `dt` is the per-sample phase increment
/// (= freq / sample_rate). Reference: Välimäki & Huovilainen (2007),
/// the popular "Tale's polyBLEP" formulation.
#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

fn mix_synth_voice(
    voice: &mut Voice,
    patches: &[Patch],
    channels: &[Channel],
    out: &mut [f32],
    reverb_in: &mut [f32],
    delay_in: &mut [f32],
) {
    // Extract everything we need before the &mut state borrow.
    let pan = voice.pan;
    let volume = voice.volume;
    let reverb_amt = voice.reverb_send as f32 / 127.0;
    let delay_amt = voice.delay_send as f32 / 127.0;
    let patch_slot = match &voice.kind {
        VoiceKind::Synth(s) => s.patch,
        _ => return,
    };
    let patch = match patches.get(patch_slot as usize) {
        Some(p) => *p,
        None => {
            voice.deactivate();
            return;
        }
    };

    let state = match &mut voice.kind {
        VoiceKind::Synth(s) => s,
        _ => return,
    };

    let dt = 1.0 / SAMPLE_RATE as f32;

    // Per-block premultiplied amp factor. Each voice contributes a
    // normalized f32 in ≈ ±1; the master soft-clipper handles
    // polyphony summation.
    let (gl, gr) = pan_gains(pan);
    let vol = volume as f32 / 127.0;
    let velocity = state.velocity as f32 / 127.0;
    let block_amp = vol * velocity;

    let lfo_rate_hz = patch.lfo.rate_centihz as f32 * 0.01;
    let lfo_inc = lfo_rate_hz * dt;
    let lfo_depth = patch.lfo.depth as f32 / 127.0;

    let glide_samples = if patch.glide_ms > 0 {
        (patch.glide_ms as f32 * 0.001 * SAMPLE_RATE as f32).max(1.0)
    } else {
        1.0
    };

    // Per-block pitch bend: the synth voice records its source channel
    // when it was triggered (Stage 3), and we read the channel's current
    // 14-bit bend value here so SMF / cart `pitch_bend` calls actually
    // shift the oscillator. Held constant across the block since events
    // dispatch at block boundaries (~3 ms granularity is fine for bend).
    let bend_factor = {
        let ch = state.channel;
        let bend = if (ch as usize) < channels.len() {
            channels[ch as usize].pitch_bend as f32
        } else {
            0.0
        };
        // ±8192 → ±PITCH_BEND_SEMITONES semitones.
        let semitones = (bend / 8192.0) * PITCH_BEND_SEMITONES;
        (2.0_f32).powf(semitones / 12.0)
    };

    for i in 0..BLOCK_FRAMES {
        // Glide cur_freq toward target_freq.
        if (state.cur_freq - state.target_freq).abs() > 0.01 {
            let delta = state.target_freq - state.cur_freq;
            state.cur_freq += delta / glide_samples;
        } else {
            state.cur_freq = state.target_freq;
        }

        // LFO.
        let lfo_phase_prev = state.lfo_phase;
        state.lfo_phase += lfo_inc;
        if state.lfo_phase >= 1.0 {
            state.lfo_phase -= 1.0;
        }
        let lfo_raw = lfo_value(
            state.lfo_phase, lfo_phase_prev,
            &mut state.rng, &mut state.sh_value,
            patch.lfo.shape,
        );
        let lfo = lfo_raw * lfo_depth;

        // Oscillator pitch with optional LFO-pitch modulation.
        let pitch_mod_cents = if matches!(patch.lfo.target, LfoTarget::Pitch) {
            lfo * 100.0
        } else {
            0.0
        };
        let pitch_mod_factor = (2.0_f32).powf(pitch_mod_cents / 1200.0);

        // Advance envelopes before osc generation — FM2OP needs the
        // filter envelope to shape the modulator amplitude. The
        // filter stage downstream uses the same envelope value.
        let amp_env = advance_envelope(&mut state.amp_env, &patch.amp_env, state.released, dt);
        let filter_env = advance_envelope(&mut state.filter_env, &patch.filter_env, state.released, dt);

        // Generate per-oscillator samples. FM2OP mode reroutes osc B
        // into modulator duty for osc A's phase argument (§5.1).
        let osc_sample = if matches!(patch.osc[0].mode, OscMode::Fm2Op) {
            // ── FM2OP ─────────────────────────────────────────────
            // Carrier params come from osc[0]; the modulator's freq
            // is *derived* from the carrier × `fm_ratio` (Q8.8). osc[1]'s
            // own octave/detune/mode fields are intentionally ignored
            // here — `fm_ratio` is the canonical control. osc[1].level
            // sets static modulator amplitude scaling, and the filter
            // envelope shapes its trajectory over the note.
            let carrier = patch.osc[0];
            let modulator = patch.osc[1];
            let octave_mult = (2.0_f32).powi(carrier.octave as i32);
            let detune_mult = (2.0_f32).powf(carrier.detune_cents as f32 / 1200.0);
            let carrier_freq = state.cur_freq
                * octave_mult * detune_mult * pitch_mod_factor * bend_factor;
            let ratio = patch.fm_ratio as f32 / 256.0;
            let index = patch.fm_index as f32 / 256.0;
            let mod_freq = carrier_freq * ratio;

            // Advance modulator (osc_phase[1]).
            let mod_inc = mod_freq / SAMPLE_RATE as f32;
            state.osc_phase[1] += mod_inc;
            if state.osc_phase[1] >= 1.0 {
                state.osc_phase[1] -= state.osc_phase[1].floor();
            } else if state.osc_phase[1] < 0.0 {
                state.osc_phase[1] += 1.0;
            }
            let mod_amp = (modulator.level as f32 / 127.0) * index * filter_env;
            let mod_signal = (state.osc_phase[1] * core::f32::consts::TAU).sin() * mod_amp;

            // Advance carrier (osc_phase[0]).
            let car_inc = carrier_freq / SAMPLE_RATE as f32;
            state.osc_phase[0] += car_inc;
            if state.osc_phase[0] >= 1.0 {
                state.osc_phase[0] -= state.osc_phase[0].floor();
            } else if state.osc_phase[0] < 0.0 {
                state.osc_phase[0] += 1.0;
            }
            let phase_arg = state.osc_phase[0] * core::f32::consts::TAU + mod_signal;
            phase_arg.sin() * (carrier.level as f32 / 127.0)
        } else {
            // ── Parallel two-osc rendering (sine / saw / square / tri / noise)
            let mut osc_sum = 0.0;
            for (k, params) in patch.osc.iter().enumerate() {
                if params.level == 0 {
                    continue;
                }
                let octave_mult = (2.0_f32).powi(params.octave as i32);
                let detune_mult = (2.0_f32).powf(params.detune_cents as f32 / 1200.0);
                let freq = state.cur_freq * octave_mult * detune_mult * pitch_mod_factor * bend_factor;
                let phase_inc = freq / SAMPLE_RATE as f32;
                state.osc_phase[k] += phase_inc;
                if state.osc_phase[k] >= 1.0 {
                    state.osc_phase[k] -= state.osc_phase[k].floor();
                } else if state.osc_phase[k] < 0.0 {
                    state.osc_phase[k] += 1.0;
                }
                let p = state.osc_phase[k];
                // Saw + square apply polyBLEP at their discontinuities so
                // the aliasing energy that would otherwise overshoot ±1
                // (and stack up after the filter) gets smoothed out.
                // Sine / triangle are already band-limited; noise is
                // intentionally broadband.
                let raw = match params.mode {
                    OscMode::Sine => (p * core::f32::consts::TAU).sin(),
                    OscMode::Saw => {
                        let naive = 2.0 * p - 1.0;
                        naive - poly_blep(p, phase_inc)
                    }
                    OscMode::Square => {
                        let naive = if p < 0.5 { 1.0 } else { -1.0 };
                        let p_half = if p < 0.5 { p + 0.5 } else { p - 0.5 };
                        naive + poly_blep(p, phase_inc) - poly_blep(p_half, phase_inc)
                    }
                    OscMode::Triangle => {
                        if p < 0.5 {
                            4.0 * p - 1.0
                        } else {
                            3.0 - 4.0 * p
                        }
                    }
                    OscMode::Noise => next_noise(&mut state.rng),
                    OscMode::Fm2Op => {
                        // osc[1] is the modulator — already consumed
                        // by the FM branch above. Ignored here.
                        0.0
                    }
                };
                let level = params.level as f32 / 127.0;
                osc_sum += raw * level;
            }
            // Two oscillators at full level sum to ±2.0; normalize back to
            // roughly ±1 to keep the mix amp scale consistent.
            osc_sum * 0.5
        };

        // Filter cutoff with env + LFO modulation.
        let mut cutoff = patch.filter.cutoff_hz as f32;
        cutoff += filter_env * (patch.filter_env_depth as f32 / 127.0) * 8000.0;
        if matches!(patch.lfo.target, LfoTarget::Filter) {
            cutoff += lfo * 4000.0;
        }
        let filtered = svf_process(&mut state.filter, osc_sample, cutoff, patch.filter.resonance, patch.filter.mode);

        // VCA — amp env × optional LFO-amp modulation.
        let mut amp = amp_env;
        if matches!(patch.lfo.target, LfoTarget::Amp) {
            amp *= (1.0 + lfo).clamp(0.0, 2.0);
        }
        let sample = filtered * amp;

        // Pan with optional LFO-pan modulation.
        let mut pl = gl;
        let mut pr = gr;
        if matches!(patch.lfo.target, LfoTarget::Pan) {
            // LFO ∈ [-1, 1] shifts the L/R balance.
            pl = (gl - lfo).clamp(0.0, 1.0);
            pr = (gr + lfo).clamp(0.0, 1.0);
        }

        let oi = i * 2;
        let l = sample * pl * block_amp;
        let r = sample * pr * block_amp;
        out[oi] += l;
        out[oi + 1] += r;
        // FX sends — pre-scaled by the per-voice send amount snapshot.
        reverb_in[oi]     += l * reverb_amt;
        reverb_in[oi + 1] += r * reverb_amt;
        delay_in[oi]      += l * delay_amt;
        delay_in[oi + 1]  += r * delay_amt;

        // Auto-free once the amp envelope has fully decayed after release.
        if state.amp_env.stage == EnvStage::Done {
            // Drop the rest of the block — voice is silent from here.
            // Mark voice as inactive after the loop ends.
            // (We continue the loop only to keep code straightforward;
            // a `break` is fine since subsequent samples would be zero.)
            voice.deactivate();
            return;
        }
    }
}

// ===========================================================================
// Sampler mix path (Stage 6b)
// ===========================================================================
//
// Combines SFX-style linear-interp sample resampling (Stage 1) with
// the synth voice's envelope / filter / LFO chain (Stage 2). The
// voice owns its own envelope + filter state — sample data is read
// from the cart's sample bank using the slot snapshotted at trigger.

fn mix_sampler_voice(
    voice: &mut Voice,
    samples: &[Option<Sample>],
    patches: &[Patch],
    channels: &[Channel],
    out: &mut [f32],
    reverb_in: &mut [f32],
    delay_in: &mut [f32],
) {
    let pan = voice.pan;
    let volume = voice.volume;
    let reverb_amt = voice.reverb_send as f32 / 127.0;
    let delay_amt = voice.delay_send as f32 / 127.0;

    let (patch_slot, sample_slot) = match &voice.kind {
        VoiceKind::Sampler(s) => (s.patch, s.sample_slot),
        _ => return,
    };
    let patch = match patches.get(patch_slot as usize) {
        Some(p) => *p,
        None => {
            voice.deactivate();
            return;
        }
    };
    let sample = match samples.get(sample_slot as usize).and_then(|s| s.as_ref()) {
        Some(s) => s,
        None => {
            voice.deactivate();
            return;
        }
    };
    let data = &sample.data;
    if data.is_empty() {
        voice.deactivate();
        return;
    }
    let data_len = data.len();

    let state = match &mut voice.kind {
        VoiceKind::Sampler(s) => s,
        _ => return,
    };

    let dt = 1.0 / SAMPLE_RATE as f32;
    let (gl, gr) = pan_gains(pan);
    let vol = volume as f32 / 127.0;
    let velocity = state.velocity as f32 / 127.0;
    // Zone volume offset: -64 → -50%, 0 → unity, +63 → +49.6%.
    let zone_amp = (1.0 + state.volume_offset as f32 / 127.0).max(0.0);
    let block_amp = vol * velocity * zone_amp;

    let lfo_rate_hz = patch.lfo.rate_centihz as f32 * 0.01;
    let lfo_inc = lfo_rate_hz * dt;
    let lfo_depth = patch.lfo.depth as f32 / 127.0;

    // Pitch bend live-read per block — matches synth voice semantics.
    let bend_factor = {
        let ch = state.channel;
        let bend = if (ch as usize) < channels.len() {
            channels[ch as usize].pitch_bend as f32
        } else {
            0.0
        };
        let semitones = (bend / 8192.0) * PITCH_BEND_SEMITONES;
        (2.0_f32).powf(semitones / 12.0) as f64
    };
    let pitch_step = state.base_pitch_ratio * bend_factor;

    let mut deactivated = false;
    for i in 0..BLOCK_FRAMES {
        // Position wrap / end-of-sample.
        if state.loop_enabled {
            let end = (state.loop_end as usize).min(data_len);
            let start = state.loop_start as usize;
            if end > start && state.position >= end as f64 {
                let span = (end - start) as f64;
                state.position = start as f64
                    + (state.position - start as f64).rem_euclid(span);
            }
        } else if state.position + 1.0 >= data_len as f64 {
            deactivated = true;
            break;
        }

        // Linear-interp sample read (matches `mix_sfx_voice` exactly).
        let s0 = state.position.floor() as usize;
        let frac = (state.position - s0 as f64) as f32;
        let a = data.get(s0).copied().unwrap_or(128);
        let b = data.get(s0 + 1).copied().unwrap_or(a);
        let f0 = ((a as i32 - 128) as f32) * (1.0 / 128.0);
        let f1 = ((b as i32 - 128) as f32) * (1.0 / 128.0);
        let raw = f0 + (f1 - f0) * frac;

        // LFO.
        let lfo_phase_prev = state.lfo_phase;
        state.lfo_phase += lfo_inc;
        if state.lfo_phase >= 1.0 {
            state.lfo_phase -= 1.0;
        }
        let lfo_raw = lfo_value(
            state.lfo_phase, lfo_phase_prev,
            &mut state.rng, &mut state.sh_value,
            patch.lfo.shape,
        );
        let lfo = lfo_raw * lfo_depth;

        // Envelopes.
        let amp_env = advance_envelope(&mut state.amp_env, &patch.amp_env, state.released, dt);
        let filter_env = advance_envelope(&mut state.filter_env, &patch.filter_env, state.released, dt);

        // Filter cutoff with env + LFO modulation.
        let mut cutoff = patch.filter.cutoff_hz as f32;
        cutoff += filter_env * (patch.filter_env_depth as f32 / 127.0) * 8000.0;
        if matches!(patch.lfo.target, LfoTarget::Filter) {
            cutoff += lfo * 4000.0;
        }
        let filtered = svf_process(&mut state.filter, raw, cutoff, patch.filter.resonance, patch.filter.mode);

        // VCA.
        let mut amp = amp_env;
        if matches!(patch.lfo.target, LfoTarget::Amp) {
            amp *= (1.0 + lfo).clamp(0.0, 2.0);
        }
        let sample_out = filtered * amp;

        // Pan.
        let mut pl = gl;
        let mut pr = gr;
        if matches!(patch.lfo.target, LfoTarget::Pan) {
            pl = (gl - lfo).clamp(0.0, 1.0);
            pr = (gr + lfo).clamp(0.0, 1.0);
        }

        let oi = i * 2;
        let l = sample_out * pl * block_amp;
        let r = sample_out * pr * block_amp;
        out[oi] += l;
        out[oi + 1] += r;
        reverb_in[oi]     += l * reverb_amt;
        reverb_in[oi + 1] += r * reverb_amt;
        delay_in[oi]      += l * delay_amt;
        delay_in[oi + 1]  += r * delay_amt;

        // Advance the sample read head.
        state.position += pitch_step;

        if state.amp_env.stage == EnvStage::Done {
            voice.deactivate();
            return;
        }
    }

    if deactivated {
        voice.deactivate();
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp_sample(len: usize, rate: SampleRate) -> Sample {
        let data = (0..len).map(|i| 128 + (i.min(127) as u8)).collect();
        Sample { data, rate, loop_points: None }
    }

    fn loud_sample(rate: SampleRate, len: usize) -> Sample {
        Sample { data: vec![255; len], rate, loop_points: None }
    }

    // ── Stage 1 SFX tests (carried over) ────────────────────────────

    #[test]
    fn single_voice_plays_and_auto_frees() {
        let mut a = AudioState::new();
        a.register_sample(0, ramp_sample(8, SampleRate::Khz22_05));
        let id = a.sfx_play(0, 127, 0, 0, false);
        assert_ne!(id, VoiceId::NONE);
        assert_eq!(a.active_voice_count(), 1);

        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        assert_eq!(a.active_voice_count(), 0);
        assert!(buf.iter().any(|&s| s != 0), "expected audible output");
    }

    #[test]
    fn empty_slot_yields_none() {
        let mut a = AudioState::new();
        let id = a.sfx_play(7, 127, 0, 0, false);
        assert_eq!(id, VoiceId::NONE);
        assert_eq!(a.active_voice_count(), 0);
    }

    #[test]
    fn voice_stealing_when_pool_full() {
        let mut a = AudioState::new();
        a.register_sample(0, loud_sample(SampleRate::Khz22_05, 4096));
        let mut ids = Vec::new();
        for _ in 0..VOICE_POOL_SIZE {
            ids.push(a.sfx_play(0, 50, 0, 0, false));
        }
        assert_eq!(a.active_voice_count(), VOICE_POOL_SIZE);
        let _stolen_owner = a.sfx_play(0, 50, 0, 0, false);
        assert_eq!(a.active_voice_count(), VOICE_POOL_SIZE);
        assert_eq!(a.voices_stolen, 1);
        a.sfx_stop(ids[0]); // stale id, no-op
        assert_eq!(a.active_voice_count(), VOICE_POOL_SIZE);
    }

    #[test]
    fn stale_voice_id_set_volume_is_noop() {
        let mut a = AudioState::new();
        a.register_sample(0, loud_sample(SampleRate::Khz22_05, 4));
        let id = a.sfx_play(0, 100, 0, 0, false);
        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        a.sfx_set_volume(id, 0);
        assert_eq!(a.active_voice_count(), 0);
    }

    #[test]
    fn looping_sample_does_not_auto_free() {
        let mut a = AudioState::new();
        let mut s = loud_sample(SampleRate::Khz22_05, 8);
        s.loop_points = Some((0, 8));
        a.register_sample(0, s);
        let _id = a.sfx_play(0, 100, 0, 0, true);
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..10 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 1);
    }

    #[test]
    fn sample_replacement_works() {
        let mut a = AudioState::new();
        a.register_sample(0, loud_sample(SampleRate::Khz22_05, 16));
        a.register_sample(0, loud_sample(SampleRate::Khz11_025, 32));
        let id = a.sfx_play(0, 0, 0, 0, false);
        let ratio = match a.voices[id.slot()].kind {
            VoiceKind::Sfx(s) => s.pitch_ratio,
            _ => unreachable!(),
        };
        assert!((ratio - 0.5).abs() < 1e-6);
    }

    #[test]
    fn multiple_voices_mix_without_overflow() {
        let mut a = AudioState::new();
        a.register_sample(0, loud_sample(SampleRate::Khz22_05, 4096));
        for _ in 0..VOICE_POOL_SIZE {
            a.sfx_play(0, 127, 0, 0, false);
        }
        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        for s in buf {
            assert!(s as i32 <= i16::MAX as i32 && s as i32 >= i16::MIN as i32);
        }
    }

    #[test]
    fn out_of_range_slot_silently_rejected() {
        let mut a = AudioState::new();
        a.register_sample(200, loud_sample(SampleRate::Khz22_05, 16));
        let id = a.sfx_play(200, 100, 0, 0, false);
        assert_eq!(id, VoiceId::NONE);
    }

    #[test]
    fn voice_id_packs_slot_and_generation() {
        let id = VoiceId::pack(7, 42);
        assert_eq!(id.slot(), 7);
        assert_eq!(id.generation(), 42);
    }

    #[test]
    fn generation_bump_skips_zero() {
        assert_eq!(bump_generation(255), 1);
        assert_eq!(bump_generation(1), 2);
    }

    #[test]
    fn pan_gains_full_left_right_and_center() {
        let (l, r) = pan_gains(-64);
        assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6);
        let (l, r) = pan_gains(63);
        assert!((r - 1.0).abs() < 1e-6 && l < 0.05);
        let (l, r) = pan_gains(0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
    }

    // ── Stage 2 synth tests ────────────────────────────────────────

    #[test]
    fn note_to_freq_a4_is_440() {
        let f = note_to_freq(69);
        assert!((f - 440.0).abs() < 1e-3);
    }

    #[test]
    fn note_to_freq_octave_doubles() {
        assert!((note_to_freq(81) / note_to_freq(69) - 2.0).abs() < 1e-3);
        assert!((note_to_freq(57) / note_to_freq(69) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn voice_trigger_produces_audible_output() {
        let mut a = AudioState::new();
        // Default patch is a pure sine. Trigger a middle-C note.
        let id = a.voice_trigger(0, 60, 100);
        assert_ne!(id, VoiceId::NONE);
        assert_eq!(a.active_voice_count(), 1);

        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        // After the first block (~3 ms) the attack ramp should have
        // produced some non-zero output.
        let max = buf.iter().map(|s| s.abs()).max().unwrap();
        assert!(max > 0, "expected audible synth output, got all zeros");
    }

    #[test]
    fn voice_release_drives_envelope_to_done() {
        let mut a = AudioState::new();
        // Punchy ADSR so release finishes within a few blocks.
        a.patch_set_amp_env(0, /*a*/1, /*d*/1, /*s*/100, /*r*/5);
        let id = a.voice_trigger(0, 60, 100);
        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        a.voice_release(id);
        // ~50 ms of release should free the voice.
        for _ in 0..20 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 0, "voice should auto-free after release");
    }

    #[test]
    fn unreleased_voice_stays_in_sustain() {
        let mut a = AudioState::new();
        a.patch_set_amp_env(0, /*a*/1, /*d*/1, /*s*/100, /*r*/100);
        let _id = a.voice_trigger(0, 60, 100);
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..200 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 1);
    }

    #[test]
    fn out_of_range_patch_yields_none_on_voice_trigger() {
        let mut a = AudioState::new();
        let id = a.voice_trigger(50, 60, 100);
        assert_eq!(id, VoiceId::NONE);
    }

    #[test]
    fn patch_reset_restores_default() {
        let mut a = AudioState::new();
        a.patch_set_osc(0, 0, OscMode::Saw, 10, 1, 60);
        a.patch_reset(0);
        let p = a.patch(0).unwrap();
        assert_eq!(p.osc[0].mode, OscMode::Sine);
        assert_eq!(p.osc[0].detune_cents, 0);
        assert_eq!(p.osc[0].octave, 0);
        assert_eq!(p.osc[0].level, 127);
    }

    #[test]
    fn patch_copy_replicates_settings() {
        let mut a = AudioState::new();
        a.patch_set_osc(0, 0, OscMode::Square, 5, 2, 90);
        a.patch_copy(0, 7);
        let p = a.patch(7).unwrap();
        assert_eq!(p.osc[0].mode, OscMode::Square);
        assert_eq!(p.osc[0].detune_cents, 5);
        assert_eq!(p.osc[0].octave, 2);
        assert_eq!(p.osc[0].level, 90);
    }

    #[test]
    fn lowpass_filter_attenuates_high_input() {
        let mut a = AudioState::new();
        // Square at 5 kHz (high) through a 200 Hz LP should be much
        // quieter than the same square unfiltered.
        a.patch_set_osc(0, 0, OscMode::Square, 0, 0, 127);

        // Unfiltered baseline.
        let id1 = a.voice_trigger(0, 100, 127); // very high note
        let mut buf = [0i16; BLOCK_SAMPLES];
        // Skip the attack ramp so we sample at steady state.
        for _ in 0..3 { a.render_block(&mut buf); }
        let peak_open: i16 = *buf.iter().max_by_key(|s| s.abs()).unwrap();
        a.voice_release(id1);
        for _ in 0..10 { a.render_block(&mut buf); }

        // With LP at 200 Hz.
        a.patch_set_filter(0, FilterMode::LowPass, 200, 0);
        let _id2 = a.voice_trigger(0, 100, 127);
        for _ in 0..3 { a.render_block(&mut buf); }
        let peak_lp: i16 = *buf.iter().max_by_key(|s| s.abs()).unwrap();

        // LP-filtered should be at least 50% quieter than open.
        assert!(
            (peak_lp.abs() as f32) < (peak_open.abs() as f32) * 0.5,
            "LP didn't attenuate ({peak_lp} vs open {peak_open})"
        );
    }

    #[test]
    fn synth_voice_steals_at_pool_capacity() {
        let mut a = AudioState::new();
        // Long-sustain so nothing auto-frees.
        a.patch_set_amp_env(0, 1, 1, 127, 1000);
        for _ in 0..VOICE_POOL_SIZE {
            a.voice_trigger(0, 60, 100);
        }
        assert_eq!(a.active_voice_count(), VOICE_POOL_SIZE);
        a.voice_trigger(0, 60, 100);
        assert_eq!(a.active_voice_count(), VOICE_POOL_SIZE);
        assert!(a.voices_stolen >= 1);
    }

    #[test]
    fn glide_moves_freq_toward_target_over_time() {
        let mut a = AudioState::new();
        // 100 ms glide. Trigger note 60 then immediately stomp the
        // voice's target to note 72 (octave up) to check glide.
        a.patch_set_glide(0, 100);
        let id = a.voice_trigger(0, 60, 100);
        let slot = id.slot();
        // Force a new target by reaching into the voice directly.
        if let VoiceKind::Synth(state) = &mut a.voices[slot].kind {
            state.target_freq = note_to_freq(72);
        }
        let target = note_to_freq(72);
        let start = note_to_freq(60);
        // Render ~50 ms (about halfway through the glide).
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..17 { a.render_block(&mut buf); }
        let mid = match &a.voices[slot].kind {
            VoiceKind::Synth(s) => s.cur_freq,
            _ => unreachable!(),
        };
        // Halfway: should be between start and target, closer to target.
        assert!(mid > start, "glide didn't move ({mid} vs start {start})");
        assert!(mid < target * 1.01, "glide overshot ({mid} vs target {target})");
    }

    #[test]
    fn patch_set_filter_clamps_resonance() {
        let mut a = AudioState::new();
        a.patch_set_filter(0, FilterMode::LowPass, 1000, 200);
        let p = a.patch(0).unwrap();
        assert_eq!(p.filter.resonance, 127);
    }

    #[test]
    fn svf_stable_at_high_cutoff_and_resonance() {
        // With the previous Chamberlin SVF, this combination drove the
        // filter past its stability bound and every output sample
        // saturated at i16::MAX. The TPT SVF should stay bounded.
        let mut a = AudioState::new();
        a.patch_set_osc(0, 0, OscMode::Saw, 0, 0, 127);
        a.patch_set_filter(0, FilterMode::LowPass, 9000, 127);
        a.patch_set_filter_env(0, /*a*/1, /*d*/200, /*s*/100, /*r*/100, /*depth*/127);
        let _id = a.voice_trigger(0, 84, 127); // high note, full velocity
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..16 {
            a.render_block(&mut buf);
        }
        // Soft-clipper bounds output to ±i16::MAX by construction;
        // the test ensures we don't see a buffer where the vast
        // majority of samples saturated (= filter ran away).
        let max_count = buf.iter().filter(|&&s| s.abs() >= i16::MAX - 1).count();
        assert!(
            max_count < BLOCK_SAMPLES / 4,
            "filter looks unstable: {max_count}/{BLOCK_SAMPLES} samples saturated"
        );
    }

    #[test]
    fn polyblep_doesnt_break_low_freq_saw() {
        // PolyBLEP correction kicks in over a window of `dt` samples
        // around discontinuities. At low frequencies the window is
        // tiny — the corrected saw should still be audibly a saw.
        let mut a = AudioState::new();
        a.patch_set_osc(0, 0, OscMode::Saw, 0, 0, 127);
        a.patch_set_filter(0, FilterMode::Off, 0, 0);
        let _id = a.voice_trigger(0, 60, 127);
        let mut buf = [0i16; BLOCK_SAMPLES];
        // Skip the attack ramp.
        for _ in 0..3 {
            a.render_block(&mut buf);
        }
        let peak = buf.iter().map(|s| s.abs()).max().unwrap();
        // Audible but not saturated — tanh on a single voice peaks
        // around i16::MAX × 0.76 ≈ 25 000.
        assert!(peak > 10_000, "low-freq saw too quiet: peak {peak}");
        assert!(peak < i16::MAX - 100, "low-freq saw is clipping: peak {peak}");
    }

    #[test]
    fn master_softclip_bounds_polyphony() {
        // Stack 16 unison saws at full volume + velocity. Before the
        // soft-clip master they would sum past i16::MAX every sample
        // (with the old MIX_HEADROOM=16 budget it just barely fit; any
        // resonance overshoot blew past). With tanh master, the sum
        // is bounded regardless of how many voices add up.
        let mut a = AudioState::new();
        a.patch_set_osc(0, 0, OscMode::Saw, 0, 0, 127);
        a.patch_set_filter(0, FilterMode::Off, 0, 0);
        for _ in 0..VOICE_POOL_SIZE {
            a.voice_trigger(0, 60, 127);
        }
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..3 {
            a.render_block(&mut buf);
        }
        // Every sample must remain a valid i16. The cast in
        // render_block uses `(tanh(s) * i16::MAX as f32) as i16`,
        // which is bounded by definition. This test exists to lock
        // that invariant in case someone tries to "optimize" the
        // master stage away.
        for &s in &buf {
            let _: i16 = s; // type-level proof
            assert!(s as i32 <= i16::MAX as i32 && s as i32 >= i16::MIN as i32);
        }
    }

    // ── Stage 3 MIDI surface ───────────────────────────────────────

    #[test]
    fn note_on_off_round_trip() {
        let mut a = AudioState::new_silent();
        let id = a.note_on(0, 60, 100);
        assert_ne!(id, VoiceId::NONE);
        assert_eq!(a.active_voice_count(), 1);
        a.note_off(0, 60);
        // Voice is in Release stage; let it run out.
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..200 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 0);
    }

    #[test]
    fn note_off_only_matches_same_channel_and_note() {
        let mut a = AudioState::new_silent();
        a.note_on(0, 60, 100);
        a.note_on(1, 60, 100);
        a.note_on(0, 64, 100);
        assert_eq!(a.active_voice_count(), 3);
        // Releasing channel-0 note-60 only affects one voice — the
        // channel-1 voice on the same note and the channel-0 voice
        // on a different note both stay live.
        a.note_off(0, 60);
        let mut buf = [0i16; BLOCK_SAMPLES];
        // Run the release envelope to completion.
        for _ in 0..200 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 2);
    }

    #[test]
    fn all_notes_off_releases_only_the_channel() {
        let mut a = AudioState::new_silent();
        a.note_on(0, 60, 100);
        a.note_on(0, 64, 100);
        a.note_on(2, 60, 100);
        assert_eq!(a.active_voice_count(), 3);
        a.all_notes_off(0);
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..200 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 1);
    }

    #[test]
    fn note_on_velocity_zero_acts_as_note_off() {
        let mut a = AudioState::new_silent();
        a.note_on(0, 60, 100);
        assert_eq!(a.active_voice_count(), 1);
        // Per §5.2 / MIDI convention: velocity 0 on note_on = release.
        a.note_on(0, 60, 0);
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..200 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 0);
    }

    #[test]
    fn sustain_pedal_delays_release_until_off() {
        let mut a = AudioState::new_silent();
        // Short release so the test doesn't wait on the default
        // 80 ms exponential tail.
        a.patch_set_amp_env(0, 1, 1, 100, 5);
        a.note_on(0, 60, 100);
        a.cc(0, CC_SUSTAIN, 127);
        a.note_off(0, 60);
        // Voice should still be alive — sustain is holding it.
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..20 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 1);
        // Release sustain — voice should now drop.
        a.cc(0, CC_SUSTAIN, 0);
        for _ in 0..40 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 0);
    }

    #[test]
    fn program_change_rebinds_channel_and_clears_drum_flag() {
        let mut a = AudioState::new_silent();
        assert_eq!(a.channel_patch(DRUM_CHANNEL), Some(DRUM_CHANNEL % PATCH_SLOTS as u8));
        // Channel 10 is drum-mode by default.
        a.program_change(DRUM_CHANNEL, 3);
        assert_eq!(a.channel_patch(DRUM_CHANNEL), Some(3));
        // Now `note_on` on channel 10 should route through patch 3,
        // not the drum bank. Triggering note 36 (would be a kick in
        // drum mode) creates a synth voice instead.
        let id = a.note_on(DRUM_CHANNEL, 36, 100);
        assert_ne!(id, VoiceId::NONE);
        // Inspect that the voice is a SynthVoiceState (not Sfx/drum).
        let slot = id.0 as usize & 0xFF;
        assert!(matches!(a.voices[slot].kind, VoiceKind::Synth(_)));
    }

    #[test]
    fn channel_10_default_routes_to_drum_bank() {
        // Need the real drum kit init for this one — drum_bank is empty
        // under `new_silent`.
        let mut a = AudioState::new();
        let id = a.note_on(DRUM_CHANNEL, 36, 100); // kick
        assert_ne!(id, VoiceId::NONE);
        let slot = id.0 as usize & 0xFF;
        // The voice should be an SFX voice sourced from the drum bank.
        match &a.voices[slot].kind {
            VoiceKind::Sfx(state) => {
                assert!(matches!(state.source, SampleSource::Drum(_)));
            }
            _ => panic!("expected drum-bank SFX voice"),
        }
    }

    #[test]
    fn note_off_with_no_match_is_silent_noop() {
        let mut a = AudioState::new_silent();
        // No voice on channel 0 note 60 — release should not panic
        // and should not affect any other state.
        a.note_off(0, 60);
        a.all_notes_off(5);
        assert_eq!(a.active_voice_count(), 0);
    }

    #[test]
    fn cc_volume_clamps_at_127() {
        let mut a = AudioState::new_silent();
        a.cc(0, CC_VOLUME, 250);
        assert_eq!(a.channels[0].volume, 127);
    }

    #[test]
    fn drum_kit_synthesizes_audible_samples() {
        // Full constructor — drum kit fills the bank.
        let a = AudioState::new();
        let kick_slot = 36 - 35;
        let drum = a.drum_bank[kick_slot].as_ref().expect("kick sample missing");
        // Sample is mono 8-bit PCM at 22.05 kHz; should have a clear
        // non-silent peak (the kick's body) and a non-empty buffer.
        assert!(!drum.data.is_empty());
        let max_excursion = drum.data
            .iter()
            .map(|&b| (b as i32 - 128).unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(max_excursion > 40, "kick sample too quiet: peak {max_excursion}");
    }

    #[test]
    fn midi_pan_to_signed_centers_correctly() {
        assert_eq!(midi_pan_to_signed(64), 0);
        assert_eq!(midi_pan_to_signed(0), -64);
        assert_eq!(midi_pan_to_signed(127), 63);
    }

    #[test]
    fn midi_volume_combined_multiplies() {
        assert_eq!(midi_volume_combined(127, 127), 127);
        assert_eq!(midi_volume_combined(127, 64), 64);
        assert_eq!(midi_volume_combined(64, 64), 32);
        assert_eq!(midi_volume_combined(0, 127), 0);
    }

    // ── Stage 4a music / SMF playback tests ────────────────────────

    /// Helper: SMF VLQ encoder.
    fn smf_vlq(mut v: u32) -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![(v & 0x7F) as u8];
        v >>= 7;
        while v > 0 {
            bytes.insert(0, ((v & 0x7F) | 0x80) as u8);
            v >>= 7;
        }
        bytes
    }

    /// Helper: build a minimal SMF type 0 with the supplied
    /// (delta_ticks, raw_event_bytes) tuples + an EOT marker.
    fn build_test_smf(division: u16, events: &[(u32, &[u8])]) -> Vec<u8> {
        let mut header = b"MThd".to_vec();
        header.extend(&6u32.to_be_bytes());
        header.extend(&0u16.to_be_bytes()); // format 0
        header.extend(&1u16.to_be_bytes()); // 1 track
        header.extend(&division.to_be_bytes());

        let mut track_body: Vec<u8> = Vec::new();
        for (d, e) in events {
            track_body.extend(smf_vlq(*d));
            track_body.extend_from_slice(e);
        }
        // End-of-track
        track_body.extend(smf_vlq(0));
        track_body.extend(&[0xFF, 0x2F, 0x00]);

        let mut out = header;
        out.extend(b"MTrk");
        out.extend(&(track_body.len() as u32).to_be_bytes());
        out.extend(track_body);
        out
    }

    fn render_n_blocks(a: &mut AudioState, n: usize) {
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..n {
            a.render_block(&mut buf);
        }
    }

    #[test]
    fn music_load_accepts_valid_smf_and_rejects_garbage() {
        let mut a = AudioState::new_silent();
        let smf = build_test_smf(96, &[(0, &[0x90, 60, 100])]);
        assert!(a.load_song(0, &smf));
        assert!(!a.load_song(0, b"not a midi file"));
        // Out-of-range slot.
        assert!(!a.load_song(99, &smf));
    }

    #[test]
    fn music_play_triggers_note_at_first_block() {
        let mut a = AudioState::new_silent();
        // tick 0: note on; tick 96: note off (≈ 0.5 s @ 120 BPM with PPQ=96).
        let smf = build_test_smf(
            96,
            &[(0, &[0x90, 60, 100]), (96, &[0x80, 60, 64])],
        );
        a.load_song(0, &smf);
        a.music_play(0, false);
        assert!(a.music_is_playing());

        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        // Note-on at tick 0 should have allocated a voice already.
        assert_eq!(a.active_voice_count(), 1);
    }

    #[test]
    fn music_position_beats_advances_with_tempo() {
        let mut a = AudioState::new_silent();
        // PPQ 96. Place a note ~1 beat in so the playhead has somewhere
        // to advance to.
        let smf = build_test_smf(
            96,
            &[(0, &[0xB0, 7, 100]), (480, &[0x80, 0, 0])],
        );
        a.load_song(0, &smf);
        a.music_play(0, false);

        // 120 BPM = 2 beats/sec; one block ≈ 2.9 ms ⇒ ~0.0058 beats.
        // Render 100 blocks (~290 ms ⇒ ~0.58 beats).
        render_n_blocks(&mut a, 100);
        let beats = a.music_position_beats();
        assert!(
            beats > 0.4 && beats < 0.7,
            "expected ~0.58 beats, got {beats}",
        );
    }

    #[test]
    fn music_set_tempo_via_meta_event() {
        let mut a = AudioState::new_silent();
        // Set tempo to 60 BPM (1_000_000 us/qn) then place a note 1 beat
        // out so we can verify the playhead's beats-per-second halved.
        let mut events: Vec<(u32, Vec<u8>)> = Vec::new();
        events.push((0, vec![0xFF, 0x51, 0x03, 0x0F, 0x42, 0x40])); // 1_000_000 us
        events.push((96, vec![0x90, 60, 100]));
        let evs: Vec<(u32, &[u8])> =
            events.iter().map(|(d, e)| (*d, e.as_slice())).collect();
        let smf = build_test_smf(96, &evs);
        a.load_song(0, &smf);
        a.music_play(0, false);

        // Render 100 blocks (~290 ms). At 60 BPM that's ~0.29 beats.
        render_n_blocks(&mut a, 100);
        let beats = a.music_position_beats();
        assert!(
            beats > 0.2 && beats < 0.4,
            "expected ~0.29 beats at 60 BPM, got {beats}",
        );
    }

    #[test]
    fn music_loop_wraps_to_start() {
        let mut a = AudioState::new_silent();
        // Very short song: one note at tick 0, EOT at tick 4. With PPQ
        // 96 @ 120 BPM that's about 21 ms. After ~10 blocks (29 ms)
        // we should have wrapped at least once.
        let smf = build_test_smf(
            96,
            &[(0, &[0x90, 60, 100]), (4, &[0x80, 60, 0])],
        );
        a.load_song(0, &smf);
        a.music_play(0, true);

        render_n_blocks(&mut a, 50);
        // Still active because looping.
        assert!(a.music_is_playing());
    }

    #[test]
    fn music_one_shot_stops_at_end() {
        let mut a = AudioState::new_silent();
        let smf = build_test_smf(
            96,
            &[(0, &[0x90, 60, 100]), (4, &[0x80, 60, 0])],
        );
        a.load_song(0, &smf);
        a.music_play(0, false);
        render_n_blocks(&mut a, 50);
        // Non-looping song should have stopped.
        assert!(!a.music_is_playing());
    }

    #[test]
    fn music_stop_silences_voices() {
        let mut a = AudioState::new_silent();
        let smf = build_test_smf(96, &[(0, &[0x90, 60, 100])]);
        a.load_song(0, &smf);
        a.music_play(0, false);
        render_n_blocks(&mut a, 2);
        assert!(a.active_voice_count() > 0);
        a.music_stop();
        // music_stop → all_notes_off → release stage. Render enough
        // blocks for the default-patch release envelope to finish.
        render_n_blocks(&mut a, 60);
        assert!(!a.music_is_playing());
    }

    #[test]
    fn music_load_replacing_active_slot_stops_playback() {
        let mut a = AudioState::new_silent();
        let smf = build_test_smf(96, &[(0, &[0x90, 60, 100])]);
        a.load_song(0, &smf);
        a.music_play(0, true);
        assert!(a.music_is_playing());
        // Replacing the slot under us should stop playback so the cart
        // doesn't dispatch events against a freshly-rebuilt event list.
        a.load_song(0, &smf);
        assert!(!a.music_is_playing());
    }

    #[test]
    fn music_tempo_scale_speeds_up_playback() {
        let mut a = AudioState::new_silent();
        let smf = build_test_smf(
            96,
            &[(0, &[0xB0, 7, 100]), (960, &[0x80, 0, 0])],
        );
        a.load_song(0, &smf);
        a.music_play(0, false);
        a.music_set_tempo_scale(4.0);

        render_n_blocks(&mut a, 100);
        let beats = a.music_position_beats();
        // 4x speed → 4x beats per real second. 290 ms * 8 beats/sec ≈ 2.3.
        assert!(beats > 2.0, "expected >2 beats at 4x speed, got {beats}");
    }

    // ── Stage 5 FX bus integration tests ──────────────────────────

    #[test]
    fn cc_91_and_93_update_channel_sends() {
        let mut a = AudioState::new_silent();
        a.cc(0, CC_REVERB_SEND, 90);
        a.cc(0, CC_DELAY_SEND, 60);
        assert_eq!(a.channels[0].reverb_send, 90);
        assert_eq!(a.channels[0].delay_send, 60);
    }

    #[test]
    fn note_on_snapshots_channel_sends_to_voice() {
        let mut a = AudioState::new_silent();
        a.cc(0, CC_REVERB_SEND, 100);
        a.cc(0, CC_DELAY_SEND, 40);
        let id = a.note_on(0, 60, 100);
        let idx = a.lookup_voice(id).expect("voice should be active");
        assert_eq!(a.voices[idx].reverb_send, 100);
        assert_eq!(a.voices[idx].delay_send, 40);
        // Subsequent CC changes do NOT modulate this voice — sends
        // are snapshotted at trigger time.
        a.cc(0, CC_REVERB_SEND, 0);
        assert_eq!(a.voices[idx].reverb_send, 100);
    }

    #[test]
    fn dry_voice_no_send_produces_no_wet_diff() {
        let mut a = AudioState::new_silent();
        // No send CC sent; default send is 0.
        a.note_on(0, 60, 100);
        let mut buf_dry = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf_dry);
        let dry_peak = buf_dry.iter().map(|&s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(dry_peak > 1000, "dry voice should produce audio: {dry_peak}");
    }

    #[test]
    fn reverb_send_adds_audible_tail() {
        let mut a = AudioState::new_silent();
        // Send through a snappy short patch so the dry signal ends
        // quickly and the reverb tail is the dominant content after
        // a few blocks.
        a.patch_set_amp_env(0, /*a*/1, /*d*/30, /*s*/0, /*r*/20);
        a.cc(0, CC_REVERB_SEND, 127);
        a.reverb_set(100, 20); // long, bright tail
        a.note_on(0, 60, 127);
        // Render past the dry signal's release.
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..30 { a.render_block(&mut buf); }
        // Now sample the next ~40 blocks looking for non-silence
        // (reverb tail energy).
        let mut total: u64 = 0;
        for _ in 0..40 {
            a.render_block(&mut buf);
            for &s in &buf { total += s.unsigned_abs() as u64; }
        }
        assert!(
            total > 10_000,
            "reverb send should produce a tail well after note ends, got {total}",
        );
    }

    #[test]
    fn delay_send_produces_ping_pong_tap() {
        let mut a = AudioState::new_silent();
        // Short note, full delay send, 50 ms delay so the tap arrives
        // within a manageable number of blocks (~17 blocks of 64 frames).
        a.patch_set_amp_env(0, 1, 20, 0, 10);
        a.cc(0, CC_DELAY_SEND, 127);
        a.delay_set(50, 80);
        a.note_on(0, 60, 127);
        let mut buf = [0i16; BLOCK_SAMPLES];
        // Let the dry signal play + decay (~30ms), then look at delay taps.
        for _ in 0..15 { a.render_block(&mut buf); }
        let mut tap_energy: u64 = 0;
        for _ in 0..15 {
            a.render_block(&mut buf);
            for &s in &buf { tap_energy += s.unsigned_abs() as u64; }
        }
        assert!(
            tap_energy > 5_000,
            "delay send should produce a delayed tap, got {tap_energy}",
        );
    }

    // ── Stage 6b sampler patch tests ──────────────────────────────

    fn pitched_sample(len: usize) -> Sample {
        // A clean ramp that's audibly pitch-shifted when resampled —
        // good for verifying resampling without checking exact spectra.
        let data = (0..len)
            .map(|i| ((i as f32 / len as f32 * core::f32::consts::TAU * 4.0).sin() * 100.0 + 128.0) as u8)
            .collect();
        Sample { data, rate: SampleRate::Khz22_05, loop_points: None }
    }

    #[test]
    fn sampler_patch_triggers_voice_via_voice_trigger() {
        let mut a = AudioState::new_silent();
        a.register_sample(0, pitched_sample(2048));
        a.patch_set_kind(1, PatchKind::Sampler);
        a.patch_set_zone(1, 0, KeyZone {
            low_note: 0, high_note: 127, root_note: 60,
            sample_slot: 0, volume_offset: 0,
            loop_start: 0, loop_end: 0, loop_enabled: false,
        });
        a.patch_set_zone_count(1, 1);
        let id = a.voice_trigger(1, 60, 100);
        assert_ne!(id, VoiceId::NONE);
        assert_eq!(a.active_voice_count(), 1);
        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        assert!(buf.iter().any(|&s| s != 0), "sampler voice should produce audio");
    }

    #[test]
    fn sampler_missing_zone_returns_none() {
        let mut a = AudioState::new_silent();
        a.register_sample(0, pitched_sample(512));
        a.patch_set_kind(1, PatchKind::Sampler);
        a.patch_set_zone(1, 0, KeyZone {
            low_note: 60, high_note: 72, root_note: 60,
            sample_slot: 0, volume_offset: 0,
            loop_start: 0, loop_end: 0, loop_enabled: false,
        });
        a.patch_set_zone_count(1, 1);
        // Note 90 is outside the zone's [60, 72] range.
        let id = a.voice_trigger(1, 90, 100);
        assert_eq!(id, VoiceId::NONE);
        assert_eq!(a.active_voice_count(), 0);
    }

    #[test]
    fn sampler_pitch_ratio_doubles_one_octave_up() {
        // Trigger a note one octave above root and confirm the voice
        // chews through twice as much sample data per block as a
        // root-note trigger does.
        fn position_after_one_block(note: u8) -> f64 {
            let mut a = AudioState::new_silent();
            a.register_sample(0, pitched_sample(4096));
            a.patch_set_kind(1, PatchKind::Sampler);
            a.patch_set_zone(1, 0, KeyZone {
                low_note: 0, high_note: 127, root_note: 60,
                sample_slot: 0, volume_offset: 0,
                loop_start: 0, loop_end: 0, loop_enabled: false,
            });
            a.patch_set_zone_count(1, 1);
            // Patch defaults give an Attack stage that ramps quickly;
            // the position advance is independent of envelope.
            let _ = a.voice_trigger(1, note, 100);
            let mut buf = [0i16; BLOCK_SAMPLES];
            a.render_block(&mut buf);
            for v in &a.voices {
                if let VoiceKind::Sampler(s) = &v.kind {
                    return s.position;
                }
            }
            0.0
        }
        let root_pos = position_after_one_block(60);
        let octave_up_pos = position_after_one_block(72);
        let ratio = octave_up_pos / root_pos;
        assert!(
            (ratio - 2.0).abs() < 0.01,
            "expected position ratio ~2.0 for +12 semitones, got {ratio} (root {root_pos}, +12 {octave_up_pos})",
        );
    }

    #[test]
    fn sampler_voice_auto_frees_at_sample_end() {
        let mut a = AudioState::new_silent();
        // Tiny sample → finishes within a single render block at
        // root-note playback rate.
        a.register_sample(0, pitched_sample(32));
        a.patch_set_kind(1, PatchKind::Sampler);
        a.patch_set_zone(1, 0, KeyZone {
            low_note: 0, high_note: 127, root_note: 60,
            sample_slot: 0, volume_offset: 0,
            loop_start: 0, loop_end: 0, loop_enabled: false,
        });
        a.patch_set_zone_count(1, 1);
        a.voice_trigger(1, 60, 100);
        assert_eq!(a.active_voice_count(), 1);
        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        assert_eq!(a.active_voice_count(), 0, "non-looping sample should auto-free");
    }

    #[test]
    fn sampler_loop_keeps_voice_alive() {
        let mut a = AudioState::new_silent();
        a.register_sample(0, pitched_sample(128));
        a.patch_set_kind(1, PatchKind::Sampler);
        a.patch_set_zone(1, 0, KeyZone {
            low_note: 0, high_note: 127, root_note: 60,
            sample_slot: 0, volume_offset: 0,
            loop_start: 0, loop_end: 128, loop_enabled: true,
        });
        a.patch_set_zone_count(1, 1);
        // Make the amp env's sustain hold so the voice doesn't free
        // via envelope-done despite the loop.
        a.patch_set_amp_env(1, 1, 10, 127, 100);
        a.voice_trigger(1, 60, 100);
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..50 {
            a.render_block(&mut buf);
        }
        // Sample is 128 frames; 50 × 64 = 3200 frames in. A non-loop
        // would have auto-freed. Loop keeps it alive.
        assert!(a.active_voice_count() > 0, "looped sample should still be active");
    }

    #[test]
    fn sampler_routes_through_note_on_with_program_change() {
        let mut a = AudioState::new_silent();
        a.register_sample(0, pitched_sample(2048));
        a.patch_set_kind(3, PatchKind::Sampler);
        a.patch_set_zone(3, 0, KeyZone {
            low_note: 0, high_note: 127, root_note: 60,
            sample_slot: 0, volume_offset: 0,
            loop_start: 0, loop_end: 0, loop_enabled: false,
        });
        a.patch_set_zone_count(3, 1);
        a.program_change(2, 3);
        let id = a.note_on(2, 60, 100);
        assert_ne!(id, VoiceId::NONE);
        // Verify the voice carries the source channel for note_off lookup.
        let idx = a.lookup_voice(id).unwrap();
        if let VoiceKind::Sampler(state) = &a.voices[idx].kind {
            assert_eq!(state.channel, 2);
            assert_eq!(state.note, 60);
        } else {
            panic!("expected Sampler voice kind, got {:?}", a.voices[idx].kind);
        }
        // note_off should release this voice.
        a.note_off(2, 60);
        if let VoiceKind::Sampler(state) = &a.voices[idx].kind {
            assert!(state.released);
        }
    }

    // ── Stage 6a FM2OP tests ───────────────────────────────────────

    #[test]
    fn fm2op_index_zero_falls_back_to_pure_sine() {
        let mut a = AudioState::new_silent();
        // Set patch 0 to FM2OP with index = 0 → modulator has no
        // effect, so the carrier is just a pure sine on the carrier
        // freq. The OscMode of osc[1] doesn't matter.
        a.patch_set_osc(0, 0, OscMode::Fm2Op, 0, 0, 127);
        a.patch_set_osc(0, 1, OscMode::Sine, 0, 0, 127); // modulator amp, but...
        a.patch_set_fm(0, /*ratio*/256, /*index*/0); // ...index 0 zeros the contribution
        // Compare against a pure-sine patch for "should match".
        a.patch_copy(0, 1);
        a.patch_set_osc(1, 0, OscMode::Sine, 0, 0, 127);
        a.patch_set_osc(1, 1, OscMode::Sine, 0, 0, 0);

        a.voice_trigger(0, 60, 100);
        let fm_id = VoiceId(
            ((a.voices[0].generation as u32) << 8) | 0,
        );
        let _ = fm_id;
        let mut fm_buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut fm_buf);
        // The FM output must be audible (non-silence).
        let fm_peak = fm_buf.iter().map(|&s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(fm_peak > 1000, "fm2op index 0 should still produce audio: {fm_peak}");
    }

    #[test]
    fn fm2op_with_index_changes_spectrum_vs_pure_sine() {
        // Compare two passes — pure sine vs FM2OP with index = 5 —
        // by accumulating the energy in the "high-frequency" half of
        // the output buffer (a crude spectral measure: noise/FM produce
        // more zero-crossings than a clean sine).
        fn count_zero_crossings(buf: &[i16]) -> u32 {
            let mut prev_sign = 0i32;
            let mut crossings = 0u32;
            for &s in buf {
                let sign = (s as i32).signum();
                if sign != 0 && sign != prev_sign && prev_sign != 0 {
                    crossings += 1;
                }
                if sign != 0 {
                    prev_sign = sign;
                }
            }
            crossings
        }

        let mut sine_state = AudioState::new_silent();
        sine_state.patch_set_osc(0, 0, OscMode::Sine, 0, 0, 127);
        sine_state.patch_set_osc(0, 1, OscMode::Sine, 0, 0, 0);
        sine_state.voice_trigger(0, 60, 100);
        let mut sine_buf = [0i16; BLOCK_SAMPLES];
        sine_state.render_block(&mut sine_buf);
        let sine_zc = count_zero_crossings(&sine_buf);

        let mut fm_state = AudioState::new_silent();
        fm_state.patch_set_osc(0, 0, OscMode::Fm2Op, 0, 0, 127);
        fm_state.patch_set_osc(0, 1, OscMode::Sine, 0, 0, 127);
        // ratio 2.0, index 5.0 — bell-like spectrum
        fm_state.patch_set_fm(0, 512, 1280);
        // Keep filter env at sustain = max so modulation is constant.
        fm_state.patch_set_filter_env(0, 0, 0, 127, 0, 0);
        fm_state.voice_trigger(0, 60, 100);
        let mut fm_buf = [0i16; BLOCK_SAMPLES];
        fm_state.render_block(&mut fm_buf);
        let fm_zc = count_zero_crossings(&fm_buf);

        // The FM output should have more zero crossings than the pure
        // sine because FM introduces sideband content.
        assert!(
            fm_zc > sine_zc,
            "FM should produce richer spectrum (more zero crossings): sine {sine_zc}, fm {fm_zc}",
        );
    }

    #[test]
    fn patch_set_fm_stores_ratio_and_index() {
        let mut a = AudioState::new_silent();
        a.patch_set_fm(3, 384, 1024);
        assert_eq!(a.patches[3].fm_ratio, 384);
        assert_eq!(a.patches[3].fm_index, 1024);
        // Out-of-range slot silently ignored.
        a.patch_set_fm(99, 999, 999);
    }

    #[test]
    fn pitch_bend_shifts_synth_voice_frequency() {
        let mut a = AudioState::new_silent();
        // Default patch (sine) on channel 0. Trigger a held note, then
        // bend the channel; render a block and confirm the voice is
        // still active (no panic / clamp / NaN crash).
        a.note_on(0, 60, 100);
        a.pitch_bend(0, 8191); // full up
        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        assert!(buf.iter().any(|&s| s != 0));
        assert_eq!(a.active_voice_count(), 1);
        // Sanity-check the bend factor — full deflection should map to
        // ~2 semitones up = freq * 2^(2/12) ≈ 1.122.
        let f = (2.0_f32).powf(PITCH_BEND_SEMITONES / 12.0);
        assert!((f - 1.1224).abs() < 0.001);
    }
}
