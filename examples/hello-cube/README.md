# hello-cube

The voxlconsl SDK reference cart — and the recommended starting point
for anyone learning the engine. Smallest cart that touches a useful
slice of the SDK; everything lives in one file.

## What it does

- Chequered 64×64 ground spanning four chunks.
- Two trees with leaf canopies + ruby caps, gold cubes scattered around.
- Three barrels at different orientations (Up / EastUp / NorthUp)
  showing the 24-orientation prefab bake.
- A controllable little dude that walks with a 4-frame flipbook
  animation and faces the direction of motion.
- A second "dungeon" scene reachable via FIRE — same materials, same
  prefabs, same player actor, completely different voxel grid.

## Controls

| Key | Action |
|---|---|
| WASD | Move (relative to camera yaw) |
| Mouse | Look (click canvas to engage pointer lock, Esc to release) |
| J / FIRE | Toggle between overworld and dungeon scenes |

## What it teaches you

| Topic | Where to look |
|---|---|
| Materials defined in code (vs `materials.toml`) | `setup_materials()` |
| World building with `set_voxel` / `fill_box` | `paint_overworld()`, `paint_dungeon()` |
| Multi-scene carts (§3.7) | `paint_dungeon()` + FIRE handler in `update()` |
| Prefabs + CoW + orientations (§11.3-§11.5) | `spawn_barrels()` |
| Flipbook animation (§11.9) | `WALK_FB` + the `walk_fb.tick(...)` block in `update()` |
| Camera-relative movement | The `forward`/`right` vectors in `update()` |
| `no_std` + `no_alloc` cart pattern | Top-level `static mut` buffers + the tiny math helpers at the bottom |

## File map

```
hello-cube/
├── Cargo.toml      — wasm32 cdylib + SDK dep
├── cart.toml       — bundler manifest
└── src/lib.rs      — the entire cart, ~470 lines
```

## Building

From the repo root:

```sh
./scripts/build-web.sh release hello-cube
```

Then open `web/index.html` via `python3 scripts/dev-server.py 8765`
and pick `hello-cube` from the cart library.
