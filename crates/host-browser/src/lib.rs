//! Browser host for voxlconsl — the reference implementation (SPEC.md §9).
//!
//! v0.0.3: loads a cart `.wasm` (compiled separately and embedded via
//! `include_bytes!`), runs it inside `wasmi`, and renders whatever world
//! state the cart populates each frame. The "test scene" is now a real
//! cart, not host-side hardcoded geometry.

use wasm_bindgen::prelude::*;

use voxlconsl_host::input::Key;
use voxlconsl_host::renderer::{render_frame, Scene, HEIGHT, WIDTH};
use voxlconsl_host::sandbox::Cart;

const FB_BYTES: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

/// The hello-cube cart, built by scripts/build-web.sh and copied to a stable
/// path before this crate is compiled.
const EMBEDDED_CART: &[u8] = include_bytes!("../embedded-cart.wasm");

#[wasm_bindgen]
pub struct BrowserHost {
    cart: Cart,
    framebuffer: Vec<u8>,
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
        })
    }

    pub fn width(&self) -> u32 { WIDTH }
    pub fn height(&self) -> u32 { HEIGHT }
    pub fn framebuffer_len(&self) -> usize { FB_BYTES }

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
}
