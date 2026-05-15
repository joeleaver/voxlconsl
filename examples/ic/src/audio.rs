//! Title-screen music wiring.
//!
//! `audio/songs/firefighter_theme.mid` is bundled into the cart's
//! Audio section by the bundler and pre-loaded by the host before
//! `init()` runs. The MIDI carries 11 melodic channels (1-8, 10-12)
//! plus a drum track on channel 9 (0-indexed). This module assigns
//! each channel its own distinct synth voice so the arrangement
//! reads as a real ensemble instead of a few patches stacked into
//! one tone. Drums bypass this table — the host kit handles them.
//!
//! Called from `title::Title::init`; `title::Title::teardown` calls
//! `music_stop` when the player picks a mode so the game scene
//! starts silent.

use voxlconsl_sdk::audio;

// ── Patch slot table (matches audio/patches.toml) ────────────────

const PATCH_TRI_SUB:     u8 = 0;
const PATCH_SAW_BASS:    u8 = 1;
const PATCH_PLUCK_BASS:  u8 = 2;
const PATCH_SQUARE_BASS: u8 = 3;
const PATCH_SQUARE_LEAD: u8 = 4;
const PATCH_SAW_LEAD:    u8 = 5;
const PATCH_STRING_PAD:  u8 = 6;
const PATCH_BRASS:       u8 = 7;
const PATCH_REED:        u8 = 8;
const PATCH_SINE_LEAD:   u8 = 9;
const PATCH_VOICE_LEAD:  u8 = 10;
const PATCH_THUNDER:     u8 = 11;

pub(crate) const MUSIC_SLOT: u8 = 0;

// ── Boot ──────────────────────────────────────────────────────────

/// Route every melodic MIDI channel of `firefighter_theme.mid` onto
/// its own patch, set per-channel volume + FX-send balance, and
/// start the looped song.
///
/// The audio worklet is asleep until the user clicks the canvas to
/// grab pointer lock, so these events queue up in the host's audio
/// event ring and drain in order the moment `AudioContext` resumes —
/// music starts from beat 0 the instant the player engages.
pub(crate) fn init_title_music() {
    // ── Channel → patch routing (each channel gets a unique voice) ─
    //
    // Bass tier — low-end content. Each bass track has its own
    // timbre so they layer without smearing.
    audio::program_change(1, PATCH_TRI_SUB);     // Bass: Acoustic Piano
    audio::program_change(2, PATCH_SAW_BASS);    // Bass: Organ
    audio::program_change(3, PATCH_PLUCK_BASS);  // Bass: Guitar (clean)
    audio::program_change(4, PATCH_SQUARE_BASS); // Bass: Bass

    // Lead + supporting melodic tier.
    audio::program_change(5,  PATCH_SQUARE_LEAD); // Synth: E.Piano — main melody
    audio::program_change(6,  PATCH_SAW_LEAD);    // Synth: Guitar (clean)
    audio::program_change(7,  PATCH_STRING_PAD);  // Synth: Strings
    audio::program_change(8,  PATCH_BRASS);       // Synth: Brass
    audio::program_change(10, PATCH_REED);        // Synth: Reed
    audio::program_change(11, PATCH_SINE_LEAD);   // Synth: Synth Lead
    audio::program_change(12, PATCH_VOICE_LEAD);  // Synth: Singing Voice

    // ── Volume balance (CC 7) ────────────────────────────────────
    // Lead voice + main melody cut on top; bass sits a notch under;
    // pad + reed support the wash quietly; the sparse high-octave
    // voices (sine_lead, voice_lead) are loud enough to read as
    // accents rather than ambient.
    audio::cc(1, audio::CC_VOLUME,  95);  // tri_sub
    audio::cc(2, audio::CC_VOLUME,  90);  // saw_bass
    audio::cc(3, audio::CC_VOLUME,  85);  // pluck_bass
    audio::cc(4, audio::CC_VOLUME,  90);  // square_bass
    audio::cc(5, audio::CC_VOLUME, 115);  // square_lead (main melody)
    audio::cc(6, audio::CC_VOLUME, 100);  // saw_lead
    audio::cc(7, audio::CC_VOLUME,  70);  // string_pad
    audio::cc(8, audio::CC_VOLUME, 100);  // brass
    audio::cc(10, audio::CC_VOLUME, 80);  // reed
    audio::cc(11, audio::CC_VOLUME, 95);  // sine_lead
    audio::cc(12, audio::CC_VOLUME, 90);  // voice_lead

    // ── FX sends ─────────────────────────────────────────────────
    // Cinematic bloom on leads + brass + voice; heavier reverb on
    // the pad layer; bass stays dry; drums get a hint of delay for
    // arcade-cabinet feel.
    audio::reverb_set(/*room_size*/80, /*damping*/55);
    audio::delay_set(/*time_ms*/250, /*feedback*/40);
    for &ch in &[5u8, 6, 8, 11, 12] {
        audio::cc(ch, audio::CC_REVERB_SEND, 60);
        audio::cc(ch, audio::CC_DELAY_SEND,  30);
    }
    for &ch in &[7u8, 10] {
        audio::cc(ch, audio::CC_REVERB_SEND, 90);
        audio::cc(ch, audio::CC_DELAY_SEND,  20);
    }
    audio::cc(9, audio::CC_DELAY_SEND, 25);

    // Start the looped song.
    audio::music_play(MUSIC_SLOT, /*loop_*/true);
}

/// Thunder hit. Triggered alongside any lightning bolt visual — both
/// the title-screen strike and in-game season strikes route here. A
/// single low-noise voice through the thunder patch; the patch's amp
/// envelope releases naturally, so we trigger + drop the voice
/// immediately and let the envelope tail out.
pub(crate) fn play_thunder() {
    let voice = audio::voice_trigger(PATCH_THUNDER, /*note*/36, /*velocity*/120);
    if voice.is_some() {
        audio::voice_release(voice);
    }
}

/// Stop the title song and silence any tail-ringing voices. Called
/// from `Title::teardown` when the player picks a mode so the game
/// scene starts silent.
pub(crate) fn stop_title_music() {
    audio::music_stop();
    // Belt-and-suspenders: explicit all-notes-off on every melodic
    // channel so released-voice tails don't bleed into gameplay.
    for ch in 0u8..16 {
        audio::all_notes_off(ch);
    }
}
