//! Audio routing: channel → patch mapping + FX-bus tuning. Patches,
//! samples, and the SMF song live on disk; the bundler packs them
//! into the cart's Audio section and the host replays them into the
//! audio state before `init` runs (§5, §7). This module only does the
//! cart-side runtime configuration.

use voxlconsl_sdk::audio::{self, CC_DELAY_SEND, CC_REVERB_SEND, DRUM_CHANNEL};

/// MIDI note the SPACE-key sustain trigger plays on channel 0 (default
/// dual-saw lead patch).
pub(crate) const SYNTH_NOTE: u8 = 57; // A3 — comfortable lead-line pitch

const BELL_PATCH:    u8 = 2;  // FM2OP — bound to channel 2.
const SAMPLER_PATCH: u8 = 3;  // sampler — bound to channel 3.

/// One-shot channel routing + FX setup. Called from `init()`.
pub(crate) fn configure() {
    audio::program_change(/*channel*/2, BELL_PATCH);
    audio::cc(/*channel*/2, CC_REVERB_SEND, 90);
    audio::program_change(/*channel*/3, SAMPLER_PATCH);
    audio::cc(/*channel*/3, CC_REVERB_SEND, 80);
    audio::cc(/*channel*/3, CC_DELAY_SEND,  20);

    audio::reverb_set(/*room_size*/80, /*damping*/40);
    audio::delay_set(/*time_ms*/180, /*feedback*/55);
    audio::cc(/*channel*/0, CC_REVERB_SEND, 60);
    audio::cc(DRUM_CHANNEL, CC_DELAY_SEND,  35);
}
