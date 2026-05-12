//! Effects bus (§5.5) — Schroeder reverb + stereo cross-feedback delay.
//!
//! Both effects are **shared sends**: each voice contributes a
//! scaled copy of its dry signal to the wet bus (scale = the source
//! channel's CC 91 / CC 93 value snapshotted at note_on time), the
//! host processes the wet bus once per block, and the result is
//! summed back into the master accumulator before the master soft-
//! clip stage.
//!
//! The parameters are global per the §5.5 fixed-architecture: cart
//! sets `room_size` + `damping` on the reverb and `time_ms` +
//! `feedback` on the delay. No per-channel reverb settings; only
//! per-channel send amounts.
//!
//! Both effect blocks are designed so MCU ports can stub them out
//! at compile time via a feature flag: cart-side CC 91/93 sends
//! become silent no-ops without any API divergence.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use crate::audio::SAMPLE_RATE;

// ============================================================================
// Schroeder reverb (Stage 5, §5.5)
// ============================================================================
//
// 4 parallel feedback combs (with damping LP in each comb's feedback
// loop) → 2 series allpasses. Combs run separately for L and R with
// slightly offset delays (the standard "stereo width" trick — same
// algorithm, different tap lengths per channel) so the wet output
// has a sense of space. Allpasses diffuse each channel's reflections
// without coloring the frequency response.
//
// Comb / allpass delay constants are Freeverb's at 44.1 kHz halved
// for our 22.05 kHz mixer (then rounded). The R channel adds a small
// constant offset so L/R taps don't ring in lockstep.

const REVERB_COMB_L: [usize; 4] = [558, 594, 638, 678];
const REVERB_COMB_R: [usize; 4] = [569, 605, 649, 689];
const REVERB_ALLPASS_L: [usize; 2] = [278, 220];
const REVERB_ALLPASS_R: [usize; 2] = [290, 233];

#[derive(Debug, Clone)]
struct Comb {
    buf: Vec<f32>,
    pos: usize,
    /// Lowpass state for the damping filter sitting in the comb's
    /// feedback loop. Higher damping = more high-frequency rolloff
    /// in the tail, i.e. a "darker" reverb that mimics absorption.
    lp_state: f32,
}

impl Comb {
    fn new(delay: usize) -> Self {
        Self { buf: vec![0.0; delay.max(1)], pos: 0, lp_state: 0.0 }
    }

    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damping: f32) -> f32 {
        let y = self.buf[self.pos];
        // One-pole lowpass on the feedback path. `damping` ∈ [0, 1):
        // 0 = no attenuation of highs, ~0.99 = severe HF rolloff.
        self.lp_state = y * (1.0 - damping) + self.lp_state * damping;
        self.buf[self.pos] = x + self.lp_state * feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

#[derive(Debug, Clone)]
struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    fn new(delay: usize) -> Self {
        Self { buf: vec![0.0; delay.max(1)], pos: 0 }
    }

    #[inline]
    fn process(&mut self, x: f32, gain: f32) -> f32 {
        // Schroeder allpass — flat magnitude response, phase scramble.
        let buf_out = self.buf[self.pos];
        let y = -gain * x + buf_out;
        self.buf[self.pos] = x + gain * buf_out;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

#[derive(Debug, Clone)]
pub struct ReverbState {
    combs_l: [Comb; 4],
    combs_r: [Comb; 4],
    allpasses_l: [Allpass; 2],
    allpasses_r: [Allpass; 2],
    /// 0..1. Drives the comb feedback gain. Larger room = longer
    /// tail. Mapped into [0.70, 0.98] so the reverb never quite
    /// self-oscillates at the upper bound.
    room_size: f32,
    /// 0..1. Damping coefficient for each comb's feedback-loop LP.
    damping: f32,
}

impl ReverbState {
    pub fn new() -> Self {
        Self {
            combs_l: [
                Comb::new(REVERB_COMB_L[0]),
                Comb::new(REVERB_COMB_L[1]),
                Comb::new(REVERB_COMB_L[2]),
                Comb::new(REVERB_COMB_L[3]),
            ],
            combs_r: [
                Comb::new(REVERB_COMB_R[0]),
                Comb::new(REVERB_COMB_R[1]),
                Comb::new(REVERB_COMB_R[2]),
                Comb::new(REVERB_COMB_R[3]),
            ],
            allpasses_l: [
                Allpass::new(REVERB_ALLPASS_L[0]),
                Allpass::new(REVERB_ALLPASS_L[1]),
            ],
            allpasses_r: [
                Allpass::new(REVERB_ALLPASS_R[0]),
                Allpass::new(REVERB_ALLPASS_R[1]),
            ],
            // Defaults: medium room, medium damping — pleasant for the
            // boot drum kit and synth lead without any cart action.
            room_size: 0.5,
            damping: 0.5,
        }
    }

    /// Set the room size + damping. Both are 0..127 (MIDI-style) and
    /// get normalized to [0, 1] internally. `room_size = 0` produces
    /// a tiny ~150 ms tail; `room_size = 127` is close to self-
    /// oscillation, ~3-5 s tail at 22.05 kHz with no damping.
    pub fn set_params(&mut self, room_size: u8, damping: u8) {
        self.room_size = (room_size.min(127) as f32) / 127.0;
        self.damping = (damping.min(127) as f32) / 127.0;
    }

    /// Process one block of interleaved stereo input → stereo output.
    /// `wet` is interleaved L,R,L,R,... with `BLOCK_FRAMES * 2` slots.
    /// The reverb input is mixed to mono (L+R)/2 and fed equally into
    /// both channel paths; the offset comb / allpass delays generate
    /// the stereo width.
    pub fn process_block(&mut self, wet: &mut [f32]) {
        // Map room_size 0..1 → comb feedback 0.70..0.98. 0.98 is just
        // shy of unity, which is where Schroeder reverbs typically
        // sit for a "concert hall" length tail without going unstable.
        let feedback = 0.70 + 0.28 * self.room_size;
        let damping = self.damping;

        let frames = wet.len() / 2;
        for i in 0..frames {
            let xl = wet[i * 2];
            let xr = wet[i * 2 + 1];
            // Mono input — the combs only see the sum, and the stereo
            // width comes entirely from L/R having different tap lengths.
            let x = (xl + xr) * 0.5;

            let mut yl = 0.0;
            for c in &mut self.combs_l {
                yl += c.process(x, feedback, damping);
            }
            let mut yr = 0.0;
            for c in &mut self.combs_r {
                yr += c.process(x, feedback, damping);
            }
            // Normalize: 4 combs in parallel each contribute their own
            // delayed signal; without scaling, the sum has unity
            // average gain at DC.
            yl *= 0.25;
            yr *= 0.25;

            // Series allpasses diffuse echo density without changing
            // the magnitude spectrum. Classic Schroeder gain 0.5.
            yl = self.allpasses_l[0].process(yl, 0.5);
            yl = self.allpasses_l[1].process(yl, 0.5);
            yr = self.allpasses_r[0].process(yr, 0.5);
            yr = self.allpasses_r[1].process(yr, 0.5);

            wet[i * 2] = yl;
            wet[i * 2 + 1] = yr;
        }
    }
}

impl Default for ReverbState {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// Stereo cross-feedback delay (Stage 5, §5.5)
// ============================================================================
//
// Two delay lines, one per channel, with cross-feedback: L's tap
// feeds back into R's input and vice versa. Produces ping-pong
// delay where echoes bounce L→R→L→R, decaying by `feedback` each
// hop. Time and feedback are cart-controllable.
//
// Buffer sized to hold up to 2 seconds of audio at 22.05 kHz so the
// cart has headroom for slow ambient delays. Time changes don't
// resize the buffer — we just move the read pointer.

const DELAY_MAX_SECONDS: usize = 2;
const DELAY_MAX_SAMPLES: usize = SAMPLE_RATE as usize * DELAY_MAX_SECONDS;
const DELAY_DEFAULT_MS: u16 = 250;
const DELAY_DEFAULT_FEEDBACK: u8 = 50;

#[derive(Debug, Clone)]
pub struct DelayState {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,
    delay_samples: usize,
    /// 0..1. Clamped to ≤ 0.95 internally — at unity the line never
    /// decays and the delay turns into a runaway feedback loop.
    feedback: f32,
}

impl DelayState {
    pub fn new() -> Self {
        let mut s = Self {
            buf_l: vec![0.0; DELAY_MAX_SAMPLES],
            buf_r: vec![0.0; DELAY_MAX_SAMPLES],
            write_pos: 0,
            delay_samples: 0,
            feedback: 0.0,
        };
        s.set_time_ms(DELAY_DEFAULT_MS);
        s.set_feedback(DELAY_DEFAULT_FEEDBACK);
        s
    }

    pub fn set_time_ms(&mut self, ms: u16) {
        let samples = (ms as u32 * SAMPLE_RATE / 1000) as usize;
        self.delay_samples = samples.clamp(1, DELAY_MAX_SAMPLES - 1);
    }

    pub fn set_feedback(&mut self, fb_0_127: u8) {
        // Hard ceiling at 0.95 so the line always decays — a fantasy
        // console doesn't ship a delay that can lock up.
        self.feedback = (fb_0_127.min(127) as f32 / 127.0).min(0.95);
    }

    /// Process one block. `wet` is interleaved L,R input that we
    /// overwrite with delayed output (the input copy that the read
    /// tap returns). Caller scales by the per-voice send amount
    /// before passing in; we don't apply another mix gain.
    pub fn process_block(&mut self, wet: &mut [f32]) {
        let len = self.buf_l.len();
        let delay = self.delay_samples;
        let frames = wet.len() / 2;
        for i in 0..frames {
            let xl = wet[i * 2];
            let xr = wet[i * 2 + 1];
            // Read tap = delay_samples behind the write head.
            let read_pos = (self.write_pos + len - delay) % len;
            let yl = self.buf_l[read_pos];
            let yr = self.buf_r[read_pos];
            // Cross-feedback ping-pong: L line gets new input + R's tap
            // scaled by feedback, R line gets new input + L's tap.
            self.buf_l[self.write_pos] = xl + yr * self.feedback;
            self.buf_r[self.write_pos] = xr + yl * self.feedback;
            self.write_pos = (self.write_pos + 1) % len;
            // Output is the tap itself — the dry signal is added back
            // at the master stage. Send level controls wet amount.
            wet[i * 2] = yl;
            wet[i * 2 + 1] = yr;
        }
    }
}

impl Default for DelayState {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverb_tails_out_after_impulse() {
        let mut rv = ReverbState::new();
        // Wider room, less damping — long, bright tail so we can see it.
        rv.set_params(100, 30);
        // Need at least ~700 frames to see the first comb echo (the
        // shortest L comb is 558 samples).
        let mut buf = vec![0.0_f32; 1024 * 2];
        buf[0] = 1.0;
        buf[1] = 1.0;
        rv.process_block(&mut buf);
        // Sum energy across the entire window past the impulse — the
        // Schroeder reverb spreads the impulse into a dense tail.
        let tail_energy: f32 = buf[10..].iter().map(|s| s.abs()).sum();
        assert!(tail_energy > 0.1, "expected audible reverb tail, got {tail_energy}");
    }

    #[test]
    fn reverb_silent_input_silent_output() {
        let mut rv = ReverbState::new();
        let mut buf = vec![0.0_f32; 64 * 2];
        rv.process_block(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn delay_produces_delayed_copy() {
        let mut d = DelayState::new();
        d.set_time_ms(10);
        d.set_feedback(0);
        // 10 ms @ 22.05 kHz = 220 samples.
        let delay_samples = (10 * SAMPLE_RATE / 1000) as usize;
        // Big enough buffer to span the delay.
        let n_frames = delay_samples + 32;
        let mut buf = vec![0.0_f32; n_frames * 2];
        // Impulse at frame 0.
        buf[0] = 1.0;
        buf[1] = 1.0;
        d.process_block(&mut buf);
        // The tap should be ~1.0 at frame `delay_samples` and ~0
        // before. Allow a small slop window around the exact tap.
        let tap_l = buf[delay_samples * 2];
        let pre = buf[(delay_samples - 5) * 2].abs();
        assert!(tap_l > 0.5, "expected delayed impulse at tap, got {tap_l}");
        assert!(pre < 0.01, "expected silence before tap, got {pre}");
    }

    #[test]
    fn delay_ping_pong_swaps_channels() {
        let mut d = DelayState::new();
        d.set_time_ms(5);
        d.set_feedback(127);  // hard feedback (clamped to 0.95 internally)
        let delay_samples = (5 * SAMPLE_RATE / 1000) as usize;
        let n_frames = delay_samples * 4 + 8;
        let mut buf = vec![0.0_f32; n_frames * 2];
        // Impulse only on L.
        buf[0] = 1.0;
        buf[1] = 0.0;
        d.process_block(&mut buf);
        // After the first delay, the impulse should have copied to L's
        // tap output. After the second delay, cross-feedback should
        // have moved energy into R.
        let r_after_two_taps = buf[(delay_samples * 2) * 2 + 1].abs();
        assert!(
            r_after_two_taps > 0.1,
            "ping-pong should have moved energy into R, got {r_after_two_taps}",
        );
    }

    #[test]
    fn delay_feedback_clamped_below_unity() {
        let mut d = DelayState::new();
        d.set_feedback(127);
        // We can't read the field directly but we can confirm by
        // running many block-passes against an impulse that the energy
        // *decays* rather than growing.
        d.set_time_ms(5);
        let delay_samples = (5 * SAMPLE_RATE / 1000) as usize;
        let n_frames = delay_samples * 20 + 8;
        let mut buf = vec![0.0_f32; n_frames * 2];
        buf[0] = 1.0;
        buf[1] = 1.0;
        d.process_block(&mut buf);
        let early = buf[(delay_samples) * 2].abs();
        let late = buf[(delay_samples * 15) * 2].abs();
        assert!(
            late < early,
            "feedback should decay across time: early {early}, late {late}",
        );
    }
}
