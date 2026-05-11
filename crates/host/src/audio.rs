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

/// Per-voice mix headroom. With i16 output and 8-bit-equivalent peak
/// amplitude 128, 16 voices at full volume + center pan sum to
/// ±32 k without clipping (headroom × peak × pool = 16 × 128 × 16 =
/// 32 768). Synth voices use the same scale via 128 × normalized_f32.
const MIX_HEADROOM: f32 = 16.0;

/// Convert normalized f32 sample (-1..1) into the 8-bit-equivalent
/// integer amplitude that `MIX_HEADROOM` budgets for. Conservative
/// at 64 (vs the theoretical 128 that matches 8-bit-PCM peak) to
/// leave headroom for: (a) the saw oscillator's sharp transients,
/// which alias above ±1 through the SVF, and (b) the SVF's resonance
/// peak when both filter envelope and resonance are pushing.
const SYNTH_AMPLITUDE: f32 = 64.0;

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

/// Oscillator waveform (§5.1). FM2OP is deferred to Stage 6.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OscMode {
    Sine,
    Saw,
    Square,
    Triangle,
    Noise,
}

impl OscMode {
    pub fn from_code(c: u8) -> Self {
        match c {
            0 => Self::Sine,
            1 => Self::Saw,
            2 => Self::Square,
            3 => Self::Triangle,
            4 => Self::Noise,
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

#[derive(Copy, Clone, Debug)]
pub struct Patch {
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

/// State-variable filter (Chamberlin form). Per-voice; reset on
/// voice_trigger.
#[derive(Copy, Clone, Debug, Default)]
struct SvfState {
    v1: f32,
    v2: f32,
}

#[derive(Copy, Clone, Debug)]
struct SfxVoiceState {
    sample_slot: u8,
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
    /// MIDI note number. Stored but not yet read directly; Stage 3
    /// will reference it for `note_off` / `all_notes_off` matching.
    #[allow(dead_code)]
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

#[derive(Copy, Clone, Debug)]
enum VoiceKind {
    Idle,
    Sfx(SfxVoiceState),
    Synth(SynthVoiceState),
}

#[derive(Copy, Clone, Debug)]
struct Voice {
    /// 0..=127 (MIDI velocity-style channel volume). Synth voices
    /// have this default to 127; Stage 3's CC 7 will modulate it.
    volume: u8,
    /// -64..=63.
    pan: i8,
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
// AudioState — public mixer API.
// ===========================================================================

pub struct AudioState {
    samples: Vec<Option<Sample>>,
    patches: Vec<Patch>,
    voices: Vec<Voice>,
    frame_counter: u64,
    /// Telemetry: incremented when `sfx_play` / `voice_trigger` had to
    /// steal a voice.
    pub voices_stolen: u32,
}

impl AudioState {
    pub fn new() -> Self {
        let mut samples = Vec::with_capacity(SAMPLE_SLOTS);
        samples.resize_with(SAMPLE_SLOTS, || None);
        let mut patches = Vec::with_capacity(PATCH_SLOTS);
        patches.resize(PATCH_SLOTS, Patch::default_synth());
        let voices = (0..VOICE_POOL_SIZE).map(|_| Voice::idle()).collect();
        Self { samples, patches, voices, frame_counter: 0, voices_stolen: 0 }
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
        v.start_frame = frame;
        v.kind = VoiceKind::Sfx(SfxVoiceState {
            sample_slot: slot,
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
            let slot = state.sample_slot;
            let src_hz = match self.samples.get(slot as usize).and_then(|s| s.as_ref()) {
                Some(s) => s.rate.hz() as f64,
                None => return,
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
        v.start_frame = frame;
        v.kind = VoiceKind::Synth(SynthVoiceState {
            patch: patch_slot,
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

    /// Move a synth voice's amp/filter envelopes into the Release
    /// stage. The voice keeps mixing until release completes, then
    /// auto-frees. No-op for SFX voices or stale ids.
    pub fn voice_release(&mut self, id: VoiceId) {
        let Some(idx) = self.lookup_voice(id) else { return };
        if let VoiceKind::Synth(state) = &mut self.voices[idx].kind {
            if !state.released {
                state.released = true;
                state.amp_env.stage = EnvStage::Release;
                state.filter_env.stage = EnvStage::Release;
            }
        }
    }

    // ── Render ─────────────────────────────────────────────────────

    /// Render one block of stereo interleaved samples. `out` must have
    /// length `BLOCK_SAMPLES`.
    pub fn render_block(&mut self, out: &mut [i16]) {
        debug_assert_eq!(out.len(), BLOCK_SAMPLES);
        for s in out.iter_mut() {
            *s = 0;
        }

        // Split borrow so each voice can read the (immutable) sample
        // bank + patch table while we mutate the voice itself.
        let samples = &self.samples;
        let patches = &self.patches;
        for voice in self.voices.iter_mut() {
            match &mut voice.kind {
                VoiceKind::Idle => {}
                VoiceKind::Sfx(_) => mix_sfx_voice(voice, samples, out),
                VoiceKind::Synth(_) => mix_synth_voice(voice, patches, out),
            }
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

// ===========================================================================
// SFX mix path (Stage 1) — unchanged behavior.
// ===========================================================================

fn mix_sfx_voice(voice: &mut Voice, samples: &[Option<Sample>], out: &mut [i16]) {
    let state = match &mut voice.kind {
        VoiceKind::Sfx(s) => s,
        _ => return,
    };
    let sample = match samples.get(state.sample_slot as usize).and_then(|s| s.as_ref()) {
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
    let amp_l = vol * gl * MIX_HEADROOM;
    let amp_r = vol * gr * MIX_HEADROOM;

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
fn lfo_value(state: &mut SynthVoiceState, shape: LfoShape, phase_prev: f32) -> f32 {
    use core::f32::consts::TAU;
    let p = state.lfo_phase;
    match shape {
        LfoShape::Sine => (p * TAU).sin(),
        LfoShape::Triangle => {
            if p < 0.5 {
                4.0 * p - 1.0
            } else {
                3.0 - 4.0 * p
            }
        }
        LfoShape::Square => if p < 0.5 { 1.0 } else { -1.0 },
        LfoShape::SampleAndHold => {
            // Wrap = new sample. We detect a wrap by seeing
            // phase_prev > phase (because phase decreased by 1.0).
            if phase_prev > p {
                state.rng = state.rng.wrapping_mul(1103515245).wrapping_add(12345);
                let raw = (state.rng >> 16) as i16;
                state.sh_value = (raw as f32) / 32768.0;
            }
            state.sh_value
        }
    }
}

#[inline]
fn next_noise(rng: &mut u32) -> f32 {
    *rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
    let raw = (*rng >> 16) as i16;
    (raw as f32) / 32768.0
}

/// Process one input sample through a state-variable filter. Returns
/// the filtered sample. `mode` decides which SVF tap is returned.
///
/// Uses the Chamberlin SVF topology. It's unconditionally stable only
/// while `f = 2·sin(π·fc/fs) < 1.4` — corresponds to `fc < ~0.247·fs`
/// (≈ 5440 Hz at 22.05 kHz). We clamp `cutoff_hz` to a safer value
/// (5000 Hz here) so the filter never runs away even when the filter
/// envelope or LFO modulates cutoff aggressively. Above that, the
/// caller is free to set higher cutoff values in the patch but the
/// filter behaves as if cutoff = SVF_CUTOFF_MAX.
const SVF_CUTOFF_MAX: f32 = 5000.0;

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
    let cutoff = cutoff_hz.clamp(20.0, SVF_CUTOFF_MAX);
    let f = 2.0 * (core::f32::consts::PI * cutoff / SAMPLE_RATE as f32).sin();
    // Resonance maps 0..127 → damping 1.0..0.05. Lower damping = more
    // peak at cutoff, but we never let it go below 0.05 to avoid the
    // SVF self-oscillating at full resonance.
    let q = 1.0 - (resonance as f32 / 127.0) * 0.95;
    let lp = state.v1 + f * state.v2;
    let hp = input - lp - q * state.v2;
    let bp = state.v2 + f * hp;
    // Belt-and-suspenders: clamp the integrators so a momentary
    // numerical excursion can't permanently destabilize the filter.
    state.v1 = lp.clamp(-4.0, 4.0);
    state.v2 = bp.clamp(-4.0, 4.0);
    match mode {
        FilterMode::LowPass => state.v1,
        FilterMode::HighPass => hp.clamp(-4.0, 4.0),
        FilterMode::BandPass => state.v2,
        FilterMode::Off => input, // unreachable
    }
}

fn mix_synth_voice(voice: &mut Voice, patches: &[Patch], out: &mut [i16]) {
    // Extract everything we need before the &mut state borrow.
    let pan = voice.pan;
    let volume = voice.volume;
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

    // Per-block premultiplied amp constants.
    let (gl, gr) = pan_gains(pan);
    let vol = volume as f32 / 127.0;
    let velocity = state.velocity as f32 / 127.0;
    let block_amp = vol * velocity * MIX_HEADROOM * SYNTH_AMPLITUDE;

    let lfo_rate_hz = patch.lfo.rate_centihz as f32 * 0.01;
    let lfo_inc = lfo_rate_hz * dt;
    let lfo_depth = patch.lfo.depth as f32 / 127.0;

    let glide_samples = if patch.glide_ms > 0 {
        (patch.glide_ms as f32 * 0.001 * SAMPLE_RATE as f32).max(1.0)
    } else {
        1.0
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
        let lfo_raw = lfo_value(state, patch.lfo.shape, lfo_phase_prev);
        let lfo = lfo_raw * lfo_depth;

        // Oscillator pitch with optional LFO-pitch modulation.
        let pitch_mod_cents = if matches!(patch.lfo.target, LfoTarget::Pitch) {
            lfo * 100.0
        } else {
            0.0
        };
        let pitch_mod_factor = (2.0_f32).powf(pitch_mod_cents / 1200.0);

        // Generate per-oscillator samples.
        let mut osc_sum = 0.0;
        for (k, params) in patch.osc.iter().enumerate() {
            if params.level == 0 {
                continue;
            }
            let octave_mult = (2.0_f32).powi(params.octave as i32);
            let detune_mult = (2.0_f32).powf(params.detune_cents as f32 / 1200.0);
            let freq = state.cur_freq * octave_mult * detune_mult * pitch_mod_factor;
            let phase_inc = freq / SAMPLE_RATE as f32;
            state.osc_phase[k] += phase_inc;
            if state.osc_phase[k] >= 1.0 {
                state.osc_phase[k] -= state.osc_phase[k].floor();
            } else if state.osc_phase[k] < 0.0 {
                state.osc_phase[k] += 1.0;
            }
            let p = state.osc_phase[k];
            let raw = match params.mode {
                OscMode::Sine => (p * core::f32::consts::TAU).sin(),
                OscMode::Saw => 2.0 * p - 1.0,
                OscMode::Square => if p < 0.5 { 1.0 } else { -1.0 },
                OscMode::Triangle => {
                    if p < 0.5 {
                        4.0 * p - 1.0
                    } else {
                        3.0 - 4.0 * p
                    }
                }
                OscMode::Noise => next_noise(&mut state.rng),
            };
            let level = params.level as f32 / 127.0;
            osc_sum += raw * level;
        }
        // Two oscillators at full level sum to ±2.0; normalize back to
        // roughly ±1 to keep the mix amp scale consistent.
        let osc_sample = osc_sum * 0.5;

        // Envelopes.
        let amp_env = advance_envelope(&mut state.amp_env, &patch.amp_env, state.released, dt);
        let filter_env = advance_envelope(&mut state.filter_env, &patch.filter_env, state.released, dt);

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
        let l = (sample * pl * block_amp) as i32;
        let r = (sample * pr * block_amp) as i32;
        out[oi] = (out[oi] as i32)
            .saturating_add(l)
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        out[oi + 1] = (out[oi + 1] as i32)
            .saturating_add(r)
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16;

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
}
