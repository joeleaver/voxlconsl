// AudioWorkletProcessor for the Stage-4b audio path (SPEC.md §5.8).
//
// Phases 2c+ (v0.1.19): runs the full §5 mixer on the audio thread
// via a wasm instance compiled from `voxlconsl-audio-worklet`. The
// main thread:
//   1. fetches `audio-worklet.wasm` bytes and passes them in via
//      processorOptions at AudioWorkletNode construction;
//   2. relays each cart audio call as a `port.postMessage` event
//      with the args (and a copy of any byte payload like a sample
//      or SMF blob);
//   3. reads back `music_position_beats` and friends from periodic
//      state-mirror messages we post out the other way.
//
// Inside `process()` we:
//   1. drain the queued events into the worklet wasm via raw exports;
//   2. call `render()` once to produce one block of audio;
//   3. copy the wasm's f32 output buffers into outputs[0][0] / [0][1].
//
// Event tags are the same enum as in `host-browser`'s
// `audio_events.rs`. Keep them in sync with that file.
const EVT = {
    NOTE_ON:               0,
    NOTE_OFF:              1,
    ALL_NOTES_OFF:         2,
    PITCH_BEND:            3,
    CC:                    4,
    PROGRAM_CHANGE:        5,
    PATCH_SET_OSC:         6,
    PATCH_SET_FILTER:      7,
    PATCH_SET_AMP_ENV:     8,
    PATCH_SET_FILTER_ENV:  9,
    PATCH_SET_LFO:         10,
    PATCH_SET_GLIDE:       11,
    PATCH_SET_FM:          12,
    PATCH_SET_KIND:        13,
    PATCH_SET_ZONE:        14,
    PATCH_SET_ZONE_COUNT:  15,
    PATCH_RESET:           16,
    PATCH_COPY:            17,
    MUSIC_PLAY:            18,
    MUSIC_STOP:            19,
    MUSIC_SET_TEMPO_SCALE: 20,
    REVERB_SET:            21,
    DELAY_SET:             22,
    SAMPLE_LOAD:           23,
    MUSIC_LOAD:            24,
    SFX_PLAY:              25,
    SFX_STOP:              26,
    SFX_SET_VOLUME:        27,
    SFX_SET_PITCH:         28,
    VOICE_TRIGGER:         29,
    VOICE_RELEASE:         30,
};

class VoxlMixerProcessor extends AudioWorkletProcessor {
    constructor(options) {
        super();
        const opts = options.processorOptions;
        this.wasmReady = false;
        this.events = [];
        this.statePostFrameCounter = 0;
        // Cart-allocated VoiceId tokens → worklet's real VoiceIds.
        // Populated when a voice-creating event arrives, consulted
        // when the cart later calls sfx_stop / voice_release / etc.
        // with the token. Stale entries (voices that auto-freed)
        // leak slowly; we'll add cleanup once it matters.
        this.voiceTokens = new Map();
        // Async wasm load. `process()` outputs silence until ready.
        WebAssembly
            .instantiate(opts.wasmBytes, {})
            .then((result) => {
                this.wasm = result.instance.exports;
                this.wasm.init();
                this.outFrames = this.wasm.out_frames();
                this.outL = new Float32Array(this.wasm.memory.buffer, this.wasm.out_l_ptr(), this.outFrames);
                this.outR = new Float32Array(this.wasm.memory.buffer, this.wasm.out_r_ptr(), this.outFrames);
                this.wasmReady = true;
                this.port.postMessage({ type: "ready" });
            })
            .catch((err) => {
                this.port.postMessage({ type: "error", error: String(err) });
            });

        // Receive cart→audio events from main thread.
        this.port.onmessage = (e) => {
            const m = e.data;
            if (m && m.type === "event") {
                this.events.push(m);
            }
        };
    }

    /// Apply one queued event to the worklet wasm.
    applyEvent(m) {
        const w = this.wasm;
        switch (m.tag) {
            case EVT.NOTE_ON: {
                const realId = w.note_on(m.channel, m.note, m.velocity);
                if (m.token && realId) this.voiceTokens.set(m.token, realId);
                break;
            }
            case EVT.NOTE_OFF:              w.note_off(m.channel, m.note); break;
            case EVT.ALL_NOTES_OFF:         w.all_notes_off(m.channel); break;
            case EVT.PITCH_BEND:            w.pitch_bend(m.channel, m.value); break;
            case EVT.CC:                    w.cc(m.channel, m.controller, m.value); break;
            case EVT.PROGRAM_CHANGE:        w.program_change(m.channel, m.patch); break;
            case EVT.PATCH_SET_OSC:
                w.patch_set_osc(m.slot, m.osc_idx, m.mode, m.detune_cents, m.octave, m.level);
                break;
            case EVT.PATCH_SET_FILTER:
                w.patch_set_filter(m.slot, m.mode, m.cutoff_hz, m.resonance);
                break;
            case EVT.PATCH_SET_AMP_ENV:
                w.patch_set_amp_env(m.slot, m.attack_ms, m.decay_ms, m.sustain, m.release_ms);
                break;
            case EVT.PATCH_SET_FILTER_ENV:
                w.patch_set_filter_env(m.slot, m.attack_ms, m.decay_ms, m.sustain, m.release_ms, m.depth);
                break;
            case EVT.PATCH_SET_LFO:
                w.patch_set_lfo(m.slot, m.rate_centihz, m.shape, m.target, m.depth);
                break;
            case EVT.PATCH_SET_GLIDE:       w.patch_set_glide(m.slot, m.ms); break;
            case EVT.PATCH_SET_FM:          w.patch_set_fm(m.slot, m.ratio_q88, m.index_q88); break;
            case EVT.PATCH_SET_KIND:        w.patch_set_kind(m.slot, m.kind); break;
            case EVT.PATCH_SET_ZONE:
                w.patch_set_zone(
                    m.slot, m.zone_idx,
                    m.low_note, m.high_note, m.root_note,
                    m.sample_slot, m.volume_offset,
                    m.loop_start, m.loop_end, m.loop_enabled,
                );
                break;
            case EVT.PATCH_SET_ZONE_COUNT:  w.patch_set_zone_count(m.slot, m.count); break;
            case EVT.PATCH_RESET:           w.patch_reset(m.slot); break;
            case EVT.PATCH_COPY:            w.patch_copy(m.src, m.dst); break;
            case EVT.MUSIC_PLAY:            w.music_play(m.slot, m.loop); break;
            case EVT.MUSIC_STOP:            w.music_stop(); break;
            case EVT.MUSIC_SET_TEMPO_SCALE: w.music_set_tempo_scale(m.scale); break;
            case EVT.REVERB_SET:            w.reverb_set(m.room_size, m.damping); break;
            case EVT.DELAY_SET:             w.delay_set(m.time_ms, m.feedback); break;
            case EVT.SAMPLE_LOAD:
            case EVT.MUSIC_LOAD: {
                // Byte-payload events: copy bytes into worklet wasm
                // memory, call the relevant load fn, then dealloc.
                const bytes = m.bytes;
                if (!bytes) break;
                const ptr = w.alloc(bytes.byteLength);
                new Uint8Array(w.memory.buffer, ptr, bytes.byteLength).set(bytes);
                if (m.tag === EVT.SAMPLE_LOAD) {
                    w.sample_load(m.slot, ptr, bytes.byteLength,
                        m.rate_code, m.flags, m.loop_start, m.loop_end);
                } else {
                    w.music_load(m.slot, ptr, bytes.byteLength);
                }
                w.dealloc(ptr, bytes.byteLength);
                break;
            }
            case EVT.SFX_PLAY: {
                const realId = w.sfx_play(m.slot, m.volume, m.pan, m.pitch_cents, m.loop);
                if (m.token && realId) this.voiceTokens.set(m.token, realId);
                break;
            }
            case EVT.SFX_STOP: {
                const realId = this.voiceTokens.get(m.voice);
                if (realId) {
                    w.sfx_stop(realId);
                    this.voiceTokens.delete(m.voice);
                }
                break;
            }
            case EVT.SFX_SET_VOLUME: {
                const realId = this.voiceTokens.get(m.voice);
                if (realId) w.sfx_set_volume(realId, m.volume);
                break;
            }
            case EVT.SFX_SET_PITCH: {
                const realId = this.voiceTokens.get(m.voice);
                if (realId) w.sfx_set_pitch(realId, m.pitch_cents);
                break;
            }
            case EVT.VOICE_TRIGGER: {
                const realId = w.voice_trigger(m.patch, m.note, m.velocity);
                if (m.token && realId) this.voiceTokens.set(m.token, realId);
                break;
            }
            case EVT.VOICE_RELEASE: {
                const realId = this.voiceTokens.get(m.voice);
                if (realId) {
                    w.voice_release(realId);
                    this.voiceTokens.delete(m.voice);
                }
                break;
            }
        }
    }

    process(_inputs, outputs) {
        const out = outputs[0];
        const L = out[0];
        const R = out.length > 1 ? out[1] : L;
        const frames = L.length;

        if (!this.wasmReady) {
            // Wasm still loading — silent for the few render quanta
            // it takes to fetch+compile.
            for (let i = 0; i < frames; i++) {
                L[i] = 0;
                if (R !== L) R[i] = 0;
            }
            return true;
        }

        // Drain queued events and apply each to the mixer state. The
        // worklet wasm's `out_frames` is fixed at 128, which is the
        // typical AudioWorkletProcessor block size — so one render()
        // produces exactly the frames we need.
        const events = this.events;
        this.events = [];
        for (let i = 0; i < events.length; i++) {
            this.applyEvent(events[i]);
        }

        this.wasm.render();

        // Copy mixer output to the AudioContext output channels.
        // outL/outR view wasm memory; sample_load / music_load /
        // patch_load can grow it and detach the views. Always
        // re-bind against the current `memory.buffer`.
        if (this.outL.buffer !== this.wasm.memory.buffer) {
            this.outL = new Float32Array(this.wasm.memory.buffer, this.wasm.out_l_ptr(), this.outFrames);
            this.outR = new Float32Array(this.wasm.memory.buffer, this.wasm.out_r_ptr(), this.outFrames);
        }
        const n = Math.min(frames, this.outFrames);
        for (let i = 0; i < n; i++) {
            L[i] = this.outL[i];
            if (R !== L) R[i] = this.outR[i];
        }
        // If the worklet block size happens to exceed our render size
        // (rare — most browsers stick to 128), zero-fill the tail.
        for (let i = n; i < frames; i++) {
            L[i] = 0;
            if (R !== L) R[i] = 0;
        }

        // Post state mirrors back to main thread every ~6 process
        // calls (= ~35 ms at 22.05 kHz, 128 frames/call) so reads of
        // `music_position_beats` from the cart's tick aren't stale.
        this.statePostFrameCounter++;
        if (this.statePostFrameCounter >= 6) {
            this.statePostFrameCounter = 0;
            this.port.postMessage({
                type: "state",
                music_beats: this.wasm.music_position_beats(),
                active_voices: this.wasm.active_voice_count(),
            });
        }

        return true;
    }
}

registerProcessor("voxl-mixer", VoxlMixerProcessor);
