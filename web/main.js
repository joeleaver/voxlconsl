// voxlconsl browser shim.
//
// Loads the wasm-bindgen-generated JS, instantiates a BrowserHost, and runs
// a requestAnimationFrame loop that copies the host's framebuffer into a
// canvas via putImageData. Captures keyboard, mouse, and wheel events and
// forwards them to the host as input.

import init, { BrowserHost } from "./pkg/voxlconsl_host_browser.js";

const FRAME_TIMES = new Array(60).fill(16.6);
let frameCursor = 0;

// Browser-port key map. Must agree with `voxlconsl_host::input::Key`.
const KEY_IDS = {
    "KeyW": 0, "KeyA": 1, "KeyS": 2, "KeyD": 3,
    "KeyI": 4, "KeyJ": 5, "KeyK": 6, "KeyL": 7,
    "KeyQ": 8, "KeyE": 9,
    "Space": 10, "Enter": 11, "Tab": 12, "Escape": 13,
    "ShiftLeft": 14, "ShiftRight": 15,
    "ArrowUp": 16, "ArrowDown": 17, "ArrowLeft": 18, "ArrowRight": 19,
    "F1": 20,
};

async function start() {
    const status = document.getElementById("status");
    status.textContent = "loading wasm…";

    let wasm;
    try {
        wasm = await init();
    } catch (err) {
        status.textContent = `failed to load wasm: ${err}`;
        console.error(err);
        return;
    }

    let host;
    try {
        host = new BrowserHost();
    } catch (err) {
        status.textContent = `cart load failed: ${err}`;
        console.error(err);
        return;
    }

    const canvas = document.getElementById("screen");
    const ctx = canvas.getContext("2d", { alpha: false });
    ctx.imageSmoothingEnabled = false;

    const w = host.width();
    const h = host.height();
    const imageData = ctx.createImageData(w, h);

    // ── Audio (§5) — Stage 4b Phase 2c+: mixer-in-worklet ─────────────
    //
    // The authoritative §5 mixer runs inside the AudioWorkletProcessor
    // (web/audio-worklet.js) backed by `voxlconsl-audio-worklet.wasm`.
    // This main-thread shim does three things per cart frame:
    //   1. Drain the cart→audio event log written by sandbox.rs.
    //   2. postMessage each event over to the worklet.
    //   3. Cache state mirrors (music_position_beats / voice_count)
    //      that the worklet posts back periodically.
    //
    // The previous SAB output ring is gone — the worklet outputs
    // directly into the AudioContext, so total latency drops from
    // ~50 ms (Phase 1 jitter buffer) to one worklet block (~5–10 ms).
    const audioSampleRate = host.audio_sample_rate();
    let audioCtx = null;
    let workletNode = null;

    // Event-tag table — mirrors `crates/host/src/audio_events.rs` and
    // the `EVT.*` consts in web/audio-worklet.js. Keep all three in
    // lockstep when adding new tags.
    const EVT_NOTE_ON              = 0;
    const EVT_NOTE_OFF             = 1;
    const EVT_ALL_NOTES_OFF        = 2;
    const EVT_PITCH_BEND           = 3;
    const EVT_CC                   = 4;
    const EVT_PROGRAM_CHANGE       = 5;
    const EVT_PATCH_SET_OSC        = 6;
    const EVT_PATCH_SET_FILTER     = 7;
    const EVT_PATCH_SET_AMP_ENV    = 8;
    const EVT_PATCH_SET_FILTER_ENV = 9;
    const EVT_PATCH_SET_LFO        = 10;
    const EVT_PATCH_SET_GLIDE      = 11;
    const EVT_PATCH_SET_FM         = 12;
    const EVT_PATCH_SET_KIND       = 13;
    const EVT_PATCH_SET_ZONE       = 14;
    const EVT_PATCH_SET_ZONE_COUNT = 15;
    const EVT_PATCH_RESET          = 16;
    const EVT_PATCH_COPY           = 17;
    const EVT_MUSIC_PLAY           = 18;
    const EVT_MUSIC_STOP           = 19;
    const EVT_MUSIC_SET_TEMPO_SC   = 20;
    const EVT_REVERB_SET           = 21;
    const EVT_DELAY_SET            = 22;
    const EVT_SAMPLE_LOAD          = 23;
    const EVT_MUSIC_LOAD           = 24;
    const EVT_SFX_PLAY             = 25;
    const EVT_SFX_STOP             = 26;
    const EVT_SFX_SET_VOLUME       = 27;
    const EVT_SFX_SET_PITCH        = 28;
    const EVT_VOICE_TRIGGER        = 29;
    const EVT_VOICE_RELEASE        = 30;

    function drainAudioEventsAndPost() {
        if (!workletNode) return;
        const len = host.audio_events_len();
        if (len === 0) return;
        const ptr = host.audio_events_ptr();
        // Snapshot the bytes — we'll iterate over them and the wasm
        // memory may grow on the next allocation.
        const view = new Uint8Array(wasm.memory.buffer, ptr, len);
        const bytes = view.slice();
        host.audio_events_clear();

        const dv = new DataView(bytes.buffer);
        let p = 0;
        while (p < bytes.length) {
            const tag = bytes[p]; p++;
            switch (tag) {
                case EVT_NOTE_ON: {
                    const token = dv.getUint32(p, true); p += 4;
                    const channel = bytes[p++];
                    const note = bytes[p++];
                    const velocity = bytes[p++];
                    workletNode.port.postMessage({
                        type: "event", tag, token,
                        channel, note, velocity,
                    });
                    break;
                }
                case EVT_NOTE_OFF: {
                    const channel = bytes[p++];
                    const note = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, channel, note });
                    break;
                }
                case EVT_ALL_NOTES_OFF: {
                    const channel = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, channel });
                    break;
                }
                case EVT_PITCH_BEND: {
                    const channel = bytes[p++];
                    const value = dv.getInt16(p, true); p += 2;
                    workletNode.port.postMessage({ type: "event", tag, channel, value });
                    break;
                }
                case EVT_CC: {
                    const channel = bytes[p++];
                    const controller = bytes[p++];
                    const value = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, channel, controller, value });
                    break;
                }
                case EVT_PROGRAM_CHANGE: {
                    const channel = bytes[p++];
                    const patch = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, channel, patch });
                    break;
                }
                case EVT_PATCH_SET_OSC: {
                    const slot = bytes[p++];
                    const osc_idx = bytes[p++];
                    const mode = bytes[p++];
                    const detune_cents = dv.getInt16(p, true); p += 2;
                    const octave = dv.getInt8(p); p += 1;
                    const level = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, slot, osc_idx, mode, detune_cents, octave, level });
                    break;
                }
                case EVT_PATCH_SET_FILTER: {
                    const slot = bytes[p++];
                    const mode = bytes[p++];
                    const cutoff_hz = dv.getUint16(p, true); p += 2;
                    const resonance = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, slot, mode, cutoff_hz, resonance });
                    break;
                }
                case EVT_PATCH_SET_AMP_ENV: {
                    const slot = bytes[p++];
                    const attack_ms = dv.getUint16(p, true); p += 2;
                    const decay_ms = dv.getUint16(p, true); p += 2;
                    const sustain = bytes[p++];
                    const release_ms = dv.getUint16(p, true); p += 2;
                    workletNode.port.postMessage({ type: "event", tag, slot, attack_ms, decay_ms, sustain, release_ms });
                    break;
                }
                case EVT_PATCH_SET_FILTER_ENV: {
                    const slot = bytes[p++];
                    const attack_ms = dv.getUint16(p, true); p += 2;
                    const decay_ms = dv.getUint16(p, true); p += 2;
                    const sustain = bytes[p++];
                    const release_ms = dv.getUint16(p, true); p += 2;
                    const depth = dv.getInt8(p); p += 1;
                    workletNode.port.postMessage({ type: "event", tag, slot, attack_ms, decay_ms, sustain, release_ms, depth });
                    break;
                }
                case EVT_PATCH_SET_LFO: {
                    const slot = bytes[p++];
                    const rate_centihz = dv.getUint16(p, true); p += 2;
                    const shape = bytes[p++];
                    const target = bytes[p++];
                    const depth = dv.getInt8(p); p += 1;
                    workletNode.port.postMessage({ type: "event", tag, slot, rate_centihz, shape, target, depth });
                    break;
                }
                case EVT_PATCH_SET_GLIDE: {
                    const slot = bytes[p++];
                    const ms = dv.getUint16(p, true); p += 2;
                    workletNode.port.postMessage({ type: "event", tag, slot, ms });
                    break;
                }
                case EVT_PATCH_SET_FM: {
                    const slot = bytes[p++];
                    const ratio_q88 = dv.getUint16(p, true); p += 2;
                    const index_q88 = dv.getUint16(p, true); p += 2;
                    workletNode.port.postMessage({ type: "event", tag, slot, ratio_q88, index_q88 });
                    break;
                }
                case EVT_PATCH_SET_KIND: {
                    const slot = bytes[p++];
                    const kind = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, slot, kind });
                    break;
                }
                case EVT_PATCH_SET_ZONE: {
                    const slot = bytes[p++];
                    const zone_idx = bytes[p++];
                    const low_note = bytes[p++];
                    const high_note = bytes[p++];
                    const root_note = bytes[p++];
                    const sample_slot = bytes[p++];
                    const volume_offset = dv.getInt8(p); p += 1;
                    const loop_start = dv.getUint32(p, true); p += 4;
                    const loop_end = dv.getUint32(p, true); p += 4;
                    const loop_enabled = bytes[p++];
                    workletNode.port.postMessage({
                        type: "event", tag,
                        slot, zone_idx,
                        low_note, high_note, root_note,
                        sample_slot, volume_offset,
                        loop_start, loop_end, loop_enabled,
                    });
                    break;
                }
                case EVT_PATCH_SET_ZONE_COUNT: {
                    const slot = bytes[p++];
                    const count = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, slot, count });
                    break;
                }
                case EVT_PATCH_RESET: {
                    const slot = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, slot });
                    break;
                }
                case EVT_PATCH_COPY: {
                    const src = bytes[p++];
                    const dst = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, src, dst });
                    break;
                }
                case EVT_MUSIC_PLAY: {
                    const slot = bytes[p++];
                    const loop = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, slot, loop });
                    break;
                }
                case EVT_MUSIC_STOP: {
                    workletNode.port.postMessage({ type: "event", tag });
                    break;
                }
                case EVT_MUSIC_SET_TEMPO_SC: {
                    const scale = dv.getFloat32(p, true); p += 4;
                    workletNode.port.postMessage({ type: "event", tag, scale });
                    break;
                }
                case EVT_REVERB_SET: {
                    const room_size = bytes[p++];
                    const damping = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, room_size, damping });
                    break;
                }
                case EVT_DELAY_SET: {
                    const time_ms = dv.getUint16(p, true); p += 2;
                    const feedback = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, time_ms, feedback });
                    break;
                }
                case EVT_SAMPLE_LOAD: {
                    const slot = bytes[p++];
                    const rate_code = bytes[p++];
                    const flags = bytes[p++];
                    const loop_start = dv.getUint32(p, true); p += 4;
                    const loop_end = dv.getUint32(p, true); p += 4;
                    const blob_len = dv.getUint32(p, true); p += 4;
                    const payload = bytes.slice(p, p + blob_len);
                    p += blob_len;
                    workletNode.port.postMessage({
                        type: "event", tag,
                        slot, rate_code, flags,
                        loop_start, loop_end,
                        bytes: payload,
                    }, [payload.buffer]);
                    break;
                }
                case EVT_MUSIC_LOAD: {
                    const slot = bytes[p++];
                    const blob_len = dv.getUint32(p, true); p += 4;
                    const payload = bytes.slice(p, p + blob_len);
                    p += blob_len;
                    workletNode.port.postMessage({
                        type: "event", tag, slot, bytes: payload,
                    }, [payload.buffer]);
                    break;
                }
                case EVT_SFX_PLAY: {
                    const token = dv.getUint32(p, true); p += 4;
                    const slot = bytes[p++];
                    const volume = bytes[p++];
                    const pan = dv.getInt8(p); p += 1;
                    const pitch_cents = dv.getInt16(p, true); p += 2;
                    const loop = bytes[p++];
                    workletNode.port.postMessage({
                        type: "event", tag, token,
                        slot, volume, pan, pitch_cents, loop,
                    });
                    break;
                }
                case EVT_SFX_STOP: {
                    const voice = dv.getUint32(p, true); p += 4;
                    workletNode.port.postMessage({ type: "event", tag, voice });
                    break;
                }
                case EVT_SFX_SET_VOLUME: {
                    const voice = dv.getUint32(p, true); p += 4;
                    const volume = bytes[p++];
                    workletNode.port.postMessage({ type: "event", tag, voice, volume });
                    break;
                }
                case EVT_SFX_SET_PITCH: {
                    const voice = dv.getUint32(p, true); p += 4;
                    const pitch_cents = dv.getInt16(p, true); p += 2;
                    workletNode.port.postMessage({ type: "event", tag, voice, pitch_cents });
                    break;
                }
                case EVT_VOICE_TRIGGER: {
                    const token = dv.getUint32(p, true); p += 4;
                    const patch = bytes[p++];
                    const note = bytes[p++];
                    const velocity = bytes[p++];
                    workletNode.port.postMessage({
                        type: "event", tag, token,
                        patch, note, velocity,
                    });
                    break;
                }
                case EVT_VOICE_RELEASE: {
                    const voice = dv.getUint32(p, true); p += 4;
                    workletNode.port.postMessage({ type: "event", tag, voice });
                    break;
                }
                default: {
                    console.warn("unknown audio event tag", tag, "at offset", p - 1);
                    return;
                }
            }
        }
    }

    async function ensureAudioStarted() {
        if (audioCtx) return;
        const Ctx = window.AudioContext || window.webkitAudioContext;
        if (!Ctx) {
            console.warn("Web Audio not available — running silently");
            return;
        }
        // Force the AudioContext to run at the mixer's native rate so
        // the worklet's `process()` block lines up 1:1 with the
        // worklet wasm's render() output. No resampling in either side.
        audioCtx = new Ctx({ latencyHint: "interactive", sampleRate: audioSampleRate });
        try {
            await audioCtx.audioWorklet.addModule("audio-worklet.js?v=worklet5");
        } catch (err) {
            console.error("worklet load failed:", err);
            audioCtx = null;
            return;
        }
        // Fetch the worklet wasm bytes ourselves and hand them in via
        // processorOptions. Inside an AudioWorkletGlobalScope `fetch`
        // is not universally available; passing the bytes is portable.
        let workletWasmBytes;
        try {
            const r = await fetch("audio-worklet.wasm?v=worklet5");
            workletWasmBytes = await r.arrayBuffer();
        } catch (err) {
            console.error("worklet wasm fetch failed:", err);
            return;
        }
        workletNode = new AudioWorkletNode(audioCtx, "voxl-mixer", {
            outputChannelCount: [2],
            processorOptions: { wasmBytes: workletWasmBytes },
        });
        workletNode.port.onmessage = (e) => {
            const m = e.data;
            if (!m) return;
            if (m.type === "state") {
                host.set_audio_music_beats_cached(m.music_beats);
                host.set_audio_voices_active_cached(m.active_voices);
            } else if (m.type === "ready") {
                console.log("[audio-worklet] mixer wasm ready");
            } else if (m.type === "error") {
                console.error("[audio-worklet] error:", m.error);
            }
        };
        workletNode.connect(audioCtx.destination);
        await audioCtx.resume().catch((err) => console.warn("audio resume failed:", err));
    }

    // ── Pointer-lock state ──────────────────────────────────────────────
    //
    // The cart treats mouse motion as `Aim` input. To get clean FPS-style
    // continuous look without the cursor drifting off the canvas, click on
    // the canvas to grab the pointer; press Escape (browser default) to
    // release it. Mouse-delta forwarding is gated on the lock state so
    // the camera doesn't drift while the user is just hovering.
    let pointerLocked = false;
    canvas.addEventListener("click", () => {
        ensureAudioStarted();
        if (!pointerLocked) {
            // Modern browsers return a Promise; older ones don't. Either
            // way, the `pointerlockchange` listener below handles success.
            const r = canvas.requestPointerLock();
            if (r && typeof r.catch === "function") {
                r.catch((err) => console.warn("pointer lock failed:", err));
            }
        }
    });
    document.addEventListener("pointerlockchange", () => {
        pointerLocked = document.pointerLockElement === canvas;
        canvas.classList.toggle("locked", pointerLocked);
    });

    // ── Input wiring ─────────────────────────────────────────────────
    // Keyboard events: capture press/release for keys we care about.
    window.addEventListener("keydown", (e) => {
        const id = KEY_IDS[e.code];
        if (id !== undefined) {
            host.set_key(id, true);
            // Prevent browser-default actions (Tab moving focus, Space
            // scrolling, etc.) when running.
            if (["Space", "Tab", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(e.code)) {
                e.preventDefault();
            }
        }
    });
    window.addEventListener("keyup", (e) => {
        const id = KEY_IDS[e.code];
        if (id !== undefined) host.set_key(id, false);
    });

    // Mouse delta: only accumulate when the canvas has pointer lock.
    // movementX/Y is the per-event delta; suppressing it when unlocked
    // keeps the cart's camera still while the user is just hovering or
    // reading the page.
    canvas.addEventListener("mousemove", (e) => {
        if (pointerLocked) {
            host.add_mouse_delta(e.movementX, e.movementY);
        }
    });

    // Mouse wheel → host's wheel-delta channel. We only forward when
    // locked so the page can still scroll normally above/below the
    // canvas. Normalizing by 100 turns one wheel notch into ≈ ±1.0
    // (matching the `BindingHint::Zoom` convention: positive = zoom in).
    canvas.addEventListener("wheel", (e) => {
        if (!pointerLocked) return;
        e.preventDefault();
        host.add_wheel_delta(-e.deltaY / 100);
    }, { passive: false });

    status.textContent = `running ${w}×${h}`;

    // Debug exposures for headless E2E testing. Pure conveniences for
    // tests/devtools — the runtime itself never reads them, so they're
    // safe to leave in place even in release builds.
    window.__host = host;
    window.__wasm = wasm;
    // Stage-4b Phase 2c+: the SAB output ring is gone — the mixer
    // lives in the worklet wasm and feeds the AudioContext directly.
    // Expose the worklet node so E2E tests can poke its port if needed.
    window.__audio = { get workletNode() { return workletNode; } };

    let lastTime = performance.now();

    function frame(now) {
        const dt = now - lastTime;
        lastTime = now;
        FRAME_TIMES[frameCursor] = dt;
        frameCursor = (frameCursor + 1) % FRAME_TIMES.length;

        const ptr = host.frame(dt);
        const len = host.framebuffer_len();
        const memory = new Uint8ClampedArray(wasm.memory.buffer, ptr, len);
        imageData.data.set(memory);
        ctx.putImageData(imageData, 0, 0);

        drainAudioEventsAndPost();

        if (frameCursor === 0) {
            const avg = FRAME_TIMES.reduce((a, b) => a + b, 0) / FRAME_TIMES.length;
            const fps = 1000 / avg;
            const hint = pointerLocked ? "Esc to release" : "click canvas to play";
            status.textContent = `running ${w}×${h} · ${fps.toFixed(1)} fps · ${hint}`;
        }

        requestAnimationFrame(frame);
    }
    requestAnimationFrame(frame);
}

start();
