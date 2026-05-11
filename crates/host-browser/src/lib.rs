//! Browser host for voxlconsl — the reference implementation (SPEC.md §9).
//!
//! Loads a `.voxl` cart binary embedded at build time, runs it inside
//! `wasmi`, and renders whatever world state the cart populates each
//! frame. `Cart::load` auto-detects `.voxl` vs raw `.wasm` so this
//! crate doesn't need to care about the format.

use wasm_bindgen::prelude::*;

use voxlconsl_host::audio::{BLOCK_FRAMES, BLOCK_SAMPLES, SAMPLE_RATE};
use voxlconsl_host::input::Key;
use voxlconsl_host::renderer::{render_frame, Scene, HEIGHT, WIDTH};
use voxlconsl_host::sandbox::Cart;

const FB_BYTES: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

/// Audio blocks rendered per JS-driven chunk pull. 4 blocks × 64
/// frames @ 22.05 kHz ≈ 11.6 ms per chunk. JS keeps ~3 chunks
/// scheduled ahead so output latency lands around 35 ms — fine for
/// SFX, well below the noticeable threshold for game audio.
const AUDIO_CHUNK_BLOCKS: usize = 4;
const AUDIO_CHUNK_FRAMES: usize = AUDIO_CHUNK_BLOCKS * BLOCK_FRAMES;

/// Embedded cart `.voxl`, produced by `scripts/build-web.sh` (which runs
/// `voxlconsl bundle` and copies the resulting blob here before this
/// crate is compiled).
const EMBEDDED_CART: &[u8] = include_bytes!("../embedded-cart.voxl");

#[wasm_bindgen]
pub struct BrowserHost {
    cart: Cart,
    framebuffer: Vec<u8>,
    /// Scratch space for one mixer block, interleaved L/R i16.
    audio_scratch: Vec<i16>,
    /// Deinterleaved + i16→f32 chunk buffers fed to Web Audio.
    /// `AudioBuffer` wants separate channel arrays.
    audio_l: Vec<f32>,
    audio_r: Vec<f32>,
}

#[wasm_bindgen]
impl BrowserHost {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<BrowserHost, JsValue> {
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(&format!("voxlconsl panic: {info}").into());
        }));

        let cart = Cart::load(EMBEDDED_CART)
            .map_err(|e| JsValue::from_str(&format!("cart load failed: {e:?}")))?;

        Ok(Self {
            cart,
            framebuffer: vec![0; FB_BYTES],
            audio_scratch: vec![0; BLOCK_SAMPLES],
            audio_l: vec![0.0; AUDIO_CHUNK_FRAMES],
            audio_r: vec![0.0; AUDIO_CHUNK_FRAMES],
        })
    }

    pub fn width(&self) -> u32 { WIDTH }
    pub fn height(&self) -> u32 { HEIGHT }
    pub fn framebuffer_len(&self) -> usize { FB_BYTES }

    /// Audio mixer sample rate (Hz) for AudioBuffer construction.
    pub fn audio_sample_rate(&self) -> u32 { SAMPLE_RATE }

    /// Frames produced per `render_audio_chunk` call. Both audio
    /// channel buffers are this length.
    pub fn audio_chunk_frames(&self) -> u32 { AUDIO_CHUNK_FRAMES as u32 }

    /// Pointer to the left-channel f32 buffer (length =
    /// `audio_chunk_frames`). Stable across calls *unless* wasm memory
    /// grows — re-read after every `render_audio_chunk` to be safe.
    pub fn audio_l_ptr(&self) -> *const f32 { self.audio_l.as_ptr() }

    /// Pointer to the right-channel f32 buffer.
    pub fn audio_r_ptr(&self) -> *const f32 { self.audio_r.as_ptr() }

    /// Number of currently-playing voices in the mixer's 16-voice
    /// pool. Pure read — useful for diagnostics and headless E2E
    /// tests that want to confirm `sfx_play` landed without inspecting
    /// the audio output stream.
    pub fn audio_active_voice_count(&mut self) -> u32 {
        self.cart.world().audio.active_voice_count() as u32
    }

    /// Pull `AUDIO_CHUNK_BLOCKS` blocks from the mixer, deinterleave
    /// + convert i16 → f32 into `audio_l` / `audio_r`. JS then copies
    /// these into an `AudioBuffer` and schedules it on an
    /// `AudioBufferSourceNode`. Idempotent: each call advances the
    /// mixer by `AUDIO_CHUNK_FRAMES` source frames.
    pub fn render_audio_chunk(&mut self) {
        let mixer = &mut self.cart.world().audio;
        for b in 0..AUDIO_CHUNK_BLOCKS {
            mixer.render_block(&mut self.audio_scratch);
            let dst_off = b * BLOCK_FRAMES;
            for i in 0..BLOCK_FRAMES {
                let l = self.audio_scratch[i * 2] as f32 / 32768.0;
                let r = self.audio_scratch[i * 2 + 1] as f32 / 32768.0;
                self.audio_l[dst_off + i] = l;
                self.audio_r[dst_off + i] = r;
            }
        }
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
            sky: world.sky_top,
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
