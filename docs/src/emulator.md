# Try It Live

The latest build of the browser host runs the bundled `hello-cube` cart at
[/voxlconsl/emulator/](emulator/).

<div class="vx-hero">
  <iframe class="vx-emulator-embed" src="emulator/" loading="lazy"
          sandbox="allow-scripts allow-same-origin allow-pointer-lock"
          title="voxlconsl live emulator"></iframe>
</div>

## Controls

| Input | Action |
|---|---|
| <kbd>W</kbd><kbd>A</kbd><kbd>S</kbd><kbd>D</kbd> | Orbit camera around the scene; <kbd>W</kbd>/<kbd>S</kbd> dollies in/out |
| Mouse motion (over the canvas) | Aim — orbit yaw + pitch |
| <kbd>J</kbd> | Primary fire — cycles the ruby's shade |
| <kbd>K</kbd> | Secondary fire |
| <kbd>Enter</kbd> / <kbd>Esc</kbd> | Confirm / cancel |
| <kbd>Tab</kbd> | Pause |

These are the cart's chosen *actions* (`move`, `aim`, `fire`) bound by the
browser port to the keys above per the
[default binding rules](spec.md#66-port-binding).

## What you're looking at

Every pixel is the result of:

1. Cart `update(dt_ms)` reading input actions to advance camera state.
2. Cart `render()` calling `camera_set_lookat(...)` with new eye/target.
3. Host CPU ray-marcher casting one ray per pixel through the SVO chunk
   the cart populated during `init()`.
4. Each ray hit is mapped to a material → ramp+shade → a color from the
   v0.1 system palette.
5. The resulting 256×144 RGBA framebuffer is `putImageData`'d to a canvas
   at 60 Hz, with `image-rendering: pixelated` for a clean 4× upscale.

The cart is a 3.5 KB WebAssembly module compiled from
[hello-cube/src/lib.rs](https://github.com/joeleaver/voxlconsl/blob/main/examples/hello-cube/src/lib.rs).
The host runs it inside [`wasmi`](https://crates.io/crates/wasmi) — itself
running in the browser's WASM engine.
