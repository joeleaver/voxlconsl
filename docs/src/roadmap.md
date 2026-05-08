# Roadmap

The [specification](spec.md) is mostly locked at v0.1. Implementation
is just starting. This page tracks what's done, what's in progress, and
what's deferred.

## Implemented (v0.0.3)

- **Core types** — full set of vectors, materials (16-byte struct with
  flags + CA tuning), action types, projection, orientation, physics
  primitives, audio enums.
- **Palette** — full v0.1 system palette wired into the renderer with
  the spec-locked shade-shift mechanism.
- **Sparse voxel octree** — per [§13](spec.md#13-sparse-voxel-octree-svo-format).
  `from_dense` build path, front-to-back DDA raycast, unit tested.
- **Renderer** — pinhole camera, per-pixel ray march, sun + ambient
  lighting, sky gradient, emission. 60 fps at 256×144 in browser WASM.
- **Cart sandbox** — `wasmi` loader, host-import table, per-frame
  lifecycle (`init` once, `update`/`render` each frame).
- **Cart-side SDK** — `no_std` Rust crate with safe wrappers over the
  host imports, panic handler, hello-cube example.
- **Input** — action-based per [§6](spec.md#6-input). Declaration,
  Button polling (held / pressed / released / held_ms), Axis2D polling,
  default browser-port bindings (WASD, mouse, J/K, Enter/Esc, Tab/F1).
- **Browser port** — wasm-bindgen + canvas blit, key/mouse capture,
  60 fps frame loop with FPS telemetry.
- **Build pipeline** — `scripts/build-web.sh` compiles cart, embeds
  it in the host, builds host WASM, runs wasm-bindgen.
- **Documentation** — this site, GitHub Pages-deployed mdBook.
- **Animation** ([§11.9](spec.md#119-animation)) — flipbook helper
  (`voxlconsl_sdk::animation::Flipbook`) and `actor_set_prefab` wired
  through the SDK; host stub is a no-op until the actor system lands.

## Up next

1. **Multi-chunk world** ([§13.6](spec.md#136-world-level-chunk-indexing))
   — replace the single 32³ chunk with a `HashMap<ChunkKey, ChunkData>`
   and per-chunk DDA in the renderer. Earns scenes bigger than 32³.
2. **Audio** ([§5](spec.md#5-audio--synth--midi--samples)) — Web Audio API
   + a tiny synth voice. Earns: the platform stops being mute.
3. **Physics queries** ([§10.1](spec.md#101-layer-1--queries)) — wire
   the SVO raycast we already have to host imports as `raycast`,
   `material_at`, `sweep_aabb`, etc. Smallest of the four.
4. **Cart format** ([§7](spec.md#7-cart-format-voxl)) — write the actual
   `.voxl` parser, materials.toml/patches.toml ingestion in the bundler,
   replace `include_bytes!` with a real `Cart::load_from_voxl`.

## After v0.0.x

- **Pointer + system actions** ([§6.3](spec.md#63-reserved-system-actions),
  [§6.4](spec.md#64-polling-api))
- **Touch overlay** ([§6.6](spec.md#66-port-binding))
- **Rigid bodies** ([§10.2](spec.md#102-layer-2--rigid-bodies))
- **Cellular automata** ([§10.3](spec.md#103-layer-3--cellular-automata))
- **Actors** ([§11](spec.md#11-actors)) including 24-orientation bake
  and prefab CoW
- **`voxlconsl new` / `bundle` / `serve`** ([§12.4](spec.md#124-the-voxlconsl-cli))
- **MagicaVoxel `.vox` importer** ([§12.3](spec.md#123-importers))
- **Editor cart** ([§12.7](spec.md#127-editor-cart-roadmap))

## Not planned

- **Soft bodies / structural simulation** ([§10.4](spec.md#104-layer-4--soft-bodies--structural-simulation-out-of-scope)) — out forever.
- **Networking-based multiplayer** — out of v1 scope per [§6](spec.md#6-input).
- **GPU rendering** — the platform's identity is CPU ray marching.
  GPU paths may exist as port-specific optimizations, but the
  reference renderer is CPU-only on every target.
