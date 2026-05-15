# big-world

voxlconsl's renderer stress test + multi-feature showcase. Builds a
512×512 procedural voxel terrain with ~500 trees, drops the player in
at the centre, and exercises most of the engine's runtime systems
simultaneously: physics raycast, rigid bodies, cellular automata,
ember-based fire spread, sequenced music, and the cart-side text
renderer.

## What it does

**Title screen.** Boots into a clean void with the floating
`voxlconsl` title rendered as a chiseled 3D voxel slab via
`paint_world`, gently swayed by an orbiting camera. Subtitle
`PRESS FIRE` below.

**Gameplay.** Pressing FIRE drops you into a 512×512 terrain with a
forest, a sand pile, a water source, a stack of dynamic crates + two
leaf-coloured balls, and a tree that's already on fire — the burn
spreads through the canopy via cart-side airborne embers. SPACE plays
a sustained synth lead; K hits a kick drum; the bundled SMF song
loops in the background.

## Controls

| Key | Action |
|---|---|
| WASD | Move (relative to camera yaw, terrain-tracking) |
| Mouse | Look (orbit camera) |
| Wheel | Zoom |
| J / FIRE | Title → Gameplay transition |
| Space | Sustained synth note on channel 0 |
| K | Kick drum (drum channel) |

## What it teaches you

| Topic | Where to look |
|---|---|
| Procedural terrain — multi-octave value noise → heightmap → voxels | `terrain.rs` |
| Deterministic tree scatter | `terrain::scatter_trees` |
| Two-scene cart with floating 3D text title | `lib.rs::paint_title_scene` |
| Flipbook walk-cycle character | `player.rs` |
| §10.1 physics raycast (targeting reticle) | `lib.rs::update_reticle` |
| §10.2 rigid bodies (AABB crates + sphere balls) | `body_demo.rs` |
| §10.3 cellular automata (sand, water, fire) | `lib.rs::drop_sand_and_water` |
| Cart-side fire spread (airborne ember system) | `embers.rs` |
| §5 audio — channel routing + boot-loaded SMF + SFX triggers | `audio.rs` + `lib.rs::handle_audio_triggers` |
| §11.10 text rendering with FONT_DCP1 + FONT_ANSI | `lib.rs::paint_title_scene` |
| Materials via `materials.toml` (bundler pipeline) | `materials.toml` |

## File map

```
big-world/
├── Cargo.toml
├── cart.toml          — bundler manifest (materials + audio paths)
├── materials.toml     — 15 material slots
├── audio/
│   ├── patches.toml   — 3 synth/sampler patches
│   ├── samples/       — WAV samples
│   ├── songs/         — SMF song(s)
│   └── build_assets.py — regenerates beep.wav + groove.mid from source
└── src/
    ├── lib.rs         — Entry points, scenes, game state, camera, update orchestration
    ├── terrain.rs     — Value noise → heightmap → voxels + tree scatter
    ├── player.rs      — Prefab frames, walk-cycle flipbook, movement
    ├── embers.rs      — Burn sites + airborne embers
    ├── body_demo.rs   — §10.2 crate stack + leaf-ball spawning
    ├── audio.rs       — Channel routing + FX bus setup
    └── mathlib.rs     — no_std sine / cosine / atan2
```

## Building

```sh
./scripts/build-web.sh release big-world
```

Then open `web/index.html` via `python3 scripts/dev-server.py 8765`
and pick `big-world` from the cart library.

## Out-of-spec note

The 512×512 terrain populates ~256–512 chunks resident at ~50 KB
each (~12–25 MB), which fits the spec's ESP32-P4 design point but is
intentionally out-of-spec for smaller MCUs. This cart exists primarily
to flex the renderer + the major subsystems together; real carts
should be more modest.
