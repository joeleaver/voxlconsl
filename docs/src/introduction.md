# voxlconsl

> A fantasy console where the only graphics primitive is a voxel.

<div class="vx-hero">
  <iframe class="vx-emulator-embed" src="emulator/" loading="lazy"
          sandbox="allow-scripts allow-same-origin allow-pointer-lock"
          title="voxlconsl live emulator"></iframe>
  <p class="vx-hero-caption">
    Click the canvas, then drive with <kbd>W</kbd><kbd>A</kbd><kbd>S</kbd><kbd>D</kbd> and the mouse —
    or open the <a href="emulator/" target="_blank">full-screen emulator</a>.
  </p>
</div>

## What is voxlconsl?

voxlconsl is a fantasy console — a virtual machine with deliberately small,
deliberately specific constraints — where every visible thing is made of
voxels. There are no sprites, no triangles, no textures. There is one
primitive, and the platform's identity is what falls out of using it
exclusively.

The constraints aren't there to be quaint. They're there because constraint
shapes a medium: PICO-8 looks like PICO-8 because of its 16 colors and 128×128
screen, the Game Boy looks like the Game Boy because of its green-tinted
4-shade palette. voxlconsl is staking out the same territory in 3D.

## What's special about it

| | |
|---|---|
| **World** | A single 1024×1024×1024 voxel grid |
| **Output** | 256×144 framebuffer, 60 Hz, ray-marched on the CPU |
| **Color** | 64-color fixed system palette, 16 ramps × 4 shades, lighting via shade-shift |
| **Audio** | Cart-defined synth + sampler patches driven by MIDI, plus runtime patch editing |
| **Input** | Action-based — same cart runs unchanged on browser, touch-only mobile, and physical handheld |
| **Cart code** | WebAssembly. Rust is the reference cart language; any language with a WASM target works |
| **Cart size** | 32 MB |
| **Hardware target** | ESP32-P4 tier eventually; the browser is the conformance reference |

## Three things that make voxlconsl distinctive

1. **GPU-less ray-marched voxels.** Every port — browser, future ESP32-P4
   handheld — uses the same CPU SVO ray marcher. Identical pixels everywhere.
2. **Cellular automata as a first-class platform feature.** Sand, water, fire,
   gas, and flammable materials are tagged in the material table; the host
   runs them. (See [§10.3 of the spec](spec.md#103-layer-3--cellular-automata).)
3. **Action-based input.** Carts declare gameplay verbs ("move", "fire",
   "menu") and the port maps physical inputs to them. Same cart, three
   completely different input topologies.

## What state is the project in?

**Pre-alpha.** The [specification](spec.md) is mostly locked at v0.1 — most
load-bearing decisions are made and written down. Implementation just
started; the live emulator above is what runs today:

- Browser host built on `wasmi` for cart sandboxing (running inside the
  host's own WASM, three nested layers cleanly).
- Sparse voxel octree per the spec, ray-marching the test scene at
  60 Hz with actor compositing — world voxels and actor volumes
  participate in the same depth comparison.
- Full actor system: lifecycle, transforms, voxel editing, prefabs +
  copy-on-write, all 24 cube-symmetry orientations, and a flipbook
  animation helper.
- Action-based input working end-to-end (browser → host → cart and back).
- The `hello-cube` example cart exercises the SDK end-to-end — a
  controllable, walk-cycle-animated character plus three barrel
  actors at different orientations.

Audio, physics queries, multi-chunk worlds, and a real `.voxl`
cart-format parser are next. See the [roadmap](roadmap.md) for what's
planned and what's already locked.

## Where to go from here

- **[Try it live](emulator.md)** — open the embedded emulator full-screen.
- **[Quick start](quick-start.md)** — clone, build, run locally.
- **[The spec](spec.md)** — the full v0.1 specification.
- **[Hello cube walkthrough](hello-cube.md)** — author your first cart.
- **[GitHub](https://github.com/joeleaver/voxlconsl)** — source code, issues, discussions.
