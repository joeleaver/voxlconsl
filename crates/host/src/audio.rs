//! Audio engine — see SPEC.md §5.
//!
//! Stage 1 (v0.1.8) ships only the mixer skeleton + sample bank +
//! one-shot SFX. The synth voice graph (§5.1), MIDI dispatch (§5.2),
//! SMF playback (§5.3), runtime patch editing (§5.7), and effects bus
//! (§5.5) land in later stages. Channel 10's default drum kit (§5.2)
//! waits on Stage 3 since it's MIDI-driven.
//!
//! ## Architecture (browser MVP)
//!
//! - Mixer runs on the **main thread** alongside the cart. No worklet,
//!   no SharedArrayBuffer, no contention.
//! - Cart calls `sfx_play(...)` via host imports, which mutate the
//!   voice pool directly. New voices start playing at the next mixer
//!   block boundary (~3 ms latency).
//! - Browser shim pulls blocks each `requestAnimationFrame` to keep ~4
//!   blocks queued ahead of `AudioContext.currentTime` (~12 ms output
//!   latency).
//!
//! Future MCU ports will move the mixer to its own task; cart-side
//! event ingress becomes a SPSC ring between cart task and audio task.
//! The portable Rust mixer doesn't care which threading model is in
//! use — its API is purely "register samples, push voices, pull
//! blocks."

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

/// Per-voice mix headroom. `i16::MAX / (peak_8bit_amplitude *
/// VOICE_POOL_SIZE)` ≈ 16, so 16 voices at full volume + center pan
/// sum to ~±32 k without clipping. Quieter scenes have plenty of
/// dynamic range left.
const MIX_HEADROOM: f32 = 16.0;

/// Declared sample rate per slot (§5.4).
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

/// One sample bank slot. `data` is 8-bit **unsigned** PCM (128 =
/// silence). `loop_points` is `(start, end)` in sample indices; play
/// position wraps from `end` back to `start` while the voice's
/// `loop_` flag is set.
#[derive(Clone)]
pub struct Sample {
    pub data: Vec<u8>,
    pub rate: SampleRate,
    pub loop_points: Option<(u32, u32)>,
}

/// Cart-facing voice handle. Bits 0..=7 carry the voice slot index;
/// bits 8..=15 carry the generation. Stale handles (slot freed +
/// reused) are silently ignored by `sfx_stop`/`sfx_set_*`.
///
/// `VoiceId::NONE` (= `VoiceId(0)`) is returned when allocation
/// fails (empty sample slot). Live voices always have generation ≥ 1.
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

struct Voice {
    active: bool,
    sample_slot: u8,
    /// Fractional sample position in `Sample::data`. `f64` keeps drift
    /// negligible over multi-second samples even with extreme
    /// pitch_ratio values.
    position: f64,
    /// Source samples consumed per output frame. 1.0 = same rate, 2.0
    /// = 1 octave up, 0.5 = 1 octave down. Composed of sample-rate
    /// conversion (src_hz / SAMPLE_RATE) * pitch (2^(cents/1200)).
    pitch_ratio: f64,
    /// 0..=127 (MIDI velocity).
    volume: u8,
    /// -64 (full left) .. +63 (full right). 0 = center.
    pan: i8,
    loop_: bool,
    /// Generation counter incremented each time the slot is freed.
    /// Always ≥ 1 while live so `VoiceId::NONE == VoiceId(0)` never
    /// matches a real voice.
    generation: u8,
    /// Mixer frame counter at trigger time. Used for oldest-first
    /// voice stealing.
    start_frame: u64,
}

impl Voice {
    fn idle() -> Self {
        Self {
            active: false,
            sample_slot: 0,
            position: 0.0,
            pitch_ratio: 1.0,
            volume: 0,
            pan: 0,
            loop_: false,
            generation: 1,
            start_frame: 0,
        }
    }

    fn deactivate(&mut self) {
        self.active = false;
        self.generation = bump_generation(self.generation);
    }
}

#[inline]
fn bump_generation(g: u8) -> u8 {
    // Wrap 1..=255, skip 0 so VoiceId::NONE is never a live handle.
    let n = g.wrapping_add(1);
    if n == 0 { 1 } else { n }
}

pub struct AudioState {
    samples: Vec<Option<Sample>>,
    voices: Vec<Voice>,
    frame_counter: u64,
    /// Telemetry: incremented when `sfx_play` had to steal a voice.
    pub voices_stolen: u32,
}

impl AudioState {
    pub fn new() -> Self {
        let mut samples = Vec::with_capacity(SAMPLE_SLOTS);
        samples.resize_with(SAMPLE_SLOTS, || None);
        let voices = (0..VOICE_POOL_SIZE).map(|_| Voice::idle()).collect();
        Self {
            samples,
            voices,
            frame_counter: 0,
            voices_stolen: 0,
        }
    }

    /// Register or replace the sample at `slot`. Out-of-range slots are
    /// silently rejected. The host **copies** PCM bytes into its own
    /// bank; the cart can drop or mutate the source buffer after this
    /// returns.
    pub fn register_sample(&mut self, slot: u8, sample: Sample) {
        if (slot as usize) >= SAMPLE_SLOTS {
            return;
        }
        self.samples[slot as usize] = Some(sample);
    }

    /// Drop a sample. Any voices currently playing the slot are
    /// deactivated on their next mix step.
    pub fn clear_sample(&mut self, slot: u8) {
        if (slot as usize) >= SAMPLE_SLOTS {
            return;
        }
        self.samples[slot as usize] = None;
    }

    /// Trigger a one-shot. Returns `VoiceId::NONE` if `slot` is empty.
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
        v.active = true;
        v.sample_slot = slot;
        v.position = 0.0;
        v.pitch_ratio = pitch_ratio;
        v.volume = volume.min(127);
        v.pan = pan.clamp(-64, 63);
        v.loop_ = loop_;
        v.start_frame = frame;
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
        let slot = self.voices[idx].sample_slot;
        let src_hz = match self.samples.get(slot as usize).and_then(|s| s.as_ref()) {
            Some(s) => s.rate.hz() as f64,
            None => return,
        };
        self.voices[idx].pitch_ratio = compute_pitch_ratio(src_hz, pitch_cents);
    }

    /// Render one block of stereo interleaved samples. `out` must have
    /// length `BLOCK_SAMPLES`.
    pub fn render_block(&mut self, out: &mut [i16]) {
        debug_assert_eq!(out.len(), BLOCK_SAMPLES);
        for s in out.iter_mut() {
            *s = 0;
        }

        let samples = &self.samples;
        for v in self.voices.iter_mut() {
            if !v.active {
                continue;
            }
            let s = match samples.get(v.sample_slot as usize).and_then(|s| s.as_ref()) {
                Some(s) => s,
                None => {
                    v.deactivate();
                    continue;
                }
            };
            mix_voice(v, s, out);
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
        if v.active && v.generation == id.generation() {
            Some(idx)
        } else {
            None
        }
    }

    /// Find a free voice slot, or steal the oldest if all 16 are
    /// active. Stage 1 has no held/released distinction (all voices
    /// are one-shots) so the policy reduces to "oldest first." Stage 3
    /// will refine this to oldest-released-first per §5.2.
    fn allocate_voice(&mut self) -> usize {
        if let Some((i, _)) = self.voices.iter().enumerate().find(|(_, v)| !v.active) {
            return i;
        }
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
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Number of frames the mixer has produced since boot. Wraps at
    /// `u64::MAX` (~26 million years at 22.05 kHz).
    pub fn frame_counter(&self) -> u64 {
        self.frame_counter
    }
}

impl Default for AudioState {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn compute_pitch_ratio(src_hz: f64, pitch_cents: i16) -> f64 {
    let factor = (2.0_f64).powf(pitch_cents as f64 / 1200.0);
    (src_hz / SAMPLE_RATE as f64) * factor
}

/// Equal-amplitude pan: center sums to 2.0 across L+R, full L/R sums
/// to 1.0. A single mono SFX at center plays full volume on both
/// channels (1.0, 1.0) — the intuitive default for a fantasy console.
/// Real stereo material would want constant-power (sqrt(2)/2 at
/// center) but we don't have stereo sources in v1.
#[inline]
fn pan_gains(pan: i8) -> (f32, f32) {
    let p = pan as f32 / 64.0;
    if p <= 0.0 {
        (1.0, 1.0 + p)
    } else {
        (1.0 - p, 1.0)
    }
}

fn mix_voice(v: &mut Voice, sample: &Sample, out: &mut [i16]) {
    let data = &sample.data;
    if data.is_empty() {
        v.deactivate();
        return;
    }

    let (gl, gr) = pan_gains(v.pan);
    let vol = v.volume as f32 / 127.0;
    let amp_l = vol * gl * MIX_HEADROOM;
    let amp_r = vol * gr * MIX_HEADROOM;

    let data_len = data.len();
    let loop_region = sample.loop_points.and_then(|(ls, le)| {
        let ls = (ls as usize).min(data_len.saturating_sub(1));
        let le = (le as usize).min(data_len);
        if le > ls { Some((ls, le)) } else { None }
    });

    for i in 0..BLOCK_FRAMES {
        // Loop wrap-around before sampling so we never index past
        // end_of_data on a looping voice.
        if v.loop_ {
            let end = loop_region.map(|(_, le)| le).unwrap_or(data_len);
            let start = loop_region.map(|(ls, _)| ls).unwrap_or(0);
            if v.position >= end as f64 {
                let span = (end - start) as f64;
                if span > 0.0 {
                    v.position = start as f64 + (v.position - start as f64).rem_euclid(span);
                } else {
                    v.position = start as f64;
                }
            }
        } else if v.position + 1.0 >= data_len as f64 {
            v.deactivate();
            return;
        }

        let s0 = v.position.floor() as usize;
        let frac = (v.position - s0 as f64) as f32;
        let a = data.get(s0).copied().unwrap_or(128);
        let b = data.get(s0 + 1).copied().unwrap_or(a);
        let f0 = (a as i32 - 128) as f32;
        let f1 = (b as i32 - 128) as f32;
        let s = f0 + (f1 - f0) * frac;

        let oi = i * 2;
        let l = (s * amp_l) as i32;
        let r = (s * amp_r) as i32;
        out[oi] = (out[oi] as i32)
            .saturating_add(l)
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        out[oi + 1] = (out[oi + 1] as i32)
            .saturating_add(r)
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16;

        v.position += v.pitch_ratio;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp_sample(len: usize, rate: SampleRate) -> Sample {
        // Linear ramp from 128 (silence) to 255 (peak positive). Easy
        // to reason about during mixing tests.
        let data = (0..len).map(|i| 128 + (i.min(127) as u8)).collect();
        Sample {
            data,
            rate,
            loop_points: None,
        }
    }

    fn loud_sample(rate: SampleRate, len: usize) -> Sample {
        // Constant peak amplitude — every sample = 255 (+127 after
        // recentering). Useful for testing mixing levels.
        Sample {
            data: vec![255; len],
            rate,
            loop_points: None,
        }
    }

    #[test]
    fn single_voice_plays_and_auto_frees() {
        let mut a = AudioState::new();
        a.register_sample(0, ramp_sample(8, SampleRate::Khz22_05));
        let id = a.sfx_play(0, 127, 0, 0, false);
        assert_ne!(id, VoiceId::NONE);
        assert_eq!(a.active_voice_count(), 1);

        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf);
        // Sample is 8 source samples, played at 1:1 against 64-sample
        // block, so the voice should free itself this block.
        assert_eq!(a.active_voice_count(), 0);
        // At least some output should be nonzero.
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

        // 17th play steals the oldest.
        let stolen_owner = a.sfx_play(0, 50, 0, 0, false);
        assert_ne!(stolen_owner, VoiceId::NONE);
        assert_eq!(a.active_voice_count(), VOICE_POOL_SIZE);
        assert_eq!(a.voices_stolen, 1);

        // Original first id is now stale — sfx_stop should be a no-op
        // (not panic, not affect any voice).
        a.sfx_stop(ids[0]);
        assert_eq!(a.active_voice_count(), VOICE_POOL_SIZE);
    }

    #[test]
    fn stale_voice_id_set_volume_is_noop() {
        let mut a = AudioState::new();
        a.register_sample(0, loud_sample(SampleRate::Khz22_05, 4));
        let id = a.sfx_play(0, 100, 0, 0, false);

        let mut buf = [0i16; BLOCK_SAMPLES];
        a.render_block(&mut buf); // sample exhausts → voice freed

        // Stale id should be silently ignored.
        a.sfx_set_volume(id, 0);
        // Still no panic; no active voice.
        assert_eq!(a.active_voice_count(), 0);
    }

    #[test]
    fn looping_sample_does_not_auto_free() {
        let mut a = AudioState::new();
        let mut s = loud_sample(SampleRate::Khz22_05, 8);
        s.loop_points = Some((0, 8));
        a.register_sample(0, s);
        let _id = a.sfx_play(0, 100, 0, 0, true);

        // Many blocks later the voice should still be alive.
        let mut buf = [0i16; BLOCK_SAMPLES];
        for _ in 0..10 {
            a.render_block(&mut buf);
        }
        assert_eq!(a.active_voice_count(), 1);
    }

    #[test]
    fn pitch_cents_doubles_rate_per_octave() {
        // +1200 cents = 1 octave up = 2× source samples consumed per
        // output frame.
        let mut a = AudioState::new();
        a.register_sample(0, loud_sample(SampleRate::Khz22_05, 256));
        let id_norm = a.sfx_play(0, 0, 0, 0, false);
        let normal = match id_norm {
            VoiceId::NONE => unreachable!(),
            _ => a.voices[id_norm.slot()].pitch_ratio,
        };
        a.sfx_set_pitch(id_norm, 1200);
        let octave_up = a.voices[id_norm.slot()].pitch_ratio;
        assert!((octave_up / normal - 2.0).abs() < 1e-6,
            "expected 2× pitch ratio, got {octave_up}/{normal}");
    }

    #[test]
    fn rate_11k_played_at_22k_halves_pitch_ratio() {
        let mut a = AudioState::new();
        a.register_sample(0, loud_sample(SampleRate::Khz11_025, 256));
        let id = a.sfx_play(0, 0, 0, 0, false);
        let ratio = a.voices[id.slot()].pitch_ratio;
        assert!((ratio - 0.5).abs() < 1e-6, "expected 0.5, got {ratio}");
    }

    #[test]
    fn sample_replacement_works() {
        let mut a = AudioState::new();
        a.register_sample(0, loud_sample(SampleRate::Khz22_05, 16));
        a.register_sample(0, loud_sample(SampleRate::Khz11_025, 32));
        let id = a.sfx_play(0, 0, 0, 0, false);
        let ratio = a.voices[id.slot()].pitch_ratio;
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
        // Every output must be a valid i16 (no overflow / wrap).
        // Constant peak source × 16 voices × headroom 16 ≈ ±32 k —
        // right at i16::MAX but clamp keeps us in range.
        for s in buf {
            assert!(s as i32 <= i16::MAX as i32 && s as i32 >= i16::MIN as i32);
        }
    }

    #[test]
    fn out_of_range_slot_silently_rejected() {
        let mut a = AudioState::new();
        // SAMPLE_SLOTS = 64; slot 200 must not panic or allocate.
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
        // Wrap from 255 should land on 1, not 0, so NONE is never
        // mistaken for a live voice.
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
}
