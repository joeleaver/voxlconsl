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
