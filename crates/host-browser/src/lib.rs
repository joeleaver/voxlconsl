//! Browser host for voxlconsl — the reference implementation (SPEC.md §9).
//!
//! Loads a `.voxl` cart binary embedded at build time, runs it inside
//! `wasmi`, and renders whatever world state the cart populates each
//! frame. `Cart::load` auto-detects `.voxl` vs raw `.wasm` so this
//! crate doesn't need to care about the format.

use wasm_bindgen::prelude::*;

use voxlconsl_host::audio::SAMPLE_RATE;
use voxlconsl_host::input::Key;
use voxlconsl_host::renderer::{render_frame, Scene, HEIGHT, WIDTH};
use voxlconsl_host::sandbox::Cart;

const FB_BYTES: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

// Stage 4b Phase 2c+ moved the entire mixer into the worklet, so the
// host-browser no longer renders audio chunks for the main thread to
// schedule. The old `AUDIO_CHUNK_BLOCKS` / `audio_l` / `audio_r` /
// `render_audio_chunk` pipeline is gone; main.js drains the
// cart→audio event log instead (`audio_events_ptr` / `_len` /
// `_clear`) and posts events to the AudioWorkletProcessor.

/// Embedded cart `.voxl`, produced by `scripts/build-web.sh` (which runs
/// `voxlconsl bundle` and copies the resulting blob here before this
/// crate is compiled). Used as the fallback when JS calls the no-args
/// constructor; the runtime cart picker passes its own bytes via
/// [`BrowserHost::new_with_cart`].
const EMBEDDED_CART: &[u8] = include_bytes!("../embedded-cart.voxl");

#[wasm_bindgen]
pub struct BrowserHost {
    cart: Cart,
    framebuffer: Vec<u8>,
}

#[wasm_bindgen]
impl BrowserHost {
    /// Boot with the compile-time embedded cart. Kept for tests, the
    /// CLI flow, and any host that doesn't expose a runtime picker.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<BrowserHost, JsValue> {
        Self::new_with_cart(EMBEDDED_CART)
    }

    /// Boot with cart bytes supplied at runtime (the JS picker fetches
    /// a `.voxl` from `/carts/<name>.voxl` and passes the buffer in).
    /// `bytes` may be either a `.voxl` cart binary or raw WASM —
    /// `Cart::load` auto-detects via the `VOXLCONSL\0` magic.
    pub fn new_with_cart(bytes: &[u8]) -> Result<BrowserHost, JsValue> {
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(&format!("voxlconsl panic: {info}").into());
        }));

        // Forward cart `log()` output to the browser console. host-browser
        // is the only place that pulls in web-sys; voxlconsl-host stays
        // web-sys-free so it builds for native + MCU targets.
        voxlconsl_host::sandbox::set_log_callback(|msg| {
            web_sys::console::log_1(&msg.into());
        });

        let cart = Cart::load(bytes)
            .map_err(|e| JsValue::from_str(&format!("cart load failed: {e:?}")))?;

        Ok(Self {
            cart,
            framebuffer: vec![0; FB_BYTES],
        })
    }

    pub fn width(&self) -> u32 { WIDTH }
    pub fn height(&self) -> u32 { HEIGHT }
    pub fn framebuffer_len(&self) -> usize { FB_BYTES }

    /// Audio mixer native sample rate (Hz). Main.js creates the
    /// AudioContext at this rate so the worklet wasm's `render()`
    /// block frame count lines up 1:1 with `process()`.
    pub fn audio_sample_rate(&self) -> u32 { SAMPLE_RATE }

    /// Number of currently-playing voices in the mixer's 16-voice
    /// pool. After Stage-4b Phase 2c+ the authoritative count lives
    /// on the worklet thread; this reads the cached mirror that JS
    /// updates from worklet `state` posts.
    pub fn audio_active_voice_count(&mut self) -> u32 {
        self.cart.world().audio_voices_active_cached
    }

    /// Debug-only: directly push a `voice_trigger` event onto the
    /// audio log so the worklet's mixer fires the voice on its next
    /// `process()` call. Returns the cart-visible VoiceId token.
    pub fn audio_debug_voice_trigger(&mut self, patch: u32, note: u32, velocity: u32) -> u32 {
        let world = self.cart.world();
        let token = world.alloc_voice_token();
        world.audio_events.push_voice_trigger(
            token, patch as u8, note as u8, velocity as u8,
        );
        token
    }

    /// Current SMF playback position in quarter-note beats, or 0 if no
    /// song is active. E2E hook for confirming `music_play` ran.
    /// Reads the cached mirror updated from worklet state posts.
    pub fn audio_music_position_beats(&mut self) -> f32 {
        self.cart.world().audio_music_beats_cached
    }

    /// `true` if a song is currently playing. Heuristic — derived
    /// from the cached `music_position_beats` (any non-zero value
    /// means the worklet's playhead is alive). Good enough for E2E
    /// tests; not authoritative if the song happens to be at tick 0.
    pub fn audio_music_is_playing(&mut self) -> u32 {
        if self.cart.world().audio_music_beats_cached > 0.0 { 1 } else { 0 }
    }

    /// Debug-only: directly enqueue a `music_play` event so the
    /// worklet starts playback on its next `process()` call. Lets
    /// E2E tests verify Stage-4a playback without depending on the
    /// (chronically flaky in headless Playwright) keyboard path.
    pub fn audio_debug_music_play(&mut self, slot: u32, loop_: u32) {
        self.cart.world().audio_events.push_music_play(slot as u8, loop_ != 0);
    }

    /// Pointer to the audio event log written by sandbox.rs's audio
    /// imports. JS reads `audio_events_len()` bytes starting here
    /// and relays each event to the AudioWorkletProcessor via
    /// `port.postMessage`. The view is invalidated when wasm memory
    /// grows; always re-create the Uint8Array view after a drain.
    pub fn audio_events_ptr(&mut self) -> *const u8 {
        self.cart.world().audio_events.buf.as_ptr()
    }

    /// Byte count in the audio event log. Cleared by
    /// `audio_events_clear()` after JS has drained it.
    pub fn audio_events_len(&mut self) -> u32 {
        self.cart.world().audio_events.buf.len() as u32
    }

    /// Reset the audio event log. Call after JS has read its bytes
    /// out to the worklet.
    pub fn audio_events_clear(&mut self) {
        self.cart.world().audio_events.clear();
    }

    /// Update the cached `music_position_beats()` value from the
    /// worklet's authoritative mixer state. JS calls this each time
    /// the worklet posts a state-mirror message.
    pub fn set_audio_music_beats_cached(&mut self, beats: f32) {
        self.cart.world().audio_music_beats_cached = beats;
    }

    pub fn set_audio_voices_active_cached(&mut self, voices: u32) {
        self.cart.world().audio_voices_active_cached = voices;
    }

    pub fn frame(&mut self, dt_ms: f32) -> *const u8 {
        // Cart per-frame lifecycle (§10 per-frame loop).
        if let Err(e) = self.cart.update(dt_ms as u32) {
            web_sys::console::error_1(&format!("cart update: {e:?}").into());
        }
        if let Err(e) = self.cart.render() {
            web_sys::console::error_1(&format!("cart render: {e:?}").into());
        }

        // Roll forward edge-triggered events and held-time counters now
        // that the cart has had a chance to query input this frame.
        self.cart.world().input.end_of_frame(dt_ms as u32);

        // Integrate Layer 2 rigid bodies (§10.2). Must run before the
        // CA tick + final flush so any body-driven mutations the cart
        // makes (in response to events drained during update) settle
        // into a consistent world state before render.
        //
        // Bodies need a flushed world view for voxel-collision queries.
        // The integrator reads `world.read_material` directly (not via
        // SVO), so no flush is required here — `read_material` sees
        // uncommitted writes.
        voxlconsl_host::bodies::step(self.cart.world(), dt_ms / 1000.0);

        // Tick the CA sim (§10.3 layer 3). Must run before flush so the
        // SVO rebuild captures any voxel mutations from sand/water/etc.
        // CA tick reads/writes the dense buffer directly and re-marks
        // dirty chunks for flush.
        voxlconsl_host::ca::tick(self.cart.world());

        // Pull the world state the cart just configured and ray-march it.
        let world = self.cart.world();
        world.flush();
        world.actors.flush_all();
        world.macro_grid.rebuild(&world.actors);

        let scene = Scene {
            chunks: world.chunks_slice(),
            actors: &world.actors,
            macro_grid: &world.macro_grid,
            materials: &world.materials,
            ca: &world.ca,
            sun_dir: world.sun_dir,
            sky_top: world.sky_top,
            sky_horizon: world.sky_horizon,
            viewport: world.viewport,
        };
        render_frame(&scene, &world.camera, &mut self.framebuffer);

        self.framebuffer.as_ptr()
    }

    /// Notify the host of a key state change. `key_id` corresponds to the
    /// numeric IDs in `voxlconsl_host::input::Key`.
    pub fn set_key(&mut self, key_id: u8, down: bool) {
        if let Some(k) = Key::from_u8(key_id) {
            self.cart.world().input.key_event(k, down);
        }
    }

    /// Accumulate mouse motion since the last frame.
    pub fn add_mouse_delta(&mut self, dx: f32, dy: f32) {
        self.cart.world().input.add_mouse_delta(dx, dy);
    }

    /// Accumulate wheel motion since the last frame. Pass
    /// `-event.deltaY / 100` so one notch is ≈ ±1.0 with positive =
    /// scroll-up = zoom-in (`BindingHint::Zoom` convention).
    pub fn add_wheel_delta(&mut self, dy: f32) {
        self.cart.world().input.add_wheel_delta(dy);
    }
}
