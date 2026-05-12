//! Cart→audio event log — written from sandbox.rs audio imports,
//! drained by main.js after each cart frame, postMessaged to the
//! AudioWorkletProcessor where the worklet wasm replays each event
//! against the §5 mixer running on the audio thread (SPEC.md §5.8).
//!
//! ## Wire format
//!
//! Single `Vec<u8>` byte stream. Each event is a 1-byte tag followed
//! by tag-specific fixed args. Byte-payload events (sample_load,
//! music_load) include a u32 length followed by N bytes inline.
//!
//! Integers are little-endian. Signed fields use two's complement.
//!
//! Keep the `EventTag` constants in lockstep with `EVT.*` in
//! `web/audio-worklet.js`.

extern crate alloc;

use alloc::vec::Vec;

#[repr(u8)]
pub enum EventTag {
    NoteOn              = 0,
    NoteOff             = 1,
    AllNotesOff         = 2,
    PitchBend           = 3,
    Cc                  = 4,
    ProgramChange       = 5,
    PatchSetOsc         = 6,
    PatchSetFilter      = 7,
    PatchSetAmpEnv      = 8,
    PatchSetFilterEnv   = 9,
    PatchSetLfo         = 10,
    PatchSetGlide       = 11,
    PatchSetFm          = 12,
    PatchSetKind        = 13,
    PatchSetZone        = 14,
    PatchSetZoneCount   = 15,
    PatchReset          = 16,
    PatchCopy           = 17,
    MusicPlay           = 18,
    MusicStop           = 19,
    MusicSetTempoScale  = 20,
    ReverbSet           = 21,
    DelaySet            = 22,
    SampleLoad          = 23,
    MusicLoad           = 24,
    SfxPlay             = 25,
    SfxStop             = 26,
    SfxSetVolume        = 27,
    SfxSetPitch         = 28,
    VoiceTrigger        = 29,
    VoiceRelease        = 30,
}

/// Event log living inside `WorldState`. The cart's audio imports
/// write directly to this; the browser-host shim drains it after
/// each `cart.frame()` and relays each event to the worklet via
/// `port.postMessage`.
#[derive(Default)]
pub struct AudioEventLog {
    pub buf: Vec<u8>,
}

impl AudioEventLog {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(4096) }
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    #[inline]
    fn tag(&mut self, t: EventTag) {
        self.buf.push(t as u8);
    }

    #[inline]
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    #[inline]
    fn i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    #[inline]
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn push_note_on(&mut self, token: u32, channel: u8, note: u8, velocity: u8) {
        self.tag(EventTag::NoteOn);
        self.u32(token);
        self.u8(channel); self.u8(note); self.u8(velocity);
    }

    pub fn push_note_off(&mut self, channel: u8, note: u8) {
        self.tag(EventTag::NoteOff);
        self.u8(channel); self.u8(note);
    }

    pub fn push_all_notes_off(&mut self, channel: u8) {
        self.tag(EventTag::AllNotesOff);
        self.u8(channel);
    }

    pub fn push_pitch_bend(&mut self, channel: u8, value: i16) {
        self.tag(EventTag::PitchBend);
        self.u8(channel); self.i16(value);
    }

    pub fn push_cc(&mut self, channel: u8, controller: u8, value: u8) {
        self.tag(EventTag::Cc);
        self.u8(channel); self.u8(controller); self.u8(value);
    }

    pub fn push_program_change(&mut self, channel: u8, patch: u8) {
        self.tag(EventTag::ProgramChange);
        self.u8(channel); self.u8(patch);
    }

    pub fn push_patch_set_osc(&mut self, slot: u8, osc_idx: u8, mode: u8, detune_cents: i16, octave: i8, level: u8) {
        self.tag(EventTag::PatchSetOsc);
        self.u8(slot); self.u8(osc_idx); self.u8(mode);
        self.i16(detune_cents); self.i8(octave); self.u8(level);
    }

    pub fn push_patch_set_filter(&mut self, slot: u8, mode: u8, cutoff_hz: u16, resonance: u8) {
        self.tag(EventTag::PatchSetFilter);
        self.u8(slot); self.u8(mode); self.u16(cutoff_hz); self.u8(resonance);
    }

    pub fn push_patch_set_amp_env(&mut self, slot: u8, a: u16, d: u16, s: u8, r: u16) {
        self.tag(EventTag::PatchSetAmpEnv);
        self.u8(slot); self.u16(a); self.u16(d); self.u8(s); self.u16(r);
    }

    pub fn push_patch_set_filter_env(&mut self, slot: u8, a: u16, d: u16, s: u8, r: u16, depth: i8) {
        self.tag(EventTag::PatchSetFilterEnv);
        self.u8(slot); self.u16(a); self.u16(d); self.u8(s); self.u16(r); self.i8(depth);
    }

    pub fn push_patch_set_lfo(&mut self, slot: u8, rate_centihz: u16, shape: u8, target: u8, depth: i8) {
        self.tag(EventTag::PatchSetLfo);
        self.u8(slot); self.u16(rate_centihz); self.u8(shape); self.u8(target); self.i8(depth);
    }

    pub fn push_patch_set_glide(&mut self, slot: u8, ms: u16) {
        self.tag(EventTag::PatchSetGlide);
        self.u8(slot); self.u16(ms);
    }

    pub fn push_patch_set_fm(&mut self, slot: u8, ratio_q88: u16, index_q88: u16) {
        self.tag(EventTag::PatchSetFm);
        self.u8(slot); self.u16(ratio_q88); self.u16(index_q88);
    }

    pub fn push_patch_set_kind(&mut self, slot: u8, kind: u8) {
        self.tag(EventTag::PatchSetKind);
        self.u8(slot); self.u8(kind);
    }

    pub fn push_patch_set_zone(
        &mut self,
        slot: u8, zone_idx: u8,
        low: u8, high: u8, root: u8,
        sample_slot: u8, volume_offset: i8,
        loop_start: u32, loop_end: u32, loop_enabled: bool,
    ) {
        self.tag(EventTag::PatchSetZone);
        self.u8(slot); self.u8(zone_idx);
        self.u8(low); self.u8(high); self.u8(root);
        self.u8(sample_slot); self.i8(volume_offset);
        self.u32(loop_start); self.u32(loop_end);
        self.u8(if loop_enabled { 1 } else { 0 });
    }

    pub fn push_patch_set_zone_count(&mut self, slot: u8, count: u8) {
        self.tag(EventTag::PatchSetZoneCount);
        self.u8(slot); self.u8(count);
    }

    pub fn push_patch_reset(&mut self, slot: u8) {
        self.tag(EventTag::PatchReset);
        self.u8(slot);
    }

    pub fn push_patch_copy(&mut self, src: u8, dst: u8) {
        self.tag(EventTag::PatchCopy);
        self.u8(src); self.u8(dst);
    }

    pub fn push_music_play(&mut self, slot: u8, loop_: bool) {
        self.tag(EventTag::MusicPlay);
        self.u8(slot); self.u8(if loop_ { 1 } else { 0 });
    }

    pub fn push_music_stop(&mut self) {
        self.tag(EventTag::MusicStop);
    }

    pub fn push_music_set_tempo_scale(&mut self, scale: f32) {
        self.tag(EventTag::MusicSetTempoScale);
        self.f32(scale);
    }

    pub fn push_reverb_set(&mut self, room_size: u8, damping: u8) {
        self.tag(EventTag::ReverbSet);
        self.u8(room_size); self.u8(damping);
    }

    pub fn push_delay_set(&mut self, time_ms: u16, feedback: u8) {
        self.tag(EventTag::DelaySet);
        self.u16(time_ms); self.u8(feedback);
    }

    pub fn push_sample_load(
        &mut self,
        slot: u8, rate_code: u8, flags: u8,
        loop_start: u32, loop_end: u32, payload: &[u8],
    ) {
        self.tag(EventTag::SampleLoad);
        self.u8(slot); self.u8(rate_code); self.u8(flags);
        self.u32(loop_start); self.u32(loop_end);
        self.u32(payload.len() as u32);
        self.buf.extend_from_slice(payload);
    }

    pub fn push_music_load(&mut self, slot: u8, payload: &[u8]) {
        self.tag(EventTag::MusicLoad);
        self.u8(slot);
        self.u32(payload.len() as u32);
        self.buf.extend_from_slice(payload);
    }

    pub fn push_sfx_play(&mut self, token: u32, slot: u8, volume: u8, pan: i8, pitch_cents: i16, loop_: bool) {
        self.tag(EventTag::SfxPlay);
        self.u32(token);
        self.u8(slot); self.u8(volume); self.i8(pan); self.i16(pitch_cents);
        self.u8(if loop_ { 1 } else { 0 });
    }

    pub fn push_sfx_stop(&mut self, voice: u32) {
        self.tag(EventTag::SfxStop);
        self.u32(voice);
    }

    pub fn push_sfx_set_volume(&mut self, voice: u32, volume: u8) {
        self.tag(EventTag::SfxSetVolume);
        self.u32(voice); self.u8(volume);
    }

    pub fn push_sfx_set_pitch(&mut self, voice: u32, pitch_cents: i16) {
        self.tag(EventTag::SfxSetPitch);
        self.u32(voice); self.i16(pitch_cents);
    }

    pub fn push_voice_trigger(&mut self, token: u32, patch: u8, note: u8, velocity: u8) {
        self.tag(EventTag::VoiceTrigger);
        self.u32(token);
        self.u8(patch); self.u8(note); self.u8(velocity);
    }

    pub fn push_voice_release(&mut self, voice: u32) {
        self.tag(EventTag::VoiceRelease);
        self.u32(voice);
    }

    /// Emit the full per-field event sequence for a `Patch` — used at
    /// cart load to replay an Audio-section patch through the worklet.
    /// Mirrors what the cart-side `patch_set_*` imports would push if
    /// the cart configured the patch field-by-field at runtime.
    pub fn push_patch_full(&mut self, slot: u8, patch: &voxlconsl_audio::Patch) {
        use voxlconsl_audio::{
            filter_mode_code, lfo_shape_code, lfo_target_code, osc_mode_code,
            patch_kind_code,
        };
        self.push_patch_set_kind(slot, patch_kind_code(patch.kind));
        for (i, osc) in patch.osc.iter().enumerate() {
            self.push_patch_set_osc(
                slot, i as u8, osc_mode_code(osc.mode),
                osc.detune_cents, osc.octave, osc.level,
            );
        }
        self.push_patch_set_filter(
            slot, filter_mode_code(patch.filter.mode),
            patch.filter.cutoff_hz, patch.filter.resonance,
        );
        self.push_patch_set_amp_env(
            slot, patch.amp_env.attack_ms, patch.amp_env.decay_ms,
            patch.amp_env.sustain, patch.amp_env.release_ms,
        );
        self.push_patch_set_filter_env(
            slot, patch.filter_env.attack_ms, patch.filter_env.decay_ms,
            patch.filter_env.sustain, patch.filter_env.release_ms,
            patch.filter_env_depth,
        );
        self.push_patch_set_lfo(
            slot, patch.lfo.rate_centihz,
            lfo_shape_code(patch.lfo.shape), lfo_target_code(patch.lfo.target),
            patch.lfo.depth,
        );
        self.push_patch_set_glide(slot, patch.glide_ms);
        self.push_patch_set_fm(slot, patch.fm_ratio, patch.fm_index);
        for i in 0..(patch.zone_count as usize).min(patch.zones.len()) {
            let z = &patch.zones[i];
            self.push_patch_set_zone(
                slot, i as u8, z.low_note, z.high_note, z.root_note,
                z.sample_slot, z.volume_offset,
                z.loop_start, z.loop_end, z.loop_enabled,
            );
        }
        self.push_patch_set_zone_count(slot, patch.zone_count);
    }
}
