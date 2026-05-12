//! voxlconsl-audio-worklet — raw wasm module loaded by the
//! AudioWorkletProcessor at `web/audio-worklet.js`.
//!
//! Runs the entire §5 mixer on the audio thread, decoupled from the
//! main-thread renderer. Cart-facing audio imports (note_on,
//! sample_load, etc.) are still received on the main thread by
//! `crates/host/src/sandbox.rs`; the shim repackages each call as a
//! message + posts it to the worklet, where this wasm applies it to
//! its `AudioState`. Audio output is written directly to f32 buffers
//! that the worklet's `process()` method copies into its outputs.
//!
//! Deliberately uses raw `extern "C"` exports rather than
//! wasm-bindgen — AudioWorkletGlobalScope can't load ES modules, so
//! we stay with `fetch + WebAssembly.instantiate` and read function
//! exports directly off the resulting instance.

#![no_main]

use voxlconsl_audio::{
    AudioState, FilterMode, KeyZone, LfoShape, LfoTarget, OscMode, PatchKind, Sample,
    SampleRate, VoiceId, BLOCK_FRAMES, BLOCK_SAMPLES,
};

/// Worklet block size in frames. AudioWorkletProcessor.process() is
/// usually called with 128-frame chunks. We render 2 × 64-frame
/// mixer blocks per call.
const OUT_FRAMES: usize = 128;

/// Stereo output scratch buffers — fixed-size statics living in
/// wasm linear memory. JS reads from them after each `render()`
/// call and copies into the worklet's outputs[0][0] / [0][1].
static mut OUT_L: [f32; OUT_FRAMES] = [0.0; OUT_FRAMES];
static mut OUT_R: [f32; OUT_FRAMES] = [0.0; OUT_FRAMES];

/// Mixer state. Lives on the worklet thread for the entire session.
/// `Option<Box<…>>` because `AudioState::new()` allocates and can't
/// be evaluated in a const context.
static mut STATE: Option<Box<AudioState>> = None;

#[inline]
fn state() -> &'static mut AudioState {
    unsafe { (*(&raw mut STATE)).as_deref_mut().expect("init() not called") }
}

// ── Lifecycle ───────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn init() {
    unsafe {
        *(&raw mut STATE) = Some(Box::new(AudioState::new()));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn out_l_ptr() -> *const f32 {
    (&raw const OUT_L).cast::<f32>()
}

#[unsafe(no_mangle)]
pub extern "C" fn out_r_ptr() -> *const f32 {
    (&raw const OUT_R).cast::<f32>()
}

#[unsafe(no_mangle)]
pub extern "C" fn out_frames() -> u32 {
    OUT_FRAMES as u32
}

/// Render `OUT_FRAMES` samples. Internally calls
/// `AudioState::render_block` twice (2 × 64 = 128 frames) and
/// converts the i16 stereo output to f32 stored in the scratch
/// buffers above. The worklet's JS-side `process()` copies these
/// into the AudioWorkletProcessor's output channels.
#[unsafe(no_mangle)]
pub extern "C" fn render() {
    let s = state();
    let mut scratch = [0i16; BLOCK_SAMPLES];
    let mut frames_done = 0;
    let out_l: *mut [f32; OUT_FRAMES] = &raw mut OUT_L;
    let out_r: *mut [f32; OUT_FRAMES] = &raw mut OUT_R;
    while frames_done < OUT_FRAMES {
        s.render_block(&mut scratch);
        for i in 0..BLOCK_FRAMES {
            unsafe {
                (*out_l)[frames_done + i] = scratch[i * 2] as f32 / 32768.0;
                (*out_r)[frames_done + i] = scratch[i * 2 + 1] as f32 / 32768.0;
            }
        }
        frames_done += BLOCK_FRAMES;
    }
}

// ── Allocator (raw bytes for sample / SMF / patch-blob transfer) ─

/// Allocate `len` bytes in wasm linear memory. JS uses this to
/// stage byte payloads (sample PCM, SMF blobs, patch blobs) before
/// calling the corresponding `*_load` export.
///
/// The returned pointer is opaque from JS's perspective — pass it
/// straight back to the matching call and then `dealloc`.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: u32) -> *mut u8 {
    let mut v: Vec<u8> = Vec::with_capacity(len as usize);
    let ptr = v.as_mut_ptr();
    core::mem::forget(v);
    ptr
}

#[unsafe(no_mangle)]
pub extern "C" fn dealloc(ptr: *mut u8, len: u32) {
    unsafe {
        let _ = Vec::from_raw_parts(ptr, len as usize, len as usize);
    }
}

// ── Cart-facing API mirroring `AudioState` + sandbox.rs ─────────

#[unsafe(no_mangle)]
pub extern "C" fn sample_load(
    slot: u32, ptr: *const u8, len: u32,
    rate_code: u32, flags: u32,
    loop_start: u32, loop_end: u32,
) -> u32 {
    let data = unsafe { core::slice::from_raw_parts(ptr, len as usize) }.to_vec();
    let rate = SampleRate::from_code(rate_code as u8);
    let loop_points = if flags & 0x1 != 0 { Some((loop_start, loop_end)) } else { None };
    state().register_sample(slot as u8, Sample { data, rate, loop_points });
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn sfx_play(slot: u32, volume: u32, pan: i32, pitch_cents: i32, loop_: u32) -> u32 {
    state().sfx_play(slot as u8, volume as u8, pan as i8, pitch_cents as i16, loop_ != 0).0
}

#[unsafe(no_mangle)]
pub extern "C" fn sfx_stop(voice: u32) {
    state().sfx_stop(VoiceId(voice));
}

#[unsafe(no_mangle)]
pub extern "C" fn sfx_set_volume(voice: u32, volume: u32) {
    state().sfx_set_volume(VoiceId(voice), volume as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn sfx_set_pitch(voice: u32, pitch_cents: i32) {
    state().sfx_set_pitch(VoiceId(voice), pitch_cents as i16);
}

#[unsafe(no_mangle)]
pub extern "C" fn note_on(channel: u32, note: u32, velocity: u32) -> u32 {
    state().note_on(channel as u8, note as u8, velocity as u8).0
}

#[unsafe(no_mangle)]
pub extern "C" fn note_off(channel: u32, note: u32) {
    state().note_off(channel as u8, note as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn pitch_bend(channel: u32, value: i32) {
    state().pitch_bend(channel as u8, value as i16);
}

#[unsafe(no_mangle)]
pub extern "C" fn cc(channel: u32, controller: u32, value: u32) {
    state().cc(channel as u8, controller as u8, value as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn program_change(channel: u32, patch: u32) {
    state().program_change(channel as u8, patch as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn all_notes_off(channel: u32) {
    state().all_notes_off(channel as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn voice_trigger(patch: u32, note: u32, velocity: u32) -> u32 {
    state().voice_trigger(patch as u8, note as u8, velocity as u8).0
}

#[unsafe(no_mangle)]
pub extern "C" fn voice_release(voice: u32) {
    state().voice_release(VoiceId(voice));
}

// Patch editing — straightforward primitive marshaling.

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_osc(
    slot: u32, osc_idx: u32,
    mode: u32, detune_cents: i32, octave: i32, level: u32,
) {
    state().patch_set_osc(
        slot as u8, osc_idx as u8,
        OscMode::from_code(mode as u8),
        detune_cents as i16, octave as i8, level as u8,
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_filter(slot: u32, mode: u32, cutoff_hz: u32, resonance: u32) {
    state().patch_set_filter(
        slot as u8,
        FilterMode::from_code(mode as u8),
        cutoff_hz as u16, resonance as u8,
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_amp_env(slot: u32, attack_ms: u32, decay_ms: u32, sustain: u32, release_ms: u32) {
    state().patch_set_amp_env(slot as u8, attack_ms as u16, decay_ms as u16, sustain as u8, release_ms as u16);
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_filter_env(slot: u32, attack_ms: u32, decay_ms: u32, sustain: u32, release_ms: u32, depth: i32) {
    state().patch_set_filter_env(slot as u8, attack_ms as u16, decay_ms as u16, sustain as u8, release_ms as u16, depth as i8);
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_lfo(slot: u32, rate_centihz: u32, shape: u32, target: u32, depth: i32) {
    state().patch_set_lfo(
        slot as u8, rate_centihz as u16,
        LfoShape::from_code(shape as u8),
        LfoTarget::from_code(target as u8),
        depth as i8,
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_glide(slot: u32, ms: u32) {
    state().patch_set_glide(slot as u8, ms as u16);
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_fm(slot: u32, ratio_q88: u32, index_q88: u32) {
    state().patch_set_fm(slot as u8, ratio_q88 as u16, index_q88 as u16);
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_kind(slot: u32, kind_code: u32) {
    state().patch_set_kind(slot as u8, PatchKind::from_code(kind_code as u8));
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_zone_count(slot: u32, count: u32) {
    state().patch_set_zone_count(slot as u8, count as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_set_zone(
    slot: u32, zone_idx: u32,
    low_note: u32, high_note: u32, root_note: u32,
    sample_slot: u32, volume_offset: i32,
    loop_start: u32, loop_end: u32, loop_enabled: u32,
) {
    state().patch_set_zone(
        slot as u8, zone_idx as u8,
        KeyZone {
            low_note: low_note as u8,
            high_note: high_note as u8,
            root_note: root_note as u8,
            sample_slot: sample_slot as u8,
            volume_offset: volume_offset as i8,
            loop_start, loop_end,
            loop_enabled: loop_enabled != 0,
        },
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_reset(slot: u32) {
    state().patch_reset(slot as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_copy(src: u32, dst: u32) {
    state().patch_copy(src as u8, dst as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_save(slot: u32, ptr: *mut u8, max_len: u32) -> u32 {
    let out = unsafe { core::slice::from_raw_parts_mut(ptr, max_len as usize) };
    state().patch_save(slot as u8, out)
}

#[unsafe(no_mangle)]
pub extern "C" fn patch_load(slot: u32, ptr: *const u8, len: u32) -> u32 {
    let src = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    if state().patch_load(slot as u8, src) { 1 } else { 0 }
}

// ── SMF / song API ─────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn music_load(slot: u32, ptr: *const u8, len: u32) -> u32 {
    let src = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    if state().load_song(slot as u8, src) { 1 } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn music_play(slot: u32, loop_: u32) {
    state().music_play(slot as u8, loop_ != 0);
}

#[unsafe(no_mangle)]
pub extern "C" fn music_stop() {
    state().music_stop();
}

#[unsafe(no_mangle)]
pub extern "C" fn music_set_tempo_scale(scale: f32) {
    state().music_set_tempo_scale(scale);
}

#[unsafe(no_mangle)]
pub extern "C" fn music_position_beats() -> f32 {
    state().music_position_beats()
}

// ── Effects bus ─────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn reverb_set(room_size: u32, damping: u32) {
    state().reverb_set(room_size as u8, damping as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn delay_set(time_ms: u32, feedback: u32) {
    state().delay_set(time_ms as u16, feedback as u8);
}

// ── Diagnostics ────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn active_voice_count() -> u32 {
    state().active_voice_count() as u32
}

// std supplies the panic_impl lang item under wasm32-unknown-unknown,
// and `panic = "abort"` in Cargo.toml gives us trap-on-panic behaviour
// without needing a custom handler.
