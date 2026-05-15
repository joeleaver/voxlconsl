# voxlconsl example carts

Three reference carts showing different slices of the voxlconsl SDK,
in roughly increasing complexity. All three deploy to GitHub Pages
together via the cart-library picker.

## Learning path

| Order | Cart | What you'll learn |
|---|---|---|
| 1 | **[hello-cube](./hello-cube/)** | Materials, world building, prefabs + flipbook animation, scenes, camera-relative movement. Smallest cart, single file, everything explicit. |
| 2 | **[big-world](./big-world/)** | Procedural terrain, §10.1 raycast, §10.2 rigid bodies, §10.3 cellular automata, §5 audio (MIDI music + samples + patches), §11.10 text rendering. The "multi-feature showcase". |
| 3 | **[voxdude](./voxdude/)** | A full game: classic-pacman ghost AI, particles, camera-relative HUD, win/lose flow, MIDI music routed through cart-defined chiptune patches. |

Each cart's `README.md` has a feature map pointing into the source.

## Feature matrix

| | hello-cube | big-world | voxdude |
|---|:---:|:---:|:---:|
| World building (`set_voxel` / `fill_box`) | ● | ● | ● |
| Materials in code | ● | | |
| Materials via `materials.toml` | | ● | ● |
| Prefabs + 24 orientations | ● | ● | ● |
| Flipbook animation | ● | ● | ● |
| Multiple scenes | ● | ● | |
| Procedural terrain | | ● | |
| Camera-relative HUD | | | ● |
| §10.1 physics raycast | | ● | |
| §10.2 rigid bodies | | ● | |
| §10.3 cellular automata | | ● | |
| §5 audio — SFX one-shots | | ● | ● |
| §5 audio — MIDI music | | ● | ● |
| §5 audio — cart-defined patches | | ● | ● |
| §5 audio — samples | | ● | |
| §11.10 text rendering | | ● | ● |
| Particles (pooled actors) | | | ● |
| Win/lose game loop | | | ● |

## Building

From the repo root, build all three plus the host wasm:

```sh
./scripts/build-web.sh release
```

Then serve the `web/` directory (it needs SAB-friendly headers — use
the bundled `scripts/dev-server.py` rather than a plain HTTP server):

```sh
python3 scripts/dev-server.py 8765
```

…and open <http://localhost:8765/>. The cart-library picker shows
all three; clicking selects one and reloads.

Building a single cart instead of all three:

```sh
./scripts/build-web.sh release voxdude
```

(Re-run with no args to restore the full picker, since single-cart
builds overwrite the embedded fallback `.voxl`.)

## Cart conventions

Each cart follows the same `no_std` + `no_alloc` pattern (carts compile
to wasm and run inside the host's wasmi sandbox):

- `Cargo.toml` declares `crate-type = ["cdylib"]` and depends only on
  `voxlconsl-sdk`.
- `cart.toml` is the bundler manifest — it points at the build output
  (`[code]`), optional `materials.toml`, and optional `audio/`
  directory.
- `src/lib.rs` exposes three `#[unsafe(no_mangle)] pub extern "C"`
  functions the host calls: `init()` (once at boot), `update(dt_ms)`
  (per frame), and `render()` (just before the renderer draws).
- A `#[panic_handler]` at the bottom of `lib.rs` logs the panic and
  spins; the host treats a panic as fatal.
- State lives in `static mut` (no allocator available). The carts use
  `pub(crate)` visibility to share state across modules — `unsafe`
  is required on every access, by design.
- The `voxlconsl_sdk::*` glob import is the standard prelude; carts
  selectively pull from `voxlconsl_sdk::audio`, `::bodies`,
  `::physics`, `::text`, `::animation` for those subsystems.

The spec at the repo root (`SPEC.md`) is the source of truth for the
console's behaviour — section numbers throughout these carts reference
specific spec sections.
