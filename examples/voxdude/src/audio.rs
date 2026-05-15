//! Music + SFX wiring.
//!
//! Patches and the bundled SMF (`audio/songs/voxel-synapse.mid`) live
//! on disk and get packed into the cart's Audio section by the
//! bundler; the host pre-populates them before `init()` runs. This
//! module only handles the cart-side routing: which MIDI channel
//! plays which patch, channel volume + FX-send balance, and the
//! `music_play` call that starts the loop.
//!
//! The SFX trigger sites (chomp / power-pellet / win / death) live
//! inline in the gameplay code that fires them; the note + patch
//! constants below are just shared so those call sites stay readable.

use voxlconsl_sdk::audio;

// ── Patch slot table (matches audio/patches.toml) ────────────────

pub(crate) const PATCH_CHOMP: u8 = 0;
pub(crate) const PATCH_PING:  u8 = 1;

const PATCH_TRI_BASS:    u8 = 2;
const PATCH_SAW_BASS:    u8 = 3;
const PATCH_SQUARE_LEAD: u8 = 4;
const PATCH_SAW_LEAD:    u8 = 5;
const PATCH_PAD:         u8 = 6;

const MUSIC_SLOT: u8 = 0;

// ── SFX note pitches ──────────────────────────────────────────────

pub(crate) const NOTE_CHOMP:        u8 = 72; // C5 — short bright stab on each dot
pub(crate) const NOTE_POWER_PELLET: u8 = 76; // E5 — power-up
pub(crate) const NOTE_GHOST_EATEN:  u8 = 84; // C6 — bright reward note
pub(crate) const NOTE_DEATH:        u8 = 48; // C3 — low descending pang
pub(crate) const NOTE_WIN:          u8 = 79; // G5 — triumphant ping

// ── Boot ──────────────────────────────────────────────────────────

/// Route every melodic MIDI channel of `voxel-synapse.mid` onto one
/// of the five chiptune patches, set per-channel volume + FX sends,
/// and kick off the looped song.
///
/// The audio worklet is asleep until the user clicks the canvas to
/// grab pointer lock, so these events queue up in `world.audio_events`
/// and drain in order the moment `AudioContext` resumes — music starts
/// from beat 0 the instant the player engages with the canvas.
pub(crate) fn init_music() {
    // ── Channel → patch routing ──────────────────────────────────
    //
    // The SMF carries `Bass:` / `Synth:` prefixed track names from
    // the stem-to-MIDI conversion. The bass register goes to triangle
    // or growly saw bass; leads/brass/reed land on bright square or
    // saw lead; the dense sustained polyphonic content (Synth:
    // Strings, Synth: Organ) gets the slow-attack pad so the wash
    // sits behind the melody instead of competing with it.

    // Bass tier — low-end content.
    audio::program_change(2, PATCH_TRI_BASS);    // Bass: Piano
    audio::program_change(3, PATCH_SAW_BASS);    // Bass: Organ
    audio::program_change(4, PATCH_SAW_BASS);    // Bass: Guitar
    audio::program_change(5, PATCH_TRI_BASS);    // Bass: Bass
    audio::program_change(6, PATCH_TRI_BASS);    // Bass: Strings
    audio::program_change(7, PATCH_SAW_BASS);    // Bass: Synth Lead
    audio::program_change(8, PATCH_TRI_BASS);    // Bass: Synth Pad

    // Lead tier — bright melodic content.
    audio::program_change( 0, PATCH_SQUARE_LEAD); // Synth: Synth Lead
    audio::program_change(14, PATCH_SQUARE_LEAD); // Synth: Brass
    audio::program_change(15, PATCH_SQUARE_LEAD); // Synth: Reed
    audio::program_change(10, PATCH_SAW_LEAD);    // Synth: Piano
    audio::program_change(12, PATCH_SAW_LEAD);    // Synth: Guitar (clean)

    // Pad tier — sustained polyphonic wash.
    audio::program_change(11, PATCH_PAD);         // Synth: Organ
    audio::program_change(13, PATCH_PAD);         // Synth: Strings

    // ── Volume balance (CC 7) ────────────────────────────────────
    // Pads sit lower so the lead line cuts; the dense Synth: Strings
    // channel is further pulled back so its polyphony doesn't
    // dominate the voice pool.
    for &ch in &[2u8, 3, 4, 5, 6, 7, 8] { audio::cc(ch, audio::CC_VOLUME, 90); }
    for &ch in &[0u8, 14, 15, 10, 12]   { audio::cc(ch, audio::CC_VOLUME, 110); }
    audio::cc(11, audio::CC_VOLUME, 75);
    audio::cc(13, audio::CC_VOLUME, 70);

    // ── FX sends ─────────────────────────────────────────────────
    // Leads + pads ride a hint of reverb + delay; bass stays dry so
    // it doesn't smear; drum bus gets a touch of delay for
    // arcade-cabinet feel.
    audio::reverb_set(/*room_size*/70, /*damping*/55);
    audio::delay_set(/*time_ms*/220, /*feedback*/45);
    for &ch in &[0u8, 14, 15, 10, 12] {
        audio::cc(ch, audio::CC_REVERB_SEND, 55);
        audio::cc(ch, audio::CC_DELAY_SEND,  30);
    }
    for &ch in &[11u8, 13] {
        audio::cc(ch, audio::CC_REVERB_SEND, 85);
        audio::cc(ch, audio::CC_DELAY_SEND,  20);
    }
    audio::cc(9, audio::CC_DELAY_SEND, 30);

    // Start the looped song.
    audio::music_play(MUSIC_SLOT, /*loop_*/true);
}
