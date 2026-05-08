# Concepts

The high-level shape of the platform. For exact definitions, byte
layouts, function signatures, and budgets, read the
[specification](spec.md).

## The world is a 1024³ voxel grid

Integer coordinates, Y-up, origin at the corner. Empty voxels are value
`0`. Non-empty voxels carry an 8-bit material index that selects one of
256 cart-defined material slots.

The world is stored as a sparse octree per [§13](spec.md#13-sparse-voxel-octree-svo-format),
tiled by 32³ chunks at the top level. Most of the world is air; air costs
zero bytes.

## Pixels are produced by ray-marching

There is no GPU. The renderer casts one ray per output pixel through the
SVO, hits a voxel, looks up its material, and resolves a color via the
system palette. The `wasm-bindgen` build of the host runs this loop on
the browser's CPU; future hardware ports will run the same Rust code.

The output framebuffer is `256 × 144` at 60 Hz. That's part of the
console's identity, not a per-port choice — every port renders to a
256×144 framebuffer and scales to its physical display.

## Color is constrained on purpose

There is no per-cart RGB. Instead:

- The platform owns a fixed **64-color system palette** organized as 16
  ramps × 4 shades. The full v0.1 RGB table is in
  [§4.3 of the spec](spec.md#43-rgb-values-v01-draft).
- A material's color field is 6 bits: 4 bits ramp index + 2 bits shade
  index.
- The renderer applies lighting by **shifting the shade index** while
  preserving the ramp:

  ```rust
  let lit = palette[(material.color & 0b1111_1100) | (brightness * 4).clamp(0, 3) as u8];
  ```

This is what makes voxlconsl carts share a visual identity the way
PICO-8 carts do — every cart pulls from the same 64 colors and shades
its voxels through the same mechanism.

## Audio is a built-in synth driven by MIDI

Carts ship up to 16 **patches** — each either a 2-osc subtractive synth
or a sample-playing sampler — and 8 standard MIDI files for music. They
also ship up to 64 PCM samples used by sampler patches and by direct
SFX playback.

Carts can drive MIDI in real time and **mutate any patch parameter at
any time**, which is what makes a synth-editor cart possible: the
platform's instrument designer is itself a regular cart.

## Input is action-based

Carts don't see physical buttons or sticks. They declare
**actions** ("move", "fire", "menu") and the port maps physical inputs
to actions:

| Port | "move" maps to | "aim" maps to |
|---|---|---|
| Browser, no gamepad | WASD | Mouse delta |
| Browser, with gamepad | Left stick | Right stick |
| Touch-only mobile | Virtual stick (auto-generated overlay) | Free-drag right half |
| ESP32-P4 dev board | Left stick | Right stick |

Same cart binary runs on all of these unchanged. See [§6](spec.md#6-input).

## Physics is layered

Four layers, each independently optional, each with its own per-port CPU
budget.

| Layer | Status | Cost |
|---|---|---|
| Queries (raycast, overlap, sweep) | In v1 | < 1% of frame |
| Rigid bodies (AABB / sphere, kinematic / dynamic / static) | In v1 | 5–10% of frame |
| Cellular automata (sand, water, fire, gas, flammable) | In v1, opt-in | up to ~25% of frame |
| Soft bodies / structural sim | **Out — not planned** | — |

Cellular automata are the platform's most distinctive feature beyond
voxels themselves. A material flagged `granular` falls and piles, a
material flagged `liquid` flows and pools, a material flagged
`flammable` next to one flagged `fire` ignites. State lives in a sparse
**active set** so the per-voxel cost on the world grid is zero — only
voxels actually doing something pay anything. See [§10.3](spec.md#103-layer-3--cellular-automata).

## Actors are the unit of "thing that moves"

The world grid is mostly static decoration. Anything that moves —
player, enemies, projectiles, doors, vehicles, particles — is an
**actor**: a small free-floating voxel volume (≤ 32³) with its own
position, yaw, and 1-of-24 fixed orientation. The renderer composites
actors into the same depth comparison as world voxels.

See [§11](spec.md#11-actors) for the full lifecycle, copy-on-write
prefab semantics, and bake triggers.

## Carts are WASM modules

Cart code is `wasm32-unknown-unknown` WebAssembly. The reference cart
language is Rust; any language with a WASM target can be used as long
as it can call the host imports.

The host runs cart WASM under [`wasmi`](https://crates.io/crates/wasmi)
on every port — including the browser, which uses `wasmi` *inside* its
own browser-engine WASM rather than handing carts to the browser's
native engine. Same runtime everywhere = identical behavior =
deterministic replay. See [§9](spec.md#9-browser-host-reference-implementation).
