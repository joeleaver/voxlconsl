//! Audio — SPEC.md §5.
//!
//! Stage 1 (v0.1.8) ships sample-bank registration + one-shot SFX
//! playback. Synth voices (§5.1), MIDI (§5.2), SMF (§5.3), runtime
//! patch editing (§5.7), and the effects bus (§5.5) land in later
//! stages.
//!
//! Workflow:
//!
//! ```ignore
//! const KICK_PCM: &[u8] = include_bytes!("../assets/kick.pcm");
//!
//! // Once during init():
//! sample_register(0, KICK_PCM, SampleRate::Khz22_05, None);
//!
//! // Anytime:
//! let voice = sfx_play(0, 100, 0, 0, false);
//! ```

use crate::host;

/// Sample rate declared per slot. The host treats the PCM data as
/// belonging to this rate when resampling for output (§5.4).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SampleRate {
    Khz11_025 = 0,
    Khz22_05 = 1,
}

/// Returned by [`sfx_play`]. Pass to [`sfx_stop`] /
/// [`sfx_set_volume`] / [`sfx_set_pitch`] to modulate a live voice.
/// Becomes stale automatically when the voice finishes or is stolen;
/// stale handles are silently ignored by subsequent calls.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct VoiceId(pub u32);

impl VoiceId {
    pub const NONE: Self = Self(0);

    pub fn is_some(self) -> bool {
        self != Self::NONE
    }
}

/// Register / replace a sample slot. PCM is **8-bit unsigned** (128 =
/// silence). `loop_points`, when provided, lets a `loop_=true` voice
/// wrap from `end` back to `start` while playing — for sustained one-
/// shots like ambient loops. Slots ≥ 64 are silently rejected by the
/// host (§5.4).
///
/// The host copies the PCM bytes into its own bank; the cart can drop
/// or mutate the source buffer after this call returns.
pub fn sample_register(
    slot: u8,
    pcm: &[u8],
    rate: SampleRate,
    loop_points: Option<(u32, u32)>,
) {
    let (flags, ls, le) = match loop_points {
        Some((s, e)) => (1u32, s, e),
        None => (0u32, 0, 0),
    };
    unsafe {
        host::sample_load(
            slot as u32,
            pcm.as_ptr(),
            pcm.len() as u32,
            rate as u32,
            flags,
            ls,
            le,
        );
    }
}

/// Trigger a one-shot sample (§5.6). Returns [`VoiceId::NONE`] if the
/// slot is empty.
///
/// - `volume` 0..=127 (MIDI velocity).
/// - `pan` -64 (full L) ..= 63 (full R), 0 = center.
/// - `pitch_cents` 0 = original pitch; ±100 = ±1 semitone.
/// - `loop_` `true` plays the declared loop region indefinitely until
///   stopped.
///
/// Non-looped one-shots can ignore the returned `VoiceId`; the host
/// auto-frees the voice when the sample ends.
pub fn sfx_play(
    slot: u8,
    volume: u8,
    pan: i8,
    pitch_cents: i16,
    loop_: bool,
) -> VoiceId {
    let raw = unsafe {
        host::sfx_play(
            slot as u32,
            volume as u32,
            pan as i32,
            pitch_cents as i32,
            loop_ as u32,
        )
    };
    VoiceId(raw)
}

/// Stop a live voice early. No-op for stale ids.
pub fn sfx_stop(voice: VoiceId) {
    unsafe { host::sfx_stop(voice.0) }
}

/// Update a live voice's volume. No-op for stale ids.
pub fn sfx_set_volume(voice: VoiceId, volume: u8) {
    unsafe { host::sfx_set_volume(voice.0, volume as u32) }
}

/// Update a live voice's pitch (cents from original). No-op for stale
/// ids.
pub fn sfx_set_pitch(voice: VoiceId, pitch_cents: i16) {
    unsafe { host::sfx_set_pitch(voice.0, pitch_cents as i32) }
}

// ============================================================================
// Synth (§5.1, §5.7) — Stage 2.
//
// Patches are cart-owned instrument definitions; carts mutate them
// freely via `patch_set_*` and trigger notes via `voice_trigger`. The
// `voice_trigger / voice_release` pair is a Stage 2 stand-in for the
// MIDI surface — Stage 3 will replace it with `note_on / note_off` on
// 16-channel routing, and these calls remain available as the under-
// MIDI primitive.
// ============================================================================

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OscMode {
    Sine = 0,
    Saw = 1,
    Square = 2,
    Triangle = 3,
    Noise = 4,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FilterMode {
    Off = 0,
    LowPass = 1,
    HighPass = 2,
    BandPass = 3,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LfoShape {
    Sine = 0,
    Triangle = 1,
    Square = 2,
    SampleAndHold = 3,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LfoTarget {
    Pitch = 0,
    Filter = 1,
    Amp = 2,
    Pan = 3,
}

/// ADSR envelope settings (used for both amp and filter envelopes).
/// All times in milliseconds; `sustain` is 0..=127 (fraction of peak).
#[derive(Copy, Clone, Debug)]
pub struct EnvParams {
    pub attack_ms: u16,
    pub decay_ms: u16,
    pub sustain: u8,
    pub release_ms: u16,
}

/// Configure one of a patch's two oscillators (§5.1).
pub fn patch_set_osc(
    patch: u8,
    osc: u8,
    mode: OscMode,
    detune_cents: i16,
    octave: i8,
    level: u8,
) {
    unsafe {
        host::patch_set_osc(
            patch as u32,
            osc as u32,
            mode as u32,
            detune_cents as i32,
            octave as i32,
            level as u32,
        )
    }
}

/// Configure the filter block of a patch (§5.1).
pub fn patch_set_filter(patch: u8, mode: FilterMode, cutoff_hz: u16, resonance: u8) {
    unsafe {
        host::patch_set_filter(patch as u32, mode as u32, cutoff_hz as u32, resonance as u32)
    }
}

/// Configure the amp envelope of a patch.
pub fn patch_set_amp_env(patch: u8, env: EnvParams) {
    unsafe {
        host::patch_set_amp_env(
            patch as u32,
            env.attack_ms as u32,
            env.decay_ms as u32,
            env.sustain as u32,
            env.release_ms as u32,
        )
    }
}

/// Configure the filter envelope + cutoff modulation depth.
/// `depth` is signed -127..=127; positive opens the filter as the
/// envelope rises, negative closes it.
pub fn patch_set_filter_env(patch: u8, env: EnvParams, depth: i8) {
    unsafe {
        host::patch_set_filter_env(
            patch as u32,
            env.attack_ms as u32,
            env.decay_ms as u32,
            env.sustain as u32,
            env.release_ms as u32,
            depth as i32,
        )
    }
}

/// Configure the LFO. `rate_centihz` is hundredths of a Hz (100 = 1 Hz,
/// 1000 = 10 Hz). `depth` is -127..=127; sign affects routing polarity.
pub fn patch_set_lfo(
    patch: u8,
    rate_centihz: u16,
    shape: LfoShape,
    target: LfoTarget,
    depth: i8,
) {
    unsafe {
        host::patch_set_lfo(
            patch as u32,
            rate_centihz as u32,
            shape as u32,
            target as u32,
            depth as i32,
        )
    }
}

/// Set the patch's portamento time. 0 = no glide (notes start at their
/// target frequency immediately).
pub fn patch_set_glide(patch: u8, ms: u16) {
    unsafe { host::patch_set_glide(patch as u32, ms as u32) }
}

/// Reset a patch to the default sine + snappy ADSR.
pub fn patch_reset(patch: u8) {
    unsafe { host::patch_reset(patch as u32) }
}

/// Copy every parameter of `src` into `dst`.
pub fn patch_copy(src: u8, dst: u8) {
    unsafe { host::patch_copy(src as u32, dst as u32) }
}

/// Trigger a synth note on the given patch. `note` is MIDI note
/// number (60 = middle C, 69 = A4 / 440 Hz). `velocity` 0..=127 scales
/// output amplitude. Returns [`VoiceId::NONE`] if the patch slot is
/// out of range.
///
/// The voice plays through its amp envelope's Attack → Decay → Sustain
/// stages and holds there until either [`voice_release`] is called
/// (transitioning into Release) or the voice gets stolen for a newer
/// trigger.
pub fn voice_trigger(patch: u8, note: u8, velocity: u8) -> VoiceId {
    let raw = unsafe { host::voice_trigger(patch as u32, note as u32, velocity as u32) };
    VoiceId(raw)
}

/// Move a synth voice into the Release stage of its envelope. The
/// voice keeps playing until release completes, then auto-frees.
/// No-op for SFX voices and stale ids.
pub fn voice_release(voice: VoiceId) {
    unsafe { host::voice_release(voice.0) }
}
