# Try It Live

The latest build of the browser host runs the bundled `hello-cube` cart at
[/voxlconsl/emulator/](emulator/).

<div class="vx-hero">
  <iframe class="vx-emulator-embed" src="emulator/" loading="lazy"
          sandbox="allow-scripts allow-same-origin allow-pointer-lock"
          title="voxlconsl live emulator"></iframe>
</div>

## Controls

The cart declares three input *actions* (`move`, `aim`, `fire`); the
browser port binds them to the keys and pointer below per the
[default binding rules](spec.md#66-port-binding).

| Input | Cart action | Effect in `hello-cube` |
|---|---|---|
| <kbd>W</kbd><kbd>A</kbd><kbd>S</kbd><kbd>D</kbd> | `move` (PrimaryMovement, Axis2D) | Walks the little dude relative to the camera direction |
| Mouse motion | `aim` (Aim, Axis2D) | Orbits the third-person camera around the dude (yaw + pitch) |
| <kbd>J</kbd> | `fire` (PrimaryFire, Button) | Cycles the dude's shirt color through the palette ramps |

The cart never asks for "WASD" or "mouse" by name — it asks for actions
with hints like `PrimaryMovement` and `Aim`, and the port owns the
mapping. On a controller-only device the same cart would respond to a
left stick + right stick + face button without any cart change.

## What you're looking at

Every pixel is the result of:

1. The browser host samples each frame's input bindings and updates the
   cart's action state (axis values, edge flags).
2. Cart `update(dt_ms)` reads `move` / `aim` / `fire`, integrates the
   player position, picks the current walk-cycle prefab (via
   [`Flipbook`](sdk.md#animation)), and calls `actor_set_position` /
   `actor_set_yaw` / `actor_set_prefab` on the player actor.
3. Cart `render()` orbits the camera around the dude with
   `camera_set_lookat(eye, target, up)`.
4. The host CPU ray-marcher casts one ray per pixel through both the
   world chunk *and* every visible actor's volume (the player + three
   tipped barrels), keeping the closest hit per ray. Actors and world
   participate in the same depth comparison.
5. Each hit is mapped material → ramp+shade → a color from the v0.1
   system palette.
6. The 256×144 RGBA framebuffer is `putImageData`'d to a canvas at
   60 Hz with `image-rendering: pixelated` for a clean 4× upscale.

The three barrels along the world's western edge are the same prefab
spawned at three different orientations (`Up`, `EastUp`, `NorthUp`) —
the host bakes one volume per unique `(prefab, orientation)` pair and
shares it across instances via copy-on-write.

The cart is a small WebAssembly module compiled from
[hello-cube/src/lib.rs](https://github.com/joeleaver/voxlconsl/blob/main/examples/hello-cube/src/lib.rs).
The host runs it inside [`wasmi`](https://crates.io/crates/wasmi) — itself
running in the browser's WASM engine.
