# voxlconsl — Specification (v0.1 draft)

A fantasy console where the only graphics primitive is a voxel.

> **Status:** working draft. Sections marked **[OPEN]** are unresolved.
> **Targets:** browser (primary, first), ESP32-P4, ESP32-S3, STM32H7. Browser is the reference implementation; hardware ports must match its behavior.

---

## 1. Identity

| | |
|---|---|
| World | Up to 256 **scenes** per cart, each a 512 × 512 × 512 voxel grid (integer coordinates, Y-up); see §3.7 |
| Output framebuffer | **256 × 144** (16:9), scaled to physical display by each port |
| Refresh rate | 60 Hz |
| Color | 64-color fixed system palette + 256-entry per-cart material table |
| Audio | 16 MIDI channels, cart-defined synth patches, 32-voice polyphony, 22.05 kHz mix |
| Input | Cart declares **actions** (gameplay verbs); the port maps physical inputs to actions. Same cart runs on browser, touch-only mobile, and physical handheld. See §6. |
| Cart format | Single binary file, max **32 MB** |
| Cart code | WASM (`wasm32-unknown-unknown`); Rust is the reference cart language |

Render output resolution is part of the console's identity and does not change per port. Physical display size and integer scale are port concerns.

---

## 2. Voxel model

**World grid (per scene):** 512³ logical voxels addressed `(x, y, z)`, origin at the corner, Y up. Empty voxels are value `0`. A cart may address up to 256 scenes (§3.7); 512³ is the *per-scene* ceiling, not the per-cart ceiling.

**Why 512³ and not larger.** 512³ is the largest scene the priority-1 hardware target (ESP32-P4, 32 MB PSRAM) can hold densely populated within the §13.8 resident budget while leaving headroom for code, audio, actor volumes, and per-frame working set. Carts targeting smaller MCUs (ESP32-S3, STM32H7) author at lower densities — see §13.8 for per-target footprint guidance.

**Voxel value:** 8-bit index into the cart's **material table** (256 entries). Index `0` is reserved for "empty / air."

**Material table** (per cart, 256 entries, 16 bytes per entry → 4 KB total, matching §7):

| Field | Bytes | Notes |
|---|---|---|
| `color` | 1 | bits 0–5: `(ramp << 2) \| shade`; bits 6–7 reserved |
| `emission` | 1 | bits 0–3: emission level 0–15 (0 = unlit, 15 = full); bits 4–7 reserved |
| `flags` | 2 | `u16` little-endian bitfield, see flag table below |
| `ca_threshold` | 1 | per-material CA tuning (e.g., flammable ignition heat); 0 = use platform default |
| `ca_lifetime` | 1 | per-material CA tuning (e.g., fire burn frames); 0 = use platform default |
| `ca_viscosity` | 1 | per-material CA tuning (e.g., granular angle of repose, liquid flow rate); 0 = use platform default |
| reserved | 9 | future use; must be 0 in v1 |

**Flags bitfield** (`u16`):

| Bit | Flag | Effect |
|---|---|---|
| 0 | `transparent` | rendered with alpha; light passes through (affects shadow rays and sky/fog blending) |
| 1 | `glossy` | specular highlight from the sun direction |
| 2 | `granular` | CA: falls + piles to angle of repose (§10.3) |
| 3 | `liquid` | CA: flows down then sideways; partial-fill rendering |
| 4 | `gas` | CA: rises, disperses, decays |
| 5 | `flammable` | CA: accumulates heat from adjacent fire; ignites at threshold |
| 6 | `fire` | CA: spreads to flammable neighbors; finite lifetime |
| 7–15 | reserved | must be 0 in v1 |

Material `0` is always "air" regardless of its struct contents — carts may write to slot 0, but the runtime ignores it.

**Storage in cart:** sparse voxel octree (SVO), compressed. Authors don't author octrees directly — the cart toolchain takes a `.vxv` source (§12.2) and produces the SVO blobs. Full SVO format is specified in §13.

**Live storage in memory:** sparse map of 32³ chunks (32,768 chunks max). Empty chunks are zero-cost. Active chunks decompress into RAM on demand; LRU eviction back to flash. Working budget: ~16–20 MB of resident voxel data. Per-chunk SVO details in §13.

---

## 3. Rendering

### 3.1 Algorithm

GPU-less ray marching through the SVO, one ray per output pixel. Same algorithm on every target — no host-specific renderer. Each ray traverses the world grid and is also tested against actor volumes (§11); actors and world voxels participate in the same depth comparison, so the closest hit wins regardless of source.

### 3.2 Camera

The cart owns the camera: it sets camera state each frame, the host stores it and uses it for the next ray-march. All camera setters are cheap and per-frame mutable.

```rust
pub enum Projection {
    Perspective  { fov_y_deg: f32 },        // clamped to 30°..120°
    Orthographic { height: f32 },           // viewport height in world voxels
    Isometric    { scale: f32 },            // pixels per voxel; fixed 30°/45° angles
}

// Orientation — pick one each frame
fn camera_set_lookat(eye: Vec3, target: Vec3, up: Vec3);
fn camera_set_euler(eye: Vec3, yaw: f32, pitch: f32, roll: f32);

// Projection
fn camera_set_projection(p: Projection);

// Range and atmosphere
fn camera_set_view_distance(voxels: f32);                       // default 256, max ~1774
fn camera_set_fog(palette_idx: u8, start: f32, end: f32);       // palette_idx = 255 disables

// Output viewport
fn camera_set_render_rect(x: u16, y: u16, w: u16, h: u16);      // default (0,0,256,144)
```

`camera_set_lookat` and `camera_set_euler` are mutually exclusive — calling one supersedes the other. The host stores whichever was most recently set.

`Projection::Perspective.fov_y_deg` is the *vertical* FOV. Aspect ratio is fixed by the framebuffer (16:9 at 256×144).

`camera_set_view_distance` directly bounds renderer cost. Pixels outside the rect set by `camera_set_render_rect` are cleared to black; this is how cutscene letterboxing and damage-vignette effects are implemented (see §3.5).

**Camera helpers** ship as a separate Rust crate (`voxlconsl-camera-helpers`) the cart imports if it wants them — `FollowCamera`, `OrbitCamera`, `FirstPersonCam`, `FixedCamera`, `DollyCamera`. They are *not* host functions; they are cart-side convenience that compiles into the WASM, so the platform's host ABI doesn't grow with them and carts can fork the helper code if their feel is different.

### 3.3 Lighting

```rust
fn light_set_sun(direction: Vec3, color: u8 /* palette idx */, intensity: u8);
fn light_set_ambient(color: u8, intensity: u8);
```

**v1 model:** ambient + 1 directional light + per-material emission. Single-bounce shadows from the directional light only. No global illumination.

**v2 (parking lot):** screen-space AO derived from primary ray steps; possibly cheap emissive bleed.

### 3.4 Sky

```rust
fn sky_set_gradient(top: u8, horizon: u8);                     // palette idxs
fn sky_set_sun_disc(color: u8, disc_size: f32);                // optional; size in radians
```

Rays that miss all geometry sample the sky: linear gradient between `horizon` (at ray.y = 0) and `top` (at ray.y = +1), with the sun disc rendered when the ray direction is within `disc_size` radians of the directional-light direction set in §3.3.

There is no cubemap or procedural atmosphere primitive. Carts wanting richer skies — clouds, mountains, floating islands — paint them as voxels in the world's outer shell. The 512³ per-scene grid leaves room for distant scenery beyond a typical 128–256³ playable area; view distance + fog do the fade.

### 3.5 Pause and cutscenes

```rust
fn world_set_paused(paused: bool);
```

Pauses Layer 2 rigid bodies and Layer 3 cellular automata. The cart's `update()` continues to run (so it can drive cutscene timelines, pause-menu UI, etc.) and audio continues to play. Resuming is a single call.

**Cutscenes are cart code.** The platform deliberately has no cutscene primitive. The pieces that *do* live in the host:

- `world_set_paused(true)` to freeze gameplay simulation.
- `camera_set_render_rect(0, 18, 256, 108)` (or similar) for letterbox bars.
- The `DollyCamera` SDK helper for keyframed camera motion.
- Voxel glyphs for dialog text (cart-rendered into actor volumes positioned in front of the camera — text is voxels, like everything else).

Skip handling, dialog progression, character animation, and scene scripting are all cart-side.

### 3.6 World mutation

```rust
fn set_voxel(pos: UVec3, material: u8);
fn fill_box(min: UVec3, max: UVec3, material: u8);
fn clear_world();
```

`UVec3` components are world-voxel coordinates `0..512`; values outside that range are clamped or rejected (host's choice — implementation must be consistent across ports).

All voxel reads, writes, and rendering target the **active scene** (see §3.7). `clear_world()` resets the active scene's voxel grid to all-air; other scenes are unaffected. Use sparingly; carts that want a fresh slate per level are better off authoring a separate scene up front.

Mutations seed the Layer 3 active set automatically (see §10.3) for any voxel whose material has CA flags. Direct world writes are intended for level setup and large infrequent edits — actor-shaped moving objects belong in §11 actors. Loading prebuilt voxel data is done at level setup via the cart's prefab table (see §11.4 / §13.6) and `actor_spawn_from`, not via a host-side blit API.

### 3.7 Scenes

A **scene** is one 512³ voxel grid the cart can address by `SceneId(u8)`. Each cart may use up to **256 scenes**. Scenes are the cart's unit of "level," "room," "stage," or anything else with a distinct voxel layout. The host doesn't impose semantics — it just keeps each scene's chunks separate and gives the cart a single API call to switch which one is active.

```rust
fn scene_set_active(scene: SceneId);
fn scene_get_active() -> SceneId;
```

Scene 0 is active at boot. `scene_set_active` switches the active scene; an unallocated scene reads as uniform air and lazy-allocates on first write, so addressing scene 200 costs nothing until you populate it.

**Carry-over semantics — what scene switching does and doesn't change:**

| State | Per-scene? | Notes |
|---|---|---|
| Voxel grid (chunks) | yes | The whole point of scenes. |
| Materials (§2 table) | no — cart-global | One palette per cart by design (§4.4). |
| Actors (§11) | no — cart-global | Carts that want per-scene NPCs despawn / spawn them on switch. |
| Prefabs + bake cache (§11.4) | no — cart-global | Source data is shared; bakes are reused across scenes. |
| Audio state, save block | no — cart-global | A song crossfades through scene transitions unless the cart stops it. |

This is deliberately minimal: the host *only* swaps which voxel grid is active. Cleanup on transition (despawn enemies, hide UI actors, clear sticky physics state) is cart-side. Carts that want elaborate transitions compose them on top of the primitive.

**Memory cost.** Empty world: ~2 KB for the scene slot table. Each populated scene adds 32 KB for its chunk slot table; populated chunks add their ~33 KB dense buffer. A typical multi-room cart with ~5 scenes × 50 chunks each is ~10 MB resident, well within the §13.8 budget.

### 3.8 Performance budget

256×144 = 36,864 rays/frame × 60 fps = **2.21 M rays/sec** at full-rect. ESP32-P4 (RISC-V @ 400 MHz w/ FPU) sustains this for typical sparse worlds. ESP32-S3 may need adaptive render res or 30 Hz under load. Reducing `view_distance` or shrinking the render rect (e.g., 256×108 letterbox = 27,648 rays/frame, 25% savings) are the cart's two biggest knobs.

---

## 4. Color

### 4.1 Structure

64 fixed RGB colors, organized as **16 ramps × 4 shades**. Each ramp is a single hue progressing from shadow to highlight: `(dark, mid-dark, mid-light, highlight)`. The 6-bit color field on a material entry decomposes as:

```
bit:  5 4 3 2 | 1 0
      [ramp]  [shade]
```

This decomposition is load-bearing. The renderer applies lighting by shifting the shade index while preserving the ramp:

```rust
let lit = palette[(material.color & 0b111100) | (brightness * 4).clamp(0, 3) as u8];
```

Voxel art's classic "top face bright, sides medium, shadowed face dark" is three shade-index lookups on the same ramp — same hue, different shade. Carts that want flat unlit colors pin a specific ramp+shade in their material; carts that want lit voxels rely on the renderer's shade shift.

### 4.2 Ramp categories

14 hue ramps + 2 neutral ramps:

| # | Ramp | Role |
|---|---|---|
| 0 | Brown | Wood, dirt, leather, deep earth |
| 1 | Tan | Sand, light wood, parchment, skin highlights |
| 2 | Forest green | Tree foliage, deep moss |
| 3 | Grass green | Grass, lime, springtime greens |
| 4 | Teal | Water shallows, jade, oxidized metal |
| 5 | Cyan | Ice, glow, sci-fi accents |
| 6 | Sky blue | Skies, water mid-depth, denim |
| 7 | Deep blue | Night, deep water, dark cloth |
| 8 | Purple | Magic, twilight, dark cloth |
| 9 | Pink | Flowers, flesh, soft accents |
| 10 | Red | Blood, fire mid-tone, warnings, bricks |
| 11 | Orange | Fire, autumn, terracotta, citrus |
| 12 | Yellow | Sun, gold, hazard, light sources |
| 13 | Magenta | Neon, alien, vivid accent |
| 14 | Cool gray | Stone, concrete, steel — pure-black-ish at shade 0 |
| 15 | Warm gray | Bone, ash, sandstone — pure-white-ish at shade 3 |

There is no dedicated skin-tone ramp — skin tones are mixed from Brown / Tan / Pink / Red. There is no separate "pure black" or "pure white" entry; the near-black at ramp 14 shade 0 and the near-white at ramp 15 shade 3 cover those roles.

Neon / saturated accents come from shade 3 of vivid ramps (Magenta, Cyan, Yellow). PICO-8-style "neon pink" is Pink shade 3.

### 4.3 RGB values (v0.1 draft)

Anchor values, not final. Subject to revision before v1.0 once palette art is tuned against real voxel scenes.

```
Ramp  Shade 0   Shade 1   Shade 2   Shade 3
 0   #2a1810   #5c2e1a   #8d4a26   #c97c4e   Brown
 1   #4a3520   #7a5d3e   #b8946a   #ead5a8   Tan
 2   #0a2516   #1d4a2c   #3a7a4a   #6cb86e   Forest green
 3   #2d3e15   #4f6a26   #82a738   #c0db5c   Grass green
 4   #0a3530   #1c5d56   #389082   #6bd1bc   Teal
 5   #0d3848   #1a6580   #4ba0c0   #88d8ee   Cyan
 6   #1a2848   #2e4d80   #5a82c8   #a8c9f0   Sky blue
 7   #0a1438   #1a2660   #344690   #6680d0   Deep blue
 8   #1d1240   #3a2270   #6849a8   #a48de0   Purple
 9   #481a3a   #802855   #c0568a   #f0a0c2   Pink
10   #401418   #7a232a   #c83a40   #f08070   Red
11   #4a1e0a   #883818   #d8642a   #f0a868   Orange
12   #4a3a0a   #88701a   #d8b830   #f8e878   Yellow
13   #38104a   #6e2680   #b048c0   #ea88f0   Magenta
14   #050810   #2c3848   #6a7888   #d8e0e8   Cool gray
15   #1a1612   #4a3e36   #98897a   #f5f0e8   Warm gray
```

Design invariants for future tuning:

- Each ramp's shade 0 trends slightly *cooler* than its shade 3 — cool shadows, warm highlights, the way natural light reads.
- Saturation peaks at shade 2 (the "body" color) and drops at both shade 0 (toward dark) and shade 3 (toward off-white).
- Highlights (shade 3) tint toward each ramp's own hue rather than pure white, to keep highlights from going flat across the palette.
- Shade 0 of each hue ramp avoids true `#000` so shadow areas retain hue character; pure near-black lives only in ramp 14 shade 0.

### 4.4 Per-cart materials (recap)

Each cart ships a 256-entry material table (§2). Each material's 6-bit color field indexes the system palette via the `(ramp << 2) | shade` decomposition above. **Carts cannot define their own RGB values.** This is deliberate: a fixed system palette is what gives voxlconsl carts a recognizable shared visual identity, the way Game Boy green-tinting or PICO-8's 16-color set do for those platforms.

---

## 5. Audio — synth + MIDI + samples

The audio system is a built-in synthesizer driven by MIDI, with a general-purpose sample bank usable for both pitched instruments and one-shot SFX. Carts ship **patches** (instrument definitions, either synth or sampler), **MIDI sequences**, and **samples**. There is no tracker — music composition is done in any DAW that exports Standard MIDI Files; SFX are triggered directly from cart code.

### 5.1 Patch engine

A **patch** is the cart's instrument definition. Each patch is one of two kinds:

- **Synth** — 2-osc subtractive engine with optional FM mode.
- **Sampler** — plays one or more samples from the cart's sample bank, pitch-shifted by MIDI note.

The non-oscillator portion of the architecture (filter, amp env, filter env, LFO, glide) is shared between both kinds — only the source section differs.

**Per voice (synth kind):**

```
osc A ─┐
       ├─► mix ─► filter ─► VCA ─► to bus
osc B ─┘             ▲        ▲
                     │        │
              env F (ADSR)  env A (ADSR)
                     ▲        ▲
                     └── LFO ─┘  (routable to: pitch, filter, amp, pan)
```

| Block | Settings |
|---|---|
| Oscillators (×2) | Mode: `sine`, `saw`, `square+pwm`, `triangle`, `noise`, `fm2op`. Detune, octave, mix level. |
| Filter | Mode: `lp`, `hp`, `bp`, `off`. Cutoff, resonance. |
| Amp envelope | ADSR. |
| Filter envelope | ADSR + depth (signed). |
| LFO | Rate, shape (`sine`/`tri`/`square`/`s&h`), 1 routing target × depth. |
| Glide | Portamento time. |

`fm2op` mode treats osc A as carrier, osc B as modulator with its own envelope; ratio + index parameters are part of the patch.

**Per voice (sampler kind):**

```
sample (selected by note via key zones) ─► resampler ─► filter ─► VCA ─► to bus
                                              ▲                ▲        ▲
                                              │                │        │
                                       env F (ADSR)     env A (ADSR)   LFO
```

| Block | Settings |
|---|---|
| Key zones (×8 max) | Each: `low_note`, `high_note`, `root_note`, `sample_slot`, `volume_offset`. Single-sample mode = 1 zone covering 0–127. |
| Resampler | Pitch shift = `(played_note − root_note)` semitones from the matching zone's sample. |
| Loop | Per-zone optional loop: `(start, end)` sample positions, sustain-loop semantics. |
| Filter / envelopes / LFO | Same as synth kind. |

**Patch budget:** ~64 bytes per patch (synth) or up to ~256 bytes per patch (sampler with 8 zones). Up to **16 patches per cart**, freely mixed between kinds.

### 5.2 MIDI

**Channels:** 16, exactly per MIDI spec. Each channel is bound to one cart patch (default channel→patch mapping is identity; carts can remap).

**Channel 10** by default is bound to a multi-sample drum-kit patch (a sampler patch where each note number maps to a different sample, slot = note − 35). This preserves the GM convention so DAW MIDI exports work out of the box. Carts can rebind channel 10 to any other patch via `program_change` if they want to reclaim it for melodic content.

**Recognized messages:**

| Message | Effect |
|---|---|
| Note On / Off | Trigger / release voice |
| Pitch Bend | ±2 semitones default, configurable per channel |
| CC 1 (Mod wheel) | LFO depth |
| CC 7 (Volume) | Channel volume |
| CC 10 (Pan) | Channel pan |
| CC 11 (Expression) | Multiplied with volume |
| CC 64 (Sustain) | Hold notes until released |
| CC 91 (Reverb send) | Per-channel reverb amount |
| CC 93 (Chorus send) | Per-channel delay amount (we map "chorus" to delay) |
| Program Change | Rebind channel to patch index |

Other CCs are ignored. The CC table is **fixed in v1** — there is no per-cart CC routing or remapping. Carts wanting custom modulation paths use the runtime patch-editing API (§5.7) directly. SysEx is reserved for future use.

**Polyphony:** 32 voices total, shared across channels. Voice-stealing strategy: oldest released note first, then oldest held note. (Bumped from 16 once we audited per-voice DSP cost against the ESP32-P4 budget; ESP32-S3 ports may need to dynamically reduce this.)

### 5.3 Sequenced music

Carts can carry **Standard MIDI Files** (SMF type 0 or 1). Up to **8 song slots** per cart. Host functions:

```rust
fn music_play(slot: u8, loop_: bool);
fn music_stop();
fn music_set_tempo_scale(scale: f32);  // 1.0 = as authored
fn music_position_beats() -> f32;       // for visual sync
```

### 5.4 Sample bank

The cart ships a bank of audio samples used by sampler patches (§5.1) and by the direct SFX API (§5.7). Samples are general-purpose: drums, footsteps, explosions, voice clips, ambient loops, anything PCM.

| Field | Notes |
|---|---|
| Slot count | Up to **64 samples per cart** |
| Format | 8-bit unsigned PCM |
| Sample rate | 11.025 or 22.05 kHz, declared per sample |
| Length | Variable per slot, total bounded by section budget |
| Loop points | Optional per sample: `(loop_start, loop_end)` for sustained sounds |

Sample data is **static** — carts cannot generate or mutate sample data at runtime. Procedural / dynamic sounds use the synth engine (which is fully runtime-mutable per §5.7). Carts can however change *which* sample a sampler patch plays at runtime.

The drum-kit convention on channel 10 is just one use of the sample bank — the bundler doesn't distinguish "drum samples" from "sfx samples" or any other label. A sample is a sample.

### 5.5 Effects bus

Two shared sends, both fixed-architecture:

- **Reverb:** Schroeder-style or FDN. Two parameters: room size, damping. Cart sets globally.
- **Delay:** stereo cross-feedback. Parameters: time (ms), feedback. Cart sets globally.

No per-instrument insert effects in v0.1.

### 5.6 Real-time API

Two surfaces for triggering audio outside of sequenced MIDI playback. Both are first-class — adaptive music, generative composition, procedural sound design, and SFX all happen here.

**Real-time MIDI:**

```rust
fn note_on(channel: u8, note: u8, velocity: u8);
fn note_off(channel: u8, note: u8);
fn pitch_bend(channel: u8, value: i16);  // -8192..8191
fn cc(channel: u8, controller: u8, value: u8);
fn program_change(channel: u8, patch: u8);
fn all_notes_off(channel: u8);
```

Sequenced playback (§5.3) and real-time events coexist — a cart can play a MIDI song while also firing one-shot notes on other channels. There's no audio-thread / cart-thread distinction visible to the cart; events are queued and applied at the next mixer block boundary (~3 ms latency at 22.05 kHz / 64-sample blocks).

**One-shot SFX:**

For fire-and-forget sample playback (footsteps, hits, UI clicks, ambient one-shots) without setting up a patch + channel routing:

```rust
fn sfx_play(slot: u8, volume: u8, pan: i8, pitch_cents: i16, loop_: bool) -> Option<VoiceId>;
fn sfx_stop(voice: VoiceId);
fn sfx_set_volume(voice: VoiceId, volume: u8);
fn sfx_set_pitch(voice: VoiceId, pitch_cents: i16);
```

- `volume`: 0–127 (MIDI-style velocity).
- `pan`: -64 (full left) to +63 (full right).
- `pitch_cents`: 0 = original pitch; ±100 = ±1 semitone.
- `loop_`: if true, sample plays its declared loop region indefinitely until stopped. The returned `VoiceId` lets the cart stop or modulate the voice; non-looped one-shots can ignore the return.

SFX voices share the same 32-voice polyphony pool as MIDI notes — playing many SFX simultaneously will steal voices via the same oldest-released-first rule. Cart-side bookkeeping isn't required; voices are reaped automatically when their sample finishes (or when stopped).

### 5.7 Runtime patch editing

Patches are mutable at runtime. Any parameter of any patch can be changed at any time. Edits apply at the next mixer block boundary and affect both new and currently-sounding voices using that patch. This makes the synth a first-class instrument the cart *plays* — not just a static asset.

Use cases this enables:

- A cart that morphs a pad's filter cutoff with player movement.
- Generative music systems that mutate timbre over time.
- Procedural SFX (build a sound from primitives in code).
- "Synth editor" carts — the platform's own instrument-design tool is just a regular cart.

Per-block API (one function per synth block, all parameters typed):

```rust
fn patch_set_osc(
    patch: u8, osc: u8 /*0|1*/,
    mode: OscMode, detune_cents: i16, octave: i8, level: u8,
);
fn patch_set_fm(patch: u8, ratio: u16 /*Q8.8*/, index: u16 /*Q8.8*/);
fn patch_set_filter(patch: u8, mode: FilterMode, cutoff: u16, resonance: u8);
fn patch_set_amp_env(patch: u8, a: u16, d: u16, s: u8, r: u16);  // ms,ms,level,ms
fn patch_set_filter_env(patch: u8, a: u16, d: u16, s: u8, r: u16, depth: i8);
fn patch_set_lfo(patch: u8, rate: u16 /*centihertz*/, shape: LfoShape, target: LfoTarget, depth: i8);
fn patch_set_glide(patch: u8, ms: u16);

// Bulk operations
fn patch_load(patch: u8, src: *const u8, len: u32);  // load a serialized patch blob (64 B for synth, up to ~256 B for sampler)
fn patch_save(patch: u8, dst: *mut u8) -> u32;        // serialize current state; returns bytes written
fn patch_copy(src: u8, dst: u8);
fn patch_reset(patch: u8);                             // back to default sine
```

**Edits per frame:** unlimited from the cart's POV. The host coalesces — only the latest value of each parameter per block matters. This means a cart can do `patch_set_filter` 100×/frame to drive automation lanes from any source (gameplay state, RNG, an LFO computed in cart code, etc.) without worrying about flooding the audio thread.

**Voice continuity:** changing oscillator `mode` mid-note may cause a discontinuity (audible click). The cart is responsible for smoothing if it cares. Filter and envelope-level edits are continuous-safe by construction.

**Persistence:** runtime edits do not modify the cart file. To survive across boots, the cart writes the patch blob to its save block (§7).

### 5.8 Mixer

22.05 kHz internal mix, 64-sample blocks (~2.9 ms latency), mono+pan → stereo. Host upsamples / converts as the output device requires.

### 5.9 Audio section budget

| | Limit |
|---|---|
| Patches | 16 × up to 256 B = ≤ 4 KB |
| Sample bank | ≤ 1 MB total (general samples + drums + SFX, all in one pool) |
| MIDI songs | ≤ 256 KB total (8 slots) |
| **Total audio section** | **≤ 1.5 MB** |

The world section absorbs the difference (25.5 MB → 24.5 MB).

---

## 6. Input

The platform exposes input as a system of **actions** — gameplay verbs the cart names at boot. The port translates the device's physical input topology (sticks, keyboard, touchscreen, gyro, etc.) to the cart's actions. **Carts never see physical inputs directly.**

This is what makes a single cart run unchanged across browser, touch-only mobile, and physical handheld — three completely different input topologies. It's also what makes user rebinding possible without per-cart support: the platform owns the mapping, the cart only declares intent.

The console is **single-player**. Networking-based multiplayer is out of v1 scope.

### 6.1 Action model

```rust
pub enum ActionKind {
    Button,        // discrete: held / pressed / released / held_ms
    Axis1D,        // f32 in -1.0..1.0 (signed) or 0.0..1.0 (unsigned by binding)
    Axis2D,        // (f32, f32) inside the unit disc — sticks or aim deltas
}

pub enum BindingHint {
    None,                // platform infers from kind alone
    PrimaryMovement,     // 2D ground/world movement (Axis2D)
    Aim,                 // 2D look/aim — accepts stick or pointer-delta (Axis2D)
    Zoom,                // zoom in/out — mouse wheel or stick axis (Axis1D);
                         // positive = closer, negative = farther
    PrimaryFire,         // main "do it" button (Button)
    SecondaryFire,       // alt fire / aim-down-sights / right click
    Confirm, Cancel,     // dialog / UI semantics (Button)
    Menu, Pause,         // system-flavored (Button)
}

pub struct ActionDecl<'a> {
    pub name: &'a str,   // used by the system rebind UI; never crosses host boundary at frame time
    pub kind: ActionKind,
    pub hint: BindingHint,
}

pub struct ActionHandle(u32);  // opaque; returned at declaration time
```

### 6.2 Lifecycle

Actions are declared once during `init()`. The cart receives a handle per action and stores them. All subsequent queries use handles — names never cross the host boundary at frame rate.

```rust
fn input_declare_actions(decls: &[ActionDecl]) -> Vec<ActionHandle>;
```

Re-declaring actions during `update()` is undefined behavior. Carts that conditionally need extra actions should pre-declare the union and gate them in cart code.

**Limit:** 64 cart-declared actions per cart.

### 6.3 Reserved system actions

The platform always provides two action handles, available without declaration:

```rust
pub const SYSTEM_PAUSE: ActionHandle = ActionHandle(0xFFFF_FFFE);
pub const SYSTEM_MENU:  ActionHandle = ActionHandle(0xFFFF_FFFF);
```

`SYSTEM_PAUSE` fires when the user invokes the platform's pause control (Start on a controller, dedicated key on browser, top-edge gesture on mobile). The cart may read it to drive its own pause menu (typically: call `world_set_paused(true)` and render an overlay). If the cart doesn't read or react within a short window, the platform shows a default suspend overlay itself.

`SYSTEM_MENU` fires for the platform's higher-level menu (rebind, save state, exit cart). Carts can observe but should not interpose.

Cart-declared handles never collide with these reserved values.

### 6.4 Polling API

```rust
fn input_action_button(h: ActionHandle) -> bool;          // currently held
fn input_action_pressed(h: ActionHandle) -> bool;          // edge: down this frame
fn input_action_released(h: ActionHandle) -> bool;         // edge: up this frame
fn input_action_held_ms(h: ActionHandle) -> u32;           // 0 if not held

fn input_action_axis1d(h: ActionHandle) -> f32;
fn input_action_axis2d(h: ActionHandle) -> (f32, f32);

fn input_action_active(h: ActionHandle) -> bool;           // true iff bound to anything on this port
```

Querying a Button function on an Axis-kind action (or vice versa) returns the zero value silently — same shape, no panic. `input_action_active` is the polite check: an action that the active port can't bind (e.g., a `Zoom` axis on a no-wheel-no-stick handheld variant) returns `false` and the cart can omit the dependent UI.

Edges cover the entire previous frame; a press-and-release between two `update()` calls surfaces both edges.

### 6.5 Binding labels

The host owns a read-only label table that maps every declared action handle to a short, human-readable glyph for the physical input currently driving it: `"J"`, `"LMB"`, `"Esc"`, `"A"` (gamepad face button), `"Start"`, `"RStick"`, `"Tap"`. Carts paint these into HUD prompts ("press [J] to act") so the displayed key tracks the active device instead of being hard-coded.

```rust
fn input_action_label(h: ActionHandle, out: &mut [u8]) -> &str;
```

The host writes UTF-8 bytes for the active binding into the cart-provided slice and returns the populated subslice. Labels are short — a 16-byte scratch is enough for every binding the v0.1 ports emit; longer labels are truncated without error.

The label is live. Within one frame of the host switching input devices (gamepad plugged in mid-session, touch overlay tapped on a hybrid device) or the user rebinding via `SYSTEM_MENU`, the returned string updates. Returns `""` when `h` is unbound on the active port.

Behavior:

- For actions bound to multiple inputs on the same port (e.g., `PrimaryFire` = LMB *or* J on browser), the label is the glyph for the *most recently used* binding, falling back to the port's canonical default at session start.
- Reserved system handles carry labels too: `SYSTEM_PAUSE` returns `"Start"` on a gamepad, `"Esc"` / `"Tab"` on the browser.
- The label is opaque text — carts must not parse it. Future ports may return Unicode glyphs in the private-use area for gamepad face icons; carts should render whatever bytes they receive.
- Carts that paint these labels should cache the previous string per handle and repaint only on change.

The host must update the table proactively (no cart-side request needed) so that the next `input_action_label` call after a device switch already returns the new glyph.

### 6.6 Deadzones

The host applies a fixed ~3% radial deadzone on Axis2D actions bound to physical sticks (hardware-noise floor). On touch virtual sticks, the host applies a small per-touch filter. Carts apply their own gameplay deadzone (typically 15–25%) on top — there is no raw-value escape hatch.

### 6.7 Port binding

Each port's auto-binder produces default action → physical-input mappings from `kind + hint`. The user can override via the system rebind UI (§6.8).

**Reference handheld profile.** voxlconsl's native target is a home-buildable handheld with a fixed, minimal input surface:

- 4 face buttons — **A** (south), **B** (east), **X** (north), **Y** (west).
- 2 system buttons — **Start**, **Select**.
- 2 bumpers — **L**, **R** (digital, no analog travel).
- 2 analog sticks — **LStick**, **RStick** (Axis2D each; stick-click state is a build-time option, so carts must not require it).
- No triggers, no touchscreen on the handheld; touchscreen and pointer input live on the browser / mobile ports.

Total: 8 digital buttons + 2 sticks. There is no analog trigger; carts that want a 1D analog input (`Zoom`, `Axis1D` + `None`) read a synthetic axis from the bumpers — **R held = +1.0, L held = -1.0**, neither = 0.0, both = 0.0. This keeps cart code identical between handheld (digital bumpers) and browser (mouse wheel) — both deliver `Axis1D`.

**Physical handheld:**

| Hint | Default |
|---|---|
| `PrimaryMovement` | Left stick |
| `Aim` | Right stick |
| `Zoom` | L/R bumpers (synthesised Axis1D: R = +1.0, L = -1.0) |
| `PrimaryFire` | A |
| `SecondaryFire` | B |
| `Confirm` | X |
| `Cancel` | Y |
| `Pause` | Start |
| `Menu` | Select |
| `None` + `Button` | unbound (no spare buttons after the eight hint slots) |
| `None` + `Axis1D` | unbound |
| `None` + `Axis2D` | unbound |

Every face button maps 1:1 to exactly one hint — there is no aliasing between `PrimaryFire` and `Confirm` (or `SecondaryFire` and `Cancel`). A cart that declares both gets two distinct buttons.

**Browser (keyboard + mouse, optional gamepad):**

| Hint | No gamepad connected | Gamepad connected |
|---|---|---|
| `PrimaryMovement` | WASD | Left stick |
| `Aim` | Mouse delta (only when canvas has pointer lock) | Right stick |
| `Zoom` | Mouse wheel | L/R bumpers (synthesised); falls back to triggers if the pad has them |
| `PrimaryFire` | Left mouse / J | A |
| `SecondaryFire` | Right mouse / K | B |
| `Confirm` | U | X |
| `Cancel` | I | Y |
| `Pause` | Enter | Start |
| `Menu` | Escape | Select |
| `None` + `Button` | Space, F, G, H… | unbound |
| `None` + `Axis1D` | first available digit-key pair | unbound |
| `None` + `Axis2D` | first available stick-equivalent | unbound |

The four face-button hints (PrimaryFire / SecondaryFire / Confirm / Cancel) map to the **J K U I** diamond under the right hand:

```
   U  I       <- Confirm, Cancel
   J  K       <- PrimaryFire, SecondaryFire
```

This keeps the right hand on the home row so the player can pan with WASD (left hand), aim with the mouse, and reach any of the four face buttons without a context switch. Enter and Escape — the two most-reached-for "go" and "back" keys when the right hand sits on the surface — become Pause (Start) and Menu (Select).

Mouse drives the right-stick equivalent *only when no gamepad is connected*; with a pad attached, mouse motion is ignored — the cart only sees stick input.

**Touch-only mobile (browser or app):**

The port auto-generates a virtual overlay from the cart's declared action list. Carts cannot author the layout — the platform owns it, so layout improvements roll out across all carts.

Layout heuristics:

- `PrimaryMovement` Axis2D → virtual analog stick, bottom-left thumb zone.
- `Aim` Axis2D → free-drag zone over the right half of the screen, delta semantics (drag to look).
- `PrimaryFire` Button → large tap target overlapping the aim area, so the right thumb can fire while aiming.
- `SecondaryFire` → tap target next to PrimaryFire in the right-thumb cluster.
- `Confirm` / `Cancel` → tap targets in the right-thumb cluster, smaller than the fire buttons; `Confirm` placed near `PrimaryFire`, `Cancel` near a screen edge for "swipe-to-back" affordance.
- `Zoom` Axis1D → pinch gesture on the world view (no dedicated tap target).
- `Pause` / `Menu` → small icons, top-right corner; or a top-edge swipe for `Pause`.

If the cart declares more Button actions than the overlay can fit (~6 visible without crowding), the extras spill into a pop-out drawer reachable from the button row.

### 6.8 User rebinding

The platform provides a system rebind overlay reachable via `SYSTEM_MENU`. It lists each cart's declared actions by name and lets the user reassign physical inputs (or, on touch, reposition virtual elements). Bindings persist per-cart in platform storage, separate from the cart's save block.

Carts never set bindings programmatically; they can only *display* the current binding via the label table in §6.5.

### 6.9 Rumble (best-effort output)

```rust
fn output_rumble(intensity: f32, duration_ms: u32);
```

Ports without haptic hardware ignore the call. Carts must not gate gameplay on rumble.

---

## 7. Cart format (`.voxl`)

Single binary file, hard cap 32 MB. Chunked layout, all multi-byte values little-endian. The header carries an authoritative section table; the bundler may emit sections in any order and may omit any section other than `Code`.

**Header (32 bytes):**

```
Offset  Field           Size    Notes
------  --------------  ------  --------------------------------------
0       magic           10 B    "VOXLCONSL\0"
                                (0x56 0x4F 0x58 0x4C 0x43 0x4F 0x4E
                                 0x53 0x4C 0x00)
10      version         u16     format version (currently 1)
12      flags           u16     0 in v1; reserved
14      section_count   u8      number of section table entries (≤ 16
                                in v1; the section table immediately
                                follows the header)
15      reserved        u8      0
16      total_size      u32     file size in bytes (must equal the
                                actual length on disk)
20      crc32           u32     CRC-32/ISO-HDLC of the entire file with
                                this 4-byte field zeroed at the time
                                of computation
24      reserved        8 B     zero
```

**Section table** — `section_count` × 16 bytes, immediately after the header:

```
Offset  Field                Size    Notes
------  -------------------  ------  --------------------------------------
0       id                   u16     well-known section id (table below)
2       flags                u16     bit 0: compressed (zstd); 0 in v1
4       offset               u32     byte offset of this section's data
                                     from the start of the file
8       size                 u32     bytes on disk
12      uncompressed_size    u32     decompressed length (== size when
                                     uncompressed)
```

A given section id appears at most once. Sections may live in any order on disk; readers MUST consult the section table rather than assuming a layout.

**Section ids (v1):**

| ID | Name        | Contents |
|---|---|---|
| 0 | Metadata    | UTF-8 TOML string holding the resolved `[cart]` manifest table (name, title, author, version, spec_version, description, license). Informational; the runtime does not require it. |
| 1 | Code        | Raw WASM module bytes, ≤ 1 MB recommended. **Required** in v1 — a cart with no Code section is rejected at load. |
| 2 | Materials   | 256 × packed material struct (§2.4). When present the host pre-populates the material table at boot; runtime `material_define` calls remain authoritative and may override. |
| 3 | World       | SVO-encoded world chunks (§13). When present the host pre-populates active-scene chunks before `init` runs. |
| 4 | Audio       | Audio asset blob — synth patches, sample bank, MIDI songs (§5). |
| 5 | SaveSchema  | UTF-8 TOML string declaring persistent state shape. Documentation-only in v1 (§7 persistent-state note below). |

**v1 minimum cart.** A valid v1 cart need only carry the Code section. Any other section is optional; the runtime treats absence as the natural default (no metadata displayed, materials defined entirely at runtime, world starts empty, no audio assets, no save schema). This lets early carts ship without forcing every subsystem (audio, world bundling, etc.) through the bundler before it's ready.

**Section ids 6..=255** are reserved for future use; readers MUST tolerate (skip) unknown section ids in newer carts on a best-effort basis as long as the rest of the file validates.

**Persistent state:** carts may declare a save block (≤ 64 KB) the host writes to local storage / flash. The cart manages its own serialization — the host treats the save block as opaque bytes. Carts read and write via `save_read` / `save_write` (§8). The optional `save.toml` schema in cart sources is **documentation only in v1**: it lets the editor and tooling display field labels but is not embedded in the cart, not enforced at runtime, and does not perform migration. Schema-driven save formats with versioned migration are parking-lotted to v2.

---

## 8. Host API surface (cart ↔ host)

This section is the index of the cart-facing API. Function declarations live in the relevant feature sections so they stay co-located with their semantics; this section pulls them together and lays out the cross-cutting rules (cart entry points, the WASM ABI conventions, and the small set of "miscellaneous" host functions that don't fit any other section).

### 8.1 Cart entry points (cart exports)

The cart exports exactly three functions:

```rust
extern "C" fn init();             // called once at boot; declare actions, set up state
extern "C" fn update(dt_ms: u32); // called every frame; tick game logic
extern "C" fn render();           // called every frame, after update; configure camera/lights/sky
```

The host calls these from the per-frame loop described in §10. Carts must export all three; the host treats missing exports as a load-time error.

### 8.2 Host imports (by feature section)

| Section | What it covers |
|---|---|
| §3.2 | Camera: `camera_set_lookat`, `camera_set_euler`, `camera_set_projection`, `camera_set_view_distance`, `camera_set_fog`, `camera_set_render_rect` |
| §3.3 | Lighting: `light_set_sun`, `light_set_ambient` |
| §3.4 | Sky: `sky_set_gradient`, `sky_set_sun_disc` |
| §3.5 | Pause: `world_set_paused` |
| §3.6 | World mutation: `set_voxel`, `fill_box`, `clear_world` |
| §3.7 | Scenes: `scene_set_active`, `scene_get_active` |
| §5.3 | Sequenced music: `music_play`, `music_stop`, `music_set_tempo_scale`, `music_position_beats` |
| §5.6 | Real-time audio: `note_on`, `note_off`, `pitch_bend`, `cc`, `program_change`, `all_notes_off`, `sfx_play`, `sfx_stop`, `sfx_set_volume`, `sfx_set_pitch` |
| §5.7 | Patch editing: `patch_set_osc`, `patch_set_fm`, `patch_set_filter`, `patch_set_amp_env`, `patch_set_filter_env`, `patch_set_lfo`, `patch_set_glide`, `patch_load`, `patch_save`, `patch_copy`, `patch_reset` |
| §6.2 | Input actions: `input_declare_actions`, `input_action_*` (button / pressed / released / held_ms / axis1d / axis2d / active) |
| §6.5 | Binding labels: `input_action_label` |
| §6.9 | Output: `output_rumble` |
| §10.1 | Physics queries: `raycast`, `raycast_world_only`, `aabb_overlap_world`, `aabb_overlap_actors`, `sweep_aabb`, `material_at` |
| §10.2 | Rigid bodies: `body_spawn`, `body_despawn`, `body_set_kind`, `body_set_position`, `body_set_velocity`, `body_get`, `body_apply_impulse`, `body_set_layer`, `body_set_sensor`, `world_set_gravity`, `drain_collision_events` |
| §10.3 | CA control: `ca_set_budget`, `ca_get_budget`, `ca_mark_active`, `ca_active_count`, `ca_set_global_param` |
| §11.7 | Actors: `actor_spawn`, `actor_spawn_from`, `actor_despawn`, `actor_count`, `actor_set_*` / `actor_get_*` (transform + visibility + bounds), `actor_set_voxel`, `actor_fill_box`, `actor_clear`, `actor_load_volume`, `actor_volume_size`, `actor_get_voxel` |

### 8.3 Miscellaneous host functions

Functions that don't belong to any feature section:

```rust
fn rand() -> u32;                         // host-seeded; cart can request a specific seed at boot
fn time_ms() -> u32;                      // monotonic ms since cart boot (wraps at u32::MAX)

fn save_read(buf: *mut u8, len: u32) -> u32;     // returns bytes read; 0 if no save
fn save_write(buf: *const u8, len: u32);         // host clamps len to ≤ save block size (§7)

fn log(ptr: *const u8, len: u32);                // debug only; no-op on release builds
```

### 8.4 WASM ABI conventions

The Rust signatures shown throughout the spec are the **SDK-side wrappers** carts use, not literal host-import declarations. WASM's ABI doesn't natively support struct-or-tuple returns, slice arguments, or references across the boundary; the SDK wraps a thinner pointer-based ABI for ergonomics.

Concretely:

- **Tuple returns** like `input_action_axis2d() -> (f32, f32)` are implemented as host imports that take a `*mut` output pointer, with the SDK reading the values back into a tuple. Spec'd return types describe what the cart sees, not the byte-level import signature.
- **Slice arguments** like `input_declare_actions(decls: &[ActionDecl])` are implemented as `(ptr: *const ActionDecl, len: u32)` host imports, with the SDK constructing the slice for the wrapper.
- **Slice returns** like `Vec<ActionHandle>` are implemented as a "cart provides a buffer, host writes handles + count" pair of imports.
- **Struct returns** like `body_get() -> BodyState` use the same out-pointer pattern as tuple returns.

The byte-level host ABI is part of the SDK's contract with the runtime, not part of the user-facing spec — but it is stable and documented in `voxlconsl-sdk` source. Any language with a WASM target can bind to that ABI.

---

## 9. Browser host (reference implementation)

- Written in Rust, compiled to WASM, served as a static page.
- **Cart `.wasm` runs inside `wasmi`** on every port — including browser. The browser does *not* use its native WebAssembly engine to host carts. Same runtime everywhere = identical behavior, deterministic replay, easier sandboxing, fewer port-specific edge cases. The renderer is the perf-critical path and runs in native (or browser-compiled) host code, so the cart-side WASM interpreter speed is rarely the bottleneck.
- Renderer: pure-Rust SVO ray marcher, same crate used on hardware ports. Browser blits the host's framebuffer to Canvas2D via `putImageData` (or WebGPU compute later as an optimization).
- All ports must produce bit-identical framebuffers given identical cart state and input — the browser is the conformance reference.

---

## 10. Physics

The platform provides four layers of physics primitives. Each is independently optional from the cart's perspective — a cart can use Layer 1 alone and roll its own movement, or opt into more. Per-port budgets cap each layer's CPU spend, and carts can lower (or zero out) any budget to reclaim time for the renderer or game logic.

| Layer | Purpose | v1 status | P4 budget (full) |
|---|---|---|---|
| 1. Queries | Read-only intersection primitives | In | < 1% of frame |
| 2. Rigid bodies | Host-integrated AABB / sphere bodies | In | 5–10% of frame |
| 3. Cellular automata | Sand / water / fire / gas voxel sims | In, opt-in | up to ~25% of frame |
| 4. Soft bodies / structural sim | Teardown-style breakable structures | **Out — not planned** | — |

**Per-frame loop:**

```
host: poll inputs
cart: update(dt)
host: integrate Layer 2 bodies          (fixed-step, may run multiple substeps per frame)
host: tick Layer 3 active set            (bounded by per-port cap)
cart: render()                            (camera/lights only)
host: ray-march framebuffer and present
```

### 10.1 Layer 1 — Queries

Read-only intersection primitives against the world grid and actors. Always available. Pure functions of world state — no host-side stored data.

```rust
pub struct Hit {
    pub pos: UVec3,         // voxel coordinate of the hit
    pub material: u8,
    pub normal: IVec3,      // axis-aligned face normal, components in {-1, 0, +1}
    pub t: f32,             // distance along ray
}

pub struct SweepHit {
    pub t: f32,
    pub normal: IVec3,
    pub blocked_by_actor: Option<ActorId>,
}

fn raycast(origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Hit>;
fn raycast_world_only(origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Hit>;
fn aabb_overlap_world(min: Vec3, max: Vec3) -> bool;
fn aabb_overlap_actors(min: Vec3, max: Vec3) -> ActorMask;
fn sweep_aabb(min: Vec3, max: Vec3, motion: Vec3) -> Option<SweepHit>;
fn material_at(x: u16, y: u16, z: u16) -> u8;
```

The same SVO traversal that drives the renderer is reused here — single implementation, two consumers.

### 10.2 Layer 2 — Rigid bodies

Host-integrated bodies attached to actors (see §11). The cart spawns and despawns bodies; the host applies gravity, resolves collisions against the world grid, and resolves pairwise actor-vs-actor collisions.

**Shapes:** AABB (axis-aligned, no rotation) and sphere only. Rotational dynamics — including OBBs, capsules, and angular impulses — are out of scope for v1. The aesthetic is "voxels gridded, bodies snappy," not realistic mechanics.

**Body kinds:**

| Kind | Behavior |
|---|---|
| `Static` | Never moves. Other bodies collide against it. Mass treated as infinite regardless of the field value. Cheapest. |
| `Dynamic` | Fully simulated: gravity applies, collisions resolve, impulses move it. The default. |
| `Kinematic` | Position is cart-controlled (`body_set_position` / `body_set_velocity`). Pushes dynamic bodies but isn't itself pushed by them; ignores gravity. Use for moving platforms, doors, scripted hazards, AI characters whose motion is bespoke. |

**Per-body state:**

| Field | Notes |
|---|---|
| Kind | `Static` \| `Dynamic` \| `Kinematic` |
| Shape | AABB (extents) or sphere (radius) |
| Position | World-space `Vec3` |
| Velocity | `Vec3` |
| Mass | `f32` (ignored for Static / Kinematic) |
| Restitution | 0.0 (inelastic) – 1.0 (perfectly elastic) |
| Friction | 0.0 – 1.0 |
| Layer | `u8`, range 0–7 — which collision layer this body belongs to |
| Mask | `u8` — bitmask of layers this body collides with (bit `i` = collide with layer `i`) |
| Sensor | `bool`; if true, generates events but resolves no contact |

Layer / mask gives 8 layers and an 8×8 collision matrix (Unity-style). Two bodies collide if `A.mask & (1 << B.layer)` and `B.mask & (1 << A.layer)` are both nonzero. 8 layers is enough for typical cart needs (player / enemy / projectile / pickup / world-decoration / trigger / debris + 1 spare); a richer model (16/32 layers) is parking-lotted to v2.

**Caps:** 64 active bodies per cart, 256 collision events queued per tick (overflow drops oldest, with a telemetry counter exposed).

**API:**
```rust
fn body_spawn(actor: ActorId, kind: BodyKind, shape: Shape, mass: f32) -> BodyId;
fn body_despawn(id: BodyId);
fn body_set_kind(id: BodyId, kind: BodyKind);
fn body_set_position(id: BodyId, pos: Vec3);   // primary path for Kinematic; valid but unusual on Dynamic
fn body_set_velocity(id: BodyId, v: Vec3);
fn body_get(id: BodyId) -> BodyState;
fn body_apply_impulse(id: BodyId, j: Vec3);
fn body_set_layer(id: BodyId, layer: u8, mask: u8);
fn body_set_sensor(id: BodyId, sensor: bool);
fn world_set_gravity(g: Vec3);

fn drain_collision_events(buf: *mut CollisionEvent, max: u32) -> u32;
```

Bodies do **not** mutate the world grid. Voxel destruction is cart-driven: the cart drains collision events and decides whether to call `set_voxel` / `fill_box` (§3) in response.

### 10.3 Layer 3 — Cellular automata

Per-material flags in the material table opt voxels into CA behavior:

| Flag | Behavior |
|---|---|
| `granular` | Falls down + diagonally; piles to angle of repose. (sand, gravel, snow) |
| `liquid` | Flows down then sideways; partial-fill cells visible to renderer. (water, oil, lava) |
| `gas` | Rises, disperses, decays. (smoke, steam) |
| `flammable` | Accumulates heat from adjacent fire; ignites at material-defined threshold. (wood, oil) |
| `fire` | Spreads to flammable neighbors; consumes them; finite lifetime. |

Multiple flags may combine where it makes sense (e.g., `flammable` + `liquid` → oil).

**Sparse active-set model.** CA state is *not* stored on the world grid. The simulator maintains:

```rust
HashMap<PackedPos, u8>   // active voxel position → 8 bits of CA state
```

When a voxel is mutated (cart write or Layer 3 itself), the simulator pushes that voxel and its 6/26 neighbors into the active set. Each tick, up to **N** entries are drained from the set, in the order specified below. For each, the CA rule for that voxel's material runs, the world grid may be mutated, and newly affected neighbors are reseeded into the set. Voxels that reach a stable state are evicted. This keeps the per-voxel cost on the world grid at zero — only voxels that are actually doing something pay anything.

**Drain order (deterministic):** the active set is processed in `(insertion_tick, morton_position)` order — earliest insertion first, position as the tiebreak when multiple voxels were inserted in the same tick. `morton_position` is the Morton-encoded packed `(x, y, z)`. This rule is identical on every port and is what makes replay determinism possible (§10.5).

**Per-active-voxel 8-bit state, decoded by material:**

| Material flag | State byte |
|---|---|
| `granular` | bit 0: just-moved (anti-jitter); bits 1–7: reserved |
| `liquid` | bits 0–3: fluid level (0–15); bits 4–7: flow direction hint |
| `gas` | bits 0–7: lifetime countdown |
| `flammable` | bits 0–7: accumulated heat (ignites at material-defined threshold) |
| `fire` | bits 0–3: temperature; bits 4–7: remaining life |

Total CA memory at full active-set cap: ~64 KB. The world model in §2 is unchanged — voxels remain 8 bits.

**Active-set caps (per port):**

| Target | Cap (voxels/frame) | Note |
|---|---|---|
| Browser | 32,768 | reference, generous |
| ESP32-P4 | 8,192 | ~25% of frame budget at full load |
| STM32H7 | 8,192 | similar to P4 |
| ESP32-S3 | 2,048 | tighter; carts can lower further |

Carts may lower their per-frame cap (`ca_set_budget(n)`) to reclaim CPU. Setting it to 0 disables Layer 3 entirely.

**Renderer integration.** For non-liquid materials, the renderer ignores the active set: voxel hit → material → palette, identical to static geometry. For materials with the `liquid` flag, when a primary ray hits a liquid voxel, the renderer does a single hashmap probe to read the fluid level and renders a sub-cell surface plane at that height. Cost is bounded by (rays-hitting-liquid × O(1) probes), well under 1% of frame budget in typical scenes.

**API:**
```rust
fn ca_set_budget(voxels_per_frame: u32);
fn ca_get_budget() -> u32;
fn ca_mark_active(pos: UVec3);                  // wake a voxel + neighbors
fn ca_active_count() -> u32;                    // for telemetry / debugging
fn ca_set_global_param(p: CaParam, value: f32); // angle of repose, viscosity, etc.
```

### 10.4 Layer 4 — Soft bodies / structural simulation (out of scope)

Teardown-style breakable voxel groupings with internal stress and break thresholds are **not planned** for voxlconsl, now or later. They would compromise the platform's CPU budget commitments and dwarf the rest of the engine in implementation cost. Carts that want destruction effects build them imperatively: detect the hit via Layer 1/2, decide what to remove, call `set_voxel` / `fill_box`, and seed Layer 3 for any ejecta.

### 10.5 Determinism and replay

- Layer 1 is deterministic by definition (pure functions of world state).
- Layer 2 is deterministic given fixed-step integration.
- Layer 3 is deterministic given the active-set drain order specified in §10.3 ("Drain order (deterministic)"): `(insertion_tick, morton_position)` ordering. The rule is identical on every port; replays produce identical output across ports.

Cart RNG (`rand()`) is host-seeded; carts may request a specific seed at boot.

**Replay format** records the *post-binding action stream* — the values cart code actually saw, not the underlying physical inputs. Per frame, the recorder serializes `(action_handle → value)` for every action whose value changed since the previous frame. Replays carry the cart name + spec version + RNG seed in their header; replay verification rebuilds the action stream and reproduces the session bit-for-bit.

This means recordings are portable across user rebindings, across ports (a recording made on browser plays back identically on hardware), and across input topologies (a recording made with a gamepad replays correctly on a touch-only device, since by replay time the action stream is what matters). The drawback is that recordings can't *replay-as-input*: a recording from a controller doesn't tell you which buttons the user pressed, only which actions fired. For voxlconsl's use cases (debugging, demo loops, competitive verification) this is the right trade.

---

## 11. Actors

Actors are the unit of "thing that moves" — anything not part of the static 512³ scene voxel grid. Player, enemies, projectiles, doors, vehicles, particles, decorative props.

### 11.1 Model

Each actor carries:

| Field | Type | Notes |
|---|---|---|
| `id` | `ActorId` | Host-assigned, stable for the actor's lifetime within a session |
| `prefab` | `Option<PrefabId>` | If `Some`, volume is shared from prefab via copy-on-write |
| `volume` | volume buffer ref | Up to 32³ voxels; canonical, axis-aligned in actor-local space |
| `position` | `Vec3` | World space, continuous (not grid-snapped) |
| `yaw` | `f32` | Continuous radians; applied at render time, never baked |
| `orientation` | `Orientation` | One of 24 cube-symmetry orientations; baked into `volume` |
| `anchor` | `Vec3` | Origin within actor-local space (cart-defined; default = volume center) |
| `visible` | `bool` | Hide without despawning |
| `body` | `Option<BodyId>` | Optional rigid body, see §10.2 |

Actor volumes use the same material table as the world (§2). Material `0` is empty/transparent. CA flags (§10.3) apply identically — sand inside an actor falls just like sand in the world, subject to the same Layer 3 budget.

### 11.2 Caps and budgets

| | Limit |
|---|---|
| Actors | 256 per cart |
| Volume per actor | 32³ voxels (32 KB dense) |
| Resident actor volume RAM | 4 MB ceiling, host-enforced |
| Bodies (subset of actors) | 64 (see §10.2) |

Copy-on-write means typical carts use far less than the 4 MB ceiling; many actors instancing the same prefab share one buffer. The ceiling is a backstop against pathological carts and is enforced by failing further `actor_spawn*` calls when exceeded.

### 11.3 Rotation model

**Continuous yaw** (rotation around world Y axis): an `f32` that may change every frame. Applied per-ray at render time as a 2D rotation in the X-Z plane — no volume re-bake.

**24 fixed orientations** for pitch/roll: the rotational symmetries of a cube. Each orientation is uniquely specified by a pair of perpendicular signed world axes — which world direction the actor's local **+Y** (up) and **+Z** (forward) end up pointing. The remaining axis (right) is `up × fwd`. The 6 possible up-axes (±X, ±Y, ±Z) × 4 yaw rotations around each = 24 orientations, grouped into 6 stances:

```rust
pub enum Orientation {
    // up = +Y  (canonical / standing)
    Up = 0,           UpRot90,        UpRot180,        UpRot270,
    // up = -Y  (upside down)
    Down,             DownRot90,      DownRot180,      DownRot270,
    // up = +X  (lying on left side, right-side-up world-east)
    EastUp,           EastUpRot90,    EastUpRot180,    EastUpRot270,
    // up = -X
    WestUp,           WestUpRot90,    WestUpRot180,    WestUpRot270,
    // up = +Z  (lying on back, head world-north)
    NorthUp,          NorthUpRot90,   NorthUpRot180,   NorthUpRot270,
    // up = -Z
    SouthUp,          SouthUpRot90,   SouthUpRot180,   SouthUpRot270,
}
```

`RotN` denotes N degrees of CCW rotation about the stance's up-axis (right-hand rule), starting from the stance's identity forward. `Up` is the identity (`up = +Y`, `forward = +Z`).

`Orientation` is **baked into the volume buffer** at spawn or on `actor_set_orientation` (§11.5). After baking, the volume is axis-aligned in actor-local space; rendering treats it identically to the `Up` case. The bake is a signed axis permutation — no trigonometry — so non-cubic source extents permute (e.g., a 5×7×3 source baked to `EastUp` becomes 7×3×5).

Result: smooth turning is free, "tipped over barrel / wall-mounted sign / sideways door" cost a one-time bake, and there is no continuous tumbling. Carts that want a tumbling effect should fake it via yaw + occasional orientation flips, or split the visual across multiple actors.

### 11.4 Prefabs

Prefabs are prebuilt voxel volumes stored in the cart's World section (§7), addressable by `PrefabId`. Multiple actors can instance the same prefab; the host shares the volume buffer between them via copy-on-write.

```rust
fn actor_spawn_from(prefab_id: u16, orientation: Orientation) -> ActorId;
```

The CoW fork happens on first mutation of an instanced actor:
- `actor_set_voxel`, `actor_fill_box`, `actor_clear`, `actor_load_volume` — fork before edit.
- `actor_set_orientation(id, ori)` to a non-current orientation — fork and re-bake.
- `actor_set_position`, `actor_set_yaw`, `actor_set_visible`, `actor_set_anchor` — never fork (transform-only).

Cart authors don't see the fork directly; the volume editing API "just works."

Prefab subsection layout in the cart's World section is specified in §13.6 (`PrefabEntry` table + shared `chunk_blobs` area).

> **v0.0.6 implementation note.** The cart format (§7) is not yet implemented, so prefabs are populated via a temporary host import `prefab_define(prefab_id, ptr, len, sx, sy, sz)` that the cart calls from `init`. The runtime API surface (`actor_spawn_from`, `actor_set_prefab`, `actor_set_orientation`) is unchanged. Once the §7 cart format lands, prefab data loads from the World section before `init` runs and `prefab_define` becomes optional.

### 11.5 Bake triggers

The host bakes the actor's volume buffer in exactly these cases:

| Trigger | Cost (32³ actor) | RAM impact |
|---|---|---|
| `actor_spawn_from(prefab, Up)` | none until first edit | none until edit |
| `actor_spawn_from(prefab, non-Up)` | one rotation bake | one volume buffer allocated |
| `actor_set_orientation(id, ori)` | one rotation bake | already allocated |
| First mutation of a prefab-shared actor | memcpy + edit | one volume buffer allocated |

Re-bake cost ≈ 32K voxel reads + writes:
- Browser: trivial.
- ESP32-P4: ~0.1–0.5 ms.
- ESP32-S3: ~1–2 ms.
- STM32H7: similar to P4.

There is no API path that triggers a bake implicitly per frame. Carts treat orientation changes as occasional events.

### 11.6 Renderer integration

Each frame, before ray-marching:

1. The host computes each visible actor's world-space AABB from `position`, `yaw`, `orientation`, `anchor`, and `volume` bounds.
2. Actors are binned into a coarse macro-grid: 16³ macro-cells across the active scene's 512³ world, each macro-cell spanning 32 world voxels (one macro-cell per chunk). Each cell maintains a list of overlapping actor IDs.
3. During DDA traversal, when a ray enters a macro-cell, it iterates that cell's actor list — for each candidate, AABB intersect first; on hit, transform the ray into actor-local space (subtract `position`, apply inverse `yaw`) and DDA the actor's volume buffer.

The closest hit (world or actor) wins. Lighting uses the world's directional light + ambient identically for both — actors and world are visually one space.

Yaw bloats the actor's world-space AABB by at most ~1.41× on the X/Z axes (sqrt(2)). The 32-voxel macro-grid cell size absorbs this with no observable broad-phase cost.

#### 11.6.1 Render modes

Every actor carries an `ActorRenderMode` enum that selects which compositor path the renderer uses for it:

- **`Worldspace`** *(default)*. The 3D ray-march path described above. `position` is a world coord; `yaw`/`orientation` apply normally.
- **`Billboard`**. The actor is anchored to a world position but rendered as a 2D sprite blit aligned to the camera. After the world ray-march finishes, the host projects `actor.position` through the camera basis to a framebuffer pixel and blits the actor's voxel grid **centered** on that point (1 voxel = 1 pixel; local `+X` → screen-right, local `+Y` → screen-up). Anchors behind the camera are skipped. Air voxels are transparent. v0.1 has no depth-test — billboards always sit on top of the world.
- **`Screen`**. Pure 2D UI: `position.(x, y)` are framebuffer pixel coords of the rect's **upper-left** corner; `position.z` is the layer (lower z paints first → higher z overwrites). The actor's voxel grid blits 1:1 to that rect with the same axis mapping as Billboard mode.

**Pass order:** world ray-march → Billboard composite → Screen composite. Within each composite, ties are broken by spawn order; Screen actors also sort by `position.z` ascending so the cart can stack UI layers.

**Prefab size limit (v0.1):** because Billboard/Screen actors still live in the actor table and their volume is built via the same prefab → SVO path, each axis is capped at 32 voxels (one SVO chunk). Wider panels are composed from multiple Screen actors. The SVO is unused for non-Worldspace actors but the cap remains for now to avoid forking the actor representation.

### 11.7 API

```rust
// Lifecycle
fn actor_spawn() -> ActorId;
fn actor_spawn_from(prefab_id: u16, orientation: Orientation) -> ActorId;
fn actor_despawn(id: ActorId);
fn actor_count() -> u32;

// Transform
fn actor_set_position(id: ActorId, pos: Vec3);
fn actor_get_position(id: ActorId) -> Vec3;
fn actor_set_yaw(id: ActorId, yaw: f32);
fn actor_get_yaw(id: ActorId) -> f32;
fn actor_set_orientation(id: ActorId, ori: Orientation);  // may re-bake
fn actor_get_orientation(id: ActorId) -> Orientation;
fn actor_set_anchor(id: ActorId, anchor: Vec3);
fn actor_set_visible(id: ActorId, visible: bool);
fn actor_get_bounds(id: ActorId) -> (Vec3, Vec3);  // world-space AABB
fn actor_set_render_mode(id: ActorId, mode: ActorRenderMode) -> bool;  // §11.6.1
fn actor_get_render_mode(id: ActorId) -> ActorRenderMode;

// Prefab swap — the basis of flipbook animation (§11.9)
fn actor_set_prefab(id: ActorId, prefab: PrefabId);  // swaps the actor's volume reference

// Volume editing — forks if currently prefab-shared
fn actor_set_voxel(id: ActorId, pos: U8Vec3, material: u8);
fn actor_fill_box(id: ActorId, min: U8Vec3, max: U8Vec3, material: u8);
fn actor_clear(id: ActorId);
fn actor_load_volume(id: ActorId, src: *const u8, len: u32);   // src points to a `ChunkData` blob (§13.2) of any size up to 32³

// Volume introspection
fn actor_volume_size(id: ActorId) -> U8Vec3;
fn actor_get_voxel(id: ActorId, pos: U8Vec3) -> u8;
```

### 11.8 Lifecycle and persistence

Actors are not persisted across cart boots. Carts wanting stateful actors (dropped items in a sandbox, NPC positions in an RPG) serialize relevant state into the save block (§7) and re-spawn at boot.

Actor IDs are stable for the actor's lifetime within a session. After despawn, an ID may be recycled.

Nested / parented actors (parent-relative transforms, hierarchical spawns) are **not** in v1 — they create surprisingly large surface area for what they buy. Carts compose multi-part entities by tracking parent transforms in cart code and updating child positions each tick.

### 11.9 Animation

Carts animate actors via **flipbook prefab-swap**: the actor's `prefab` field cycles through a list of `PrefabId`s over time. Each "frame" of an animation is a separate prefab in the cart's prefab table — e.g., `dude_walk_0`, `dude_walk_1`, `dude_walk_2`, `dude_walk_3` are four distinct authored volumes, played in sequence.

**Why flipbook (and not skeletal):**

- Voxel grids read badly at non-90° rotations — sub-voxel-aligned voxels look broken rather than smooth. Continuous bone rotation isn't a good fit for the medium.
- The CoW prefab system (§11.4) makes prefab-swap effectively free at runtime: every prefab is baked exactly once per `(prefab, orientation)` pair across the whole cart, and any actor playing the animation just rotates a pointer reference through the baked-volume cache. Twenty walking dudes share four baked volumes.
- Flipbook's per-frame authoring matches the voxel/pixel-art tradition where animation is a sequence of discrete poses.

**Authoring** is the same workflow as any other prefabs (§12.6.1):

```toml
# cart.toml
[prefabs]
dude_idle    = "world/prefabs/dude_idle.vxv"
dude_walk_0  = "world/prefabs/dude_walk_0.vxv"
dude_walk_1  = "world/prefabs/dude_walk_1.vxv"
dude_walk_2  = "world/prefabs/dude_walk_2.vxv"
dude_walk_3  = "world/prefabs/dude_walk_3.vxv"
```

Each name maps to a `PrefabId` constant the SDK exposes at bundle time.

**Runtime** uses one host import (`actor_set_prefab`, §11.7) plus a cart-side timing helper. The helper lives in `voxlconsl-sdk::animation::Flipbook`; the host has no animation-specific state. The platform never tracks "what animation is this actor playing" — it just receives prefab swaps.

The `Flipbook` helper API:

```rust
pub struct Flipbook { /* ... */ }

impl Flipbook {
    /// Build a clip from a list of prefab IDs and a uniform per-frame duration.
    pub const fn new(frames: &'static [PrefabId], frame_duration_ms: u32, looping: bool) -> Self;

    /// Advance the playhead by `dt_ms`. Should be called once per frame
    /// from `update`.
    pub fn tick(&mut self, dt_ms: u32);

    /// The prefab ID of the current frame. Pass to `actor_set_prefab`.
    pub fn current(&self) -> PrefabId;
    pub fn current_frame(&self) -> usize;

    pub fn reset(&mut self);
    pub fn is_done(&self) -> bool;        // for non-looping clips

    /// Edge: true on the tick the playhead just landed on `frame`.
    /// Useful for triggering frame-synced events (footstep SFX, hit-frame
    /// damage application, etc.).
    pub fn just_entered_frame(&self, frame: usize) -> bool;
}
```

**Cart-side usage:**

```rust
static mut WALK: Flipbook = Flipbook::new(
    &[WALK_0, WALK_1, WALK_2, WALK_3],
    120,
    true,
);

fn update(dt_ms: u32) {
    let (mx, my) = input_action_axis2d(MOVE);
    let moving = mx.abs() > 0.1 || my.abs() > 0.1;

    let clip = unsafe { &mut WALK };
    if moving { clip.tick(dt_ms); } else { clip.reset(); }

    let prefab = if moving { clip.current() } else { IDLE };
    actor_set_prefab(player_actor, prefab);

    if moving && clip.just_entered_frame(0) {
        sfx_play(FOOTSTEP_LEFT, /* ... */);
    } else if moving && clip.just_entered_frame(2) {
        sfx_play(FOOTSTEP_RIGHT, /* ... */);
    }
}
```

**Memory cost:** the only per-actor cost is the `ActorId` and its existing transform fields. Animation state (elapsed time, frame index) lives on the cart side in cart memory — the host knows nothing about it. Multiple actors playing the same `Flipbook` can each carry their own copy if their playback is desynchronized, or share a single one if they're locked together.

**Skeletal animation** (parted actors with parent-relative bone transforms, smooth bone rotation, blendable clips) is parking-lotted to v2. It would require continuous sub-voxel rotation of voxel sub-volumes, which doesn't read well in the medium without aggressive snapping — at which point you're authoring discrete poses anyway, which is what flipbook already does.

### 11.10 Text rendering

Text in voxlconsl is voxels. There is no glyph rasterizer in the host, no overlay layer, no 2D framebuffer — letters are voxels carved into the world or into actor volumes, lit and ray-marched by the same pipeline as everything else. This matches §3.5's note that dialog text is "cart-rendered into actor volumes positioned in front of the camera — text is voxels, like everything else."

The SDK ships `voxlconsl_sdk::text`, a pure cart-side helper that consumes `.vfnt` fonts (§12.7) and paints voxel text into either world voxels or a caller-provided dense buffer. The host is unaware of fonts or text — it just receives `set_voxel` calls or has a `prefab_define` data buffer handed to it.

**Two paint paths:**

```rust
pub enum Axis { XY, XZ, YZ }  // which plane the 2D glyph lives in;
                              // the perpendicular axis is the extrusion direction

pub static FONT_ANSI: Font<'static>;  // 10×11 ASCII (codepoints 32–126)
pub static FONT_DCP1: Font<'static>;  // 16×18 ASCII (codepoints 32–126)

impl<'a> Font<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, FontError>;
    pub fn cell_width(&self) -> u8;
    pub fn cell_height(&self) -> u8;
    pub fn glyph_count(&self) -> u16;
}

/// Paint text into world voxels via `set_voxel`. For permanent signs.
pub fn paint_world(
    font: &Font,
    origin: UVec3,
    axis: Axis,
    color: u8,
    face_color: Option<u8>,
    scale: u8,
    depth: u32,
    s: &str,
);

/// Rasterize text into a caller-provided dense buffer. The cart can hand
/// the buffer to `prefab_define` and spawn an actor — the basis of HUD,
/// dialog boxes, and billboardable signage.
pub fn rasterize_into(
    font: &Font,
    buf: &mut [u8],
    buf_size: U8Vec3,
    axis: Axis,
    color: u8,
    face_color: Option<u8>,
    scale: u8,
    depth: u32,
    s: &str,
) -> U8Vec3;  // extents written

/// Compute extents (cell_w*scale*chars × cell_h*scale × depth) for layout.
pub fn measure(font: &Font, scale: u8, depth: u32, s: &str) -> U8Vec3;
```

**Depth and face_color.** The `.vfnt` format itself is strictly 2D — every glyph is a flat bitmap. The 3D shape comes at paint time:

- **`depth: u32`** repeats the 2D glyph `depth` slices along the axis perpendicular to the chosen plane. `depth = 1` is a thin slab; `depth = 8+` is a chunky 3D sign.
- **`face_color: Option<u8>`** paints the first slice (the slice at the lower coord on the extrusion axis) with material `m`; the remaining `depth - 1` slices use `color`. Lets carts author glowing fronts on wooden signs, RUBY-faced letters carved into stone, etc., without authoring 3D fonts. Pass `None` for a uniform color across all slices.

Carts that want the face on the *back* swap front/back materials and adjust `origin` accordingly; the helper deliberately picks one convention rather than exposing every flip.

**`scale`** is a per-axis voxel multiplier in the painted plane. `scale = 1` paints one voxel per glyph bit; `scale = 3` paints a 3×3 voxel block per bit. `depth` is independent of `scale`.

**Layout.** Glyphs are placed left-to-right with no inter-letter gap (the font's cell already includes its own padding). Multi-line layouts are cart-side: split on `\n`, advance origin by `cell_h * scale + line_spacing` per line, call `paint_world`/`rasterize_into` per line.

**Why both paint paths.** `paint_world` covers permanent text (signs in the world, carved-stone messages, world-as-UI) where the text shouldn't follow the camera. `rasterize_into` covers dynamic text (dialog boxes, HUD, score counters, billboards) where the cart wants to spawn the text as an actor it can position, hide, animate, or yaw to face the camera. Forcing one to emulate the other costs either a per-frame world-rewrite or a per-sign actor.

**Deferred to a later session.** Variable-width / proportional glyphs (flag bit reserved), anti-aliased / multi-color glyphs (flag bit reserved), built-in 8×8 font, billboard helper (carts can `actor_set_yaw` themselves), dialog-box / panel helper, PNG-to-`.vfnt` and TTF-to-`.vfnt` build-time converters.

---

## 12. Authoring & toolchain

This section describes how cart authors produce `.voxl` files. None of it affects the runtime — the host loads `.voxl` cartridges directly, and the toolchain is the path from human-authored source files to a bundled cart.

### 12.1 Project layout

A cart project is a directory tree:

```
my-game/
├── cart.toml                 # manifest
├── Cargo.toml
├── src/lib.rs                # WASM entry — init / update / render
├── materials.toml            # 256 material entries
├── world/
│   ├── overworld.vxv         # native voxel volumes (or .vox via importer)
│   └── prefabs/
│       ├── tree.vxv
│       ├── enemy.vxv
│       └── door.vxv
└── audio/
    ├── patches.toml
    ├── songs/*.mid
    └── samples/*.wav
```

### 12.2 The `.vxv` format (canonical voxel volume)

`.vxv` is voxlconsl's native voxel-volume format, used for world chunks, actor prefabs, and any other voxel data the bundler embeds. Foreign formats (MagicaVoxel `.vox`, Qubicle, etc.) are handled via importers that emit `.vxv` as their output. The platform never depends on a foreign format.

**Header (16 bytes, all little-endian):**

```
Offset  Field          Size    Notes
------  -------------  ------  --------------------------------------------------------------
0       magic          4 B     "VXV1" (0x56 0x58 0x56 0x31)
4       version        u8      format version (currently 1)
5       flags          u8      bit 0: has_anchor; bit 1: has_markers; bit 2: has_ca_seeds
6       encoding       u8      0=dense, 1=rle, 2=sparse_list, 3=svo
7       reserved       u8      0
8       size_x         u16     extents in voxels, max 512
10      size_y         u16     max 512
12      size_z         u16     max 512
14      voxel_count    u16     informational (saturates at 0xFFFF); voxel data is authoritative
```

**Optional sections (in order, present only when their flag bit is set):**

```
1. Anchor (has_anchor):
     anchor_x, anchor_y, anchor_z   (i16 × 3)
     Origin point in voxel-local coords. Defaults to volume center if absent.

2. Markers (has_markers):
     marker_count                   u8
     For each marker:
        name_len                    u8
        name                        u8 × name_len   (UTF-8, no null terminator)
        x, y, z                     u16 × 3
        material                    u8              (the material at this position
                                                     before the marker overlay was applied;
                                                     useful for editor "ghost" rendering)

3. CA seeds (has_ca_seeds):
     bitmap of (size_x × size_y × size_z) bits, row-major (x fastest, then y, then z).
     Each set bit means "this voxel starts in the Layer 3 active set" (§10.3).
     Used for scenes that begin mid-simulation (sand starts falling, fire is already burning).
```

**Voxel data (encoding-dependent):**

| Encoding | Layout |
|---|---|
| 0 — Dense | `size_x × size_y × size_z` bytes, row-major. Each byte = material index; 0 = air. |
| 1 — RLE | Sequence of `(count: u16, material: u8)` runs traversed in dense row-major order. Sum of counts equals the voxel volume. |
| 2 — Sparse list | `count: u32`, then `count` records of `(x: u16, y: u16, z: u16, material: u8)`. Unlisted voxels are air. |
| 3 — SVO | The runtime sparse voxel octree encoding (full spec in §13). Used for content that maps 1:1 onto resident world chunks at load time. |

The bundler chooses encoding based on data shape: dense for tiny prefabs, RLE for blocky terrain, sparse list for sparse-arbitrary, SVO for content destined directly for world chunks. Importers may pick whichever encoding fits their source format's natural representation; the bundler may re-encode at section-assembly time (e.g., world-section content is always re-encoded into the SVO form §7 describes).

**Trailer (4 bytes):**

```
crc32   u32   CRC-32 of all preceding bytes (header through voxel data, inclusive)
```

### 12.3 Importers

Foreign voxel formats are converted to `.vxv` by importers. v1 ships with one:

- **MagicaVoxel `.vox`** — requires a per-file `colors.toml` mapping each `.vox` palette index to a voxlconsl material index. The importer flips coordinates from Z-up to Y-up. Multi-model `.vox` scenes flatten into a single `.vxv` (or split, with `--split-models`).

```
voxlconsl import scene.vox -o scene.vxv --colors colors.toml
```

The bundler also accepts `.vox` paths directly in `cart.toml`, in which case it runs the importer transparently during `voxlconsl bundle`.

Future importers parking-lotted: `.qb` (Qubicle), `.gox` (Goxel), PNG slice stacks, programmatic generators (Rust scripts emitting `.vxv` on stdout).

### 12.4 The `voxlconsl` CLI

A single binary handles project tooling:

```
voxlconsl new my-game            # scaffold a starter template
voxlconsl bundle [path]          # build .wasm, gather assets, write my-game.voxl
voxlconsl run my-game.voxl       # launch in the desktop reference host
voxlconsl serve [path]           # dev mode: rebuild & reload in browser on file change
voxlconsl validate my-game.voxl  # lint, check section sizes, palette references, etc.
voxlconsl import IN -o OUT       # convert foreign formats to .vxv
```

`voxlconsl bundle` invokes `cargo build --target wasm32-unknown-unknown --release` itself by default; carts that pre-build can specify a path in `cart.toml`.

Distribution: `cargo install voxlconsl` initially; prebuilt binaries on releases.

### 12.5 SDK crate

`voxlconsl-sdk` is the Rust crate carts depend on. It contains:

- `extern "C"` bindings for every host function in §3, §5, §6, §10, §11.
- Strongly-typed Rust wrappers (`Material`, `ActionDecl`, `Projection`, `Orientation`, etc.).
- Optional camera helper modules (`camera::OrbitCamera`, etc., per §3.2). Opt-in; LLVM strips unused helpers from the final WASM.
- `#[cart_entry]` macros so cart authors don't write WASM exports by hand.

Other languages can bind manually against the host ABI. Official non-Rust SDKs are out of v1 scope.

### 12.6 Configuration file schemas

All cart-side config files are TOML. The bundler parses them and produces the binary cart sections described in §7.

#### 12.6.1 `cart.toml` — the manifest

The single source-of-truth for what's in a cart.

```toml
[cart]
name          = "voxel-game"          # short identifier; lowercase ASCII, used as cart filename and key
title         = "Voxel Game: The Adventure"
author        = "Studio Studio"
version       = "0.1.0"               # cart version, semver
spec_version  = "0.1"                 # voxlconsl spec version targeted
description   = "An adventure in voxels."
license       = "MIT"                 # optional, free-form

# Optional: small voxel volume shown in the platform's cart picker
[cart.icon]
volume = "icon.vxv"                   # ≤ 16³ recommended

[code]
# Either build the WASM, or point to a pre-built .wasm
build  = "cargo build --target wasm32-unknown-unknown --release"
output = "target/wasm32-unknown-unknown/release/voxel_game.wasm"
# Alternative form:
# wasm = "path/to/prebuilt.wasm"

[materials]
file = "materials.toml"

[world]
# Voxel volumes composing the static world.
# Each entry places a .vxv at a specific world position (origin = corner of the volume).
chunks = [
    { source = "world/overworld.vxv", at = [0,   0, 0  ] },
    { source = "world/cave.vxv",      at = [256, 0, 256] },
]
# Optional: .vox imports run through the importer at bundle time
vox_imports = [
    { source = "world/raw.vox", colors = "world/colors.toml", at = [512, 0, 0] },
]

[prefabs]
# Named volumes referenced by `actor_spawn_from(prefab_id, …)`.
# Names map to PrefabId values at bundle time, exposed to the cart as constants in the SDK.
tree  = "world/prefabs/tree.vxv"
enemy = "world/prefabs/enemy.vxv"
door  = "world/prefabs/door.vxv"

[audio]
patches = "audio/patches.toml"
songs   = "audio/songs"               # directory; all .mid files included, sorted by filename → song slot 0..n
samples = "audio/samples"             # directory; all .wav files included, sorted by filename → sample slot 0..n

[save]
schema  = "save.toml"                 # optional; declares persistent state shape
size_kb = 16                          # ≤ 64 KB per §7

[settings]
# Cart-level startup hints (optional; cart code can override per frame)
default_view_distance = 256
default_fog           = { palette = "deep_blue:1", start = 64, end = 256 }
```

Bundler validates: required keys present, paths resolve, file extensions match expected formats, declared sizes don't blow §7 budgets.

#### 12.6.2 `materials.toml` — 256 material entries

Each entry corresponds to one slot in the binary material table (§2). Slot 0 is reserved for air; the bundler refuses to write a non-default entry to slot 0.

```toml
# Slots not listed default to all-zero (which means "air-like" — material exists but is empty).
# Carts typically populate slots 1..N where N is the count of distinct materials they use.

[[material]]
slot     = 1
name     = "stone"                    # used by editor / debug tooling; not embedded in cart
color    = "cool_gray:1"              # ramp:shade syntax (see §4.2 for ramp names) OR raw index 0-63
emission = 0
flags    = []                         # default: opaque solid

[[material]]
slot     = 2
name     = "wood"
color    = "brown:1"
flags    = ["flammable"]
ca_threshold = 60                     # heat units to ignite
ca_lifetime  = 120                    # frames once ignited

[[material]]
slot  = 3
name  = "fire"
color = "orange:3"
emission = 12
flags    = ["fire"]
ca_lifetime = 20                      # frames before burning out

[[material]]
slot  = 4
name  = "sand"
color = "tan:2"
flags = ["granular"]
ca_viscosity = 4                      # angle-of-repose tuning

[[material]]
slot  = 5
name  = "water"
color = "sky_blue:1"
flags = ["liquid", "transparent"]
ca_viscosity = 8                      # flow rate

[[material]]
slot  = 6
name  = "oil"
color = "yellow:0"
flags = ["liquid", "flammable", "transparent"]
ca_viscosity = 6
ca_threshold = 30                     # ignites easier than wood
```

**Color reference forms:**

- `"<ramp_name>:<shade>"` where `ramp_name` is one of: `brown`, `tan`, `forest_green`, `grass_green`, `teal`, `cyan`, `sky_blue`, `deep_blue`, `purple`, `pink`, `red`, `orange`, `yellow`, `magenta`, `cool_gray`, `warm_gray` — and shade is `0`–`3`.
- Raw integer `0`–`63` for the packed `(ramp << 2) | shade` index.

**Flags vocabulary:** the bitfield names from §2 — `transparent`, `glossy`, `granular`, `liquid`, `gas`, `flammable`, `fire`. Combinations are allowed where the spec permits (e.g., `["liquid", "flammable"]` for oil).

#### 12.6.3 `patches.toml` — synth and sampler patches

Up to 16 entries, indexed by `slot` (0–15). Each patch is either `kind = "synth"` or `kind = "sampler"`.

```toml
# Synth patch — subtractive engine
[[patch]]
slot = 0
name = "lead"
kind = "synth"

[patch.osc1]
mode         = "saw"                  # sine | saw | square_pwm | triangle | noise | fm2op
detune_cents = 0
octave       = 0
level        = 100                    # 0-127

[patch.osc2]
mode         = "square_pwm"
detune_cents = 7
octave       = 0
level        = 80
duty         = 0.4                    # PWM duty cycle, only for square_pwm

# fm2op extras (only if either osc.mode = "fm2op")
# [patch.fm]
# ratio = 2.0
# index = 1.5

[patch.filter]
mode      = "lp"                      # lp | hp | bp | off
cutoff    = 8000                      # Hz at default; LFO/env modulate
resonance = 30                        # 0-127

[patch.amp_env]
attack_ms  = 5
decay_ms   = 200
sustain    = 100                      # 0-127
release_ms = 100

[patch.filter_env]
attack_ms  = 0
decay_ms   = 300
sustain    = 0
release_ms = 200
depth      = 60                       # signed -127..127

[patch.lfo]
rate_hz = 5.5
shape   = "sine"                      # sine | tri | square | sh
target  = "filter"                    # pitch | filter | amp | pan
depth   = 30                          # signed

[patch.glide]
ms = 0


# Sampler patch — drum kit on channel 10 (default GM mapping)
[[patch]]
slot = 1
name = "drums"
kind = "sampler"

[[patch.zone]]
sample    = "kick"                    # filename without extension, found in audio/samples/
low_note  = 36
high_note = 36
root_note = 36
volume    = 100

[[patch.zone]]
sample    = "snare"
low_note  = 38
high_note = 38
root_note = 38

[[patch.zone]]
sample    = "hihat"
low_note  = 42
high_note = 46                        # any of these notes plays this sample
root_note = 42

[patch.filter]
mode = "off"

[patch.amp_env]
attack_ms  = 0
decay_ms   = 0
sustain    = 127
release_ms = 50


# Sampler patch — single-sample mode (one zone covering the whole keyboard)
[[patch]]
slot = 2
name = "voice"
kind = "sampler"

[[patch.zone]]
sample    = "vox_aaa"
low_note  = 0
high_note = 127
root_note = 60                        # original recording pitch
loop      = true                      # play sample's declared loop region
```

Sampler `zone.sample` references samples by filename (without `.wav`); the bundler resolves to slot indices at bundle time. Up to 8 zones per sampler patch.

#### 12.6.4 `colors.toml` — `.vox` import color mapping

Used by the `voxlconsl import` command (and transparently by the bundler when it encounters a `.vox` source) to map MagicaVoxel palette indices to voxlconsl material indices.

```toml
# MagicaVoxel palette is 1-indexed (1-255); index 0 is empty/transparent.

# Direct one-to-one assignments
1   = 1                                # vox color 1 → material 1
2   = 2
3   = 4
255 = 0                                # vox color 255 → empty/transparent

# Range syntax for batches (inclusive)
"5-10" = 5                             # vox colors 5..=10 all → material 5

# Default for unmapped colors (otherwise import errors)
default = 1

# Optional: marker colors. These vox voxels do NOT become world voxels in the .vxv;
# they emit named markers (per §12.2 markers) and the voxel position becomes air.
[markers]
"40" = { name = "spawn",   material = 0 }
"41" = { name = "trigger", material = 0 }
"42" = { name = "loot",    material = 0 }
```

The importer flips coordinates Z-up → Y-up automatically. Models with multiple shapes are flattened into a single `.vxv` by default; pass `--split-models` to emit one file per shape.

### 12.7 The `.vfnt` format (voxel font)

`.vfnt` is voxlconsl's native bitmap-font format. Carts use it via the cart-side text renderer (§11.10, `voxlconsl_sdk::text`) which extrudes the 2D glyphs through a third axis to paint voxel text into the world or into actor volumes. The format is fixed-width and 2D — runtime `scale` and `depth` parameters in the paint API cover the dynamic-size and 3D-extrusion intents, so the file stays small and authoring stays trivial.

The SDK ships two built-in fonts (both ASCII printable, codepoints 32–126) so simple carts can paint text without authoring a `.vfnt`:

- `FONT_ANSI` — 10×11, derived from the figlet "ANSI Regular" face. Clean blocky letterforms; a sensible default for HUD and dialog.
- `FONT_DCP1` — 16×18, derived from the figlet "Delta Corps Priest 1" face. Stylized chiseled-serif look; suits title signage and stone-carved messaging.

Carts that want custom typefaces ship `.vfnt` blobs alongside their other assets and parse them via `Font::from_bytes(&'static [u8])`. The repo includes `scripts/flf_to_vfnt.py` which converts figlet `.flf` source fonts into `.vfnt` blobs (auto-detecting whether the source uses `#`-style or unicode-half-block rendering).

**Header (16 bytes, all little-endian):**

```
Offset  Field          Size    Notes
------  -------------  ------  --------------------------------------------------------------
0       magic          4 B     "VFN1" (0x56 0x46 0x4E 0x31)
4       version        u8      format version (currently 1)
5       cell_w         u8      base glyph width in voxels (1..=64)
6       cell_h         u8      base glyph height in voxels (1..=64)
7       flags          u8      0 in v1; reserved bits: 0=variable-width, 1=anti-aliased
8       glyph_count    u16     number of glyphs in the index
10      reserved       u8 × 6  zero
```

**Glyph index** (immediately follows the header): `glyph_count` records of 8 bytes each, sorted ascending by codepoint:

```
codepoint   u32   Unicode scalar value
bitmap_off  u32   byte offset of this glyph's bitmap, measured from the start
                  of the bitmap section
```

**Bitmap section** (immediately follows the index): a flat byte sequence. Each glyph occupies `ceil(cell_w * cell_h / 8)` bytes containing `cell_w * cell_h` bits, MSB-first, laid out row-major (left-to-right, top-to-bottom). A set bit means "this voxel is part of the glyph"; a clear bit means "skip this voxel". Glyph bitmaps are tightly packed back-to-back; offsets in the index point to the first byte of each glyph and bits don't span glyph boundaries (each glyph rounds up to a whole byte).

**Sizing.** Multiple `.vfnt` files can ship at different cell sizes (5×7, 8×8, etc.) and a cart can hold several fonts simultaneously. There is no internal palette — color is supplied by the painter call, not the font.

### 12.8 Editor cart (roadmap)

Authoring `.vxv` in v1 means MagicaVoxel + importer, or a programmatic pipeline. A native voxlconsl world editor — built as a regular cart running on the platform — is an explicit roadmap goal, mirroring the synth-editor pattern from §5.7.

The editor cart is a deliberate early-stage project: it forces the platform to handle heavy volume editing, large voxel UIs, and host filesystem I/O for `.vxv` save/load — all of which exercises corners of the spec that gameplay carts won't. Once shipped, voxlconsl is self-hosting for asset creation.

**[OPEN — host API extensions needed for editor carts: filesystem read/write to author-side `.vxv` files, larger input bandwidth than gameplay carts, possibly a "tool cart" privilege class. v2 work.]**

---

## 13. Sparse voxel octree (SVO) format

The SVO is voxlconsl's canonical voxel storage format. It appears in three places:

- The cart's World section (compressed at the section level).
- Resident world chunks in RAM (uncompressed).
- `.vxv` encoding 3 (§12.2).
- Actor volume buffers, when the actor isn't using the simpler dense form.

One algorithm, four callers.

### 13.1 Two-tier structure

The world is organized in two levels:

1. **Chunk grid:** the 512³ per-scene world is tiled by a 16×16×16 grid of chunks, each chunk a 32³ volume of voxels. Empty chunks (entirely material 0 / air) are not stored.
2. **Per-chunk SVO:** each non-empty chunk is encoded as an SVO of depth 5 (32 = 2⁵).

The two-tier model is identical on disk and in RAM. It enables per-chunk streaming, LRU eviction of inactive chunks, and per-chunk modification without touching the rest of the world.

### 13.2 Chunk encoding

A chunk is either *uniform* (one material everywhere) or an *octree* (sparse subdivisions).

```
ChunkData {
    flags        : u8   // bit 0: is_uniform; bits 1-7: reserved
    material     : u8   // meaningful only when is_uniform = 1
    node_count   : u16  // count of u32 entries in `nodes`; 0 when uniform
    nodes        : [u32; node_count]   // octree nodes, present only when !uniform
}
```

- **Uniform-empty** chunks are *not stored* — they're absent from the chunk index entirely.
- **Uniform-non-empty** chunks (e.g., a sky chunk, a solid-stone chunk) are 4 bytes total: header + material.
- **Sparse** chunks store a flat array of 4-byte SVO nodes after the header.

### 13.3 Node format

Every node is 4 bytes (a single `u32` little-endian). The high bit discriminates leaf vs branch — the format is self-describing.

```
bit 31:        is_leaf

if is_leaf = 1   (Leaf):
    bits  0-7  : material         (material index for the leaf's whole region)
    bits  8-30 : reserved (= 0)

if is_leaf = 0   (Branch):
    bits  0-7  : valid_mask        (which of 8 octants have content)
    bits  8-23 : first_child       (index into chunk's nodes[] of the first child)
    bits 24-30 : reserved (= 0)
```

`nodes[0]` is the root and is always a Branch by convention. (A whole-chunk uniform value is encoded at the chunk level via `flags.is_uniform`, not as a root leaf.)

**Octant ordering.** Children are indexed 0–7 by `octant = (z << 2) | (y << 1) | x`:

```
0:  -X -Y -Z      4:  -X -Y +Z
1:  +X -Y -Z      5:  +X -Y +Z
2:  -X +Y -Z      6:  -X +Y +Z
3:  +X +Y -Z      7:  +X +Y +Z
```

**Child layout.** Children of a branch are stored contiguously starting at `first_child`. The number of children equals `valid_mask.count_ones()` (0–8). Octants whose bit is *clear* in `valid_mask` are *air* and have no entry. To find the child entry for octant `k`:

```rust
fn child_index(branch: Branch, k: u8) -> Option<u16> {
    if branch.valid_mask & (1 << k) == 0 {
        return None;  // octant is air
    }
    let lower = branch.valid_mask & ((1 << k) - 1);
    Some(branch.first_child + lower.count_ones() as u16)
}
```

`first_child` is 16 bits → up to 65,535 nodes per chunk. Theoretical worst-case node count for a fully-subdivided 32³ chunk (every voxel its own leaf) is ~37K, so this fits comfortably with margin.

### 13.4 Traversal

Standard stack-based front-to-back DDA. Pseudocode for a single ray:

```
push (root_node = 0, ray bounds [t_in, t_out], chunk-space AABB of root)
while stack not empty:
    pop (node_idx, t_in, t_out, aabb)
    if t_in > current_best: continue                        // already shadowed
    n = nodes[node_idx]
    if n.is_leaf:
        if n.material != 0:
            record hit (material, t_in, face normal)
            update current_best
        continue
    // branch: enumerate octants in front-to-back order based on ray direction
    for octant in front_to_back_order(ray.dir):
        if !(n.valid_mask & (1 << octant)): continue        // air
        let child_aabb = sub_aabb(aabb, octant)
        let (t_a, t_b) = intersect(ray, child_aabb)
        if t_a > t_b || t_b < 0: continue                    // miss
        let child = n.first_child + popcount(n.valid_mask & ((1 << octant) - 1))
        push (child, max(t_a, t_in), min(t_b, t_out), child_aabb)
```

Real implementations replace the recursive AABB recomputation with incremental tracking of `t_x`, `t_y`, `t_z` per Amanatides-Woo to skip the multiplications. The format permits any traversal that respects the node semantics; this pseudocode is a correctness reference, not a prescribed implementation.

### 13.5 Mutation

`set_voxel(x, y, z, material)`:

1. **Descend.** Walk the tree from the root, creating branches as needed for missing path segments. Each new branch is appended to `nodes[]`. Update parent's `valid_mask` and `first_child` (a parent may need to be rewritten if its child layout shifts; see "child shift" below).
2. **Set the leaf.** Append the new leaf (or update an existing one) at the deepest level.
3. **Collapse upward.** Walk back to the root: if a branch's children are all leaves of the same material *and* `valid_mask == 0xFF`, replace the branch with a single leaf of that material in the parent's child slot. Keep collapsing until no more reductions apply.

**Child shift.** Adding a child to a branch can change its `valid_mask`, which means existing children are still in `nodes[]` at the old contiguous location — the new child must be inserted in the right position to keep child order consistent with bit order in `valid_mask`. The simplest correct strategy is "rewrite": copy the branch's children to the end of the array with the new child inserted at the right offset, update the branch's `first_child`. Old child entries become orphaned.

**Compaction.** Orphaned nodes accumulate over time. v1 strategy: append-and-compact. The host runs a compaction pass when fragmentation exceeds a threshold (e.g., > 30% orphaned by node count) or on save. Compaction walks the tree and rewrites all nodes into a fresh contiguous array.

A real free-list-managed allocator can come later; v1 prioritizes simplicity.

### 13.6 World-level chunk indexing

Each scene (§3.7) holds its own 32³ chunk grid. The grid is sparse — most chunks don't exist in a typical scene.

**In RAM:** `HashMap<ChunkKey, ChunkData>` per scene, where `ChunkKey` is a packed 15-bit `(cx, cy, cz)` (5 bits per axis, since 32 = 2⁵). Fast lookup, simple eviction. The hashmap's value can be the `ChunkData` directly or a handle to a chunk pool (implementation detail). The reference browser host uses a dense `Vec<Option<Box<ChunkState>>>` of length 32 768 per scene to keep per-cell lookup at one cache-friendly array indexing — both shapes preserve the §13.6 semantics.

**On disk** (cart World section): a sorted index plus a blob area:

```
World section:
    prefab_count : u16
    prefab_table : PrefabEntry × prefab_count
    chunk_count  : u32
    chunk_index  : ChunkIndexEntry × chunk_count   (sorted by key, ascending)
    chunk_blobs  : raw byte stream of per-chunk ChunkData

ChunkIndexEntry {
    key    : u16   // bits 0-4: cx, bits 5-9: cy, bits 10-14: cz, bit 15: reserved
    offset : u32   // offset from start of chunk_blobs
    length : u32   // length in bytes (uncompressed ChunkData size)
}

PrefabEntry {
    name_len    : u8
    name        : u8 × name_len   (UTF-8, lowercase ASCII recommended; no null terminator)
    // pad to 4-byte alignment with zeros
    size_x, size_y, size_z : u8 × 3   (extents, each ≤ 32)
    reserved    : u8
    data_offset : u32   (offset into chunk_blobs)
    data_length : u32   (length in bytes)
}
```

Prefabs and chunks share the same blob area and the same `ChunkData` encoding. A prefab is just a (possibly small) chunk with its own extents in the prefab table; the renderer/actor system reads the extents to know the prefab's bounds rather than assuming 32³.

### 13.7 Compression

The cart's whole World section is wrapped in a compression scheme — recommended **zstd** for desktop/browser ports, **lz4** for memory-constrained MCU ports where decompression speed dominates over compression ratio. The cart format header (§7) carries a compression-tag byte for the World section so the runtime knows which decoder to invoke.

In RAM, chunks are stored in their unwrapped `ChunkData` form — uncompressed but already sparse. The SVO is the in-memory format; there is no separate "decoded" representation.

### 13.8 Memory budgets

Per chunk:

| Case | Size |
|---|---|
| Uniform-empty | not stored |
| Uniform-non-empty | 4 bytes |
| Typical outdoor terrain | 1–10 KB (~250–2,500 nodes) |
| Pathological worst case (every voxel a leaf) | ~150 KB (~37K nodes) |

Per-chunk RAM cost in the reference host: 32 KB dense buffer (always resident) + the SVO above, ≈ **40–60 KB per populated chunk** for typical content.

Per-scene fixed cost: 32 KB chunk slot table (4096 slots × 8 bytes) + active scene's chunks. Empty world: 2 KB scene table + 0.

**Realistic playable footprint by target.** A scene's *addressable* size is always 512³, but *populated* chunk count is what RAM constrains. Density guidance:

| Target | RAM (PSRAM) | Voxel-data budget | ≈ chunks resident | Realistic populated footprint |
|---|---|---|---|---|
| **STM32H7** | ~1 MB SRAM | ~256 KB | 4–8 chunks | 64×64 ground, no hills |
| **ESP32-S3** | 8 MB | ~5 MB | ~100 chunks | 256×256 ground + scattered decor |
| **ESP32-P4** (priority 1) | 32 MB | ~25 MB | ~500 chunks | 512×512 ground + hills + trees |
| Browser / desktop | gigabytes | unconstrained | thousands | 512³ fully populated if you want |

The ESP32-P4 row is the **design point** — the spec's 512³ ceiling is sized so a P4 cart can populate the full 512×512 ground with terrain detail, hills, and decoration within budget. Carts that target smaller MCUs author at correspondingly smaller densities; the SDK + tooling will surface a "doesn't fit on $TARGET" warning when the bundler can prove it (post v1).

### 13.9 Future work (parking lot)

- **Free-list-managed mutation pool** to replace append-and-compact.
- **Brick leaves** at depth 4 or 5: replace the bottom of the SVO with a small dense voxel buffer (e.g., an 8³ "brick"). Trades memory for traversal speed at the leaf level. Worth measuring before committing.
- **GPU-friendly linearization** for ports with GPUs (out of v1 scope).
- **Run-length leaf encoding** for cases where many adjacent voxels share a material — could compress sparse-but-streaky data better than the current per-leaf-byte form.

---

## 14. Open questions / parking lot

- [ ] Final palette RGB tuning against real voxel scenes (structure locked in §4; values are v0.1 anchor)
- [ ] Synth patch binary layout — exact byte offsets for synth and sampler kinds (TOML schema in §12.6.3 is authoritative; lock binary at SDK implementation)
- [ ] Reverb / delay algorithm choice and CPU budget on ESP32-S3
- [ ] Lighting v2 (AO, emissive bleed)
- [ ] Split-screen / multi-viewport rendering — v2; foundation laid via `camera_set_render_rect`
- [ ] Editor cart host APIs (filesystem, tool-cart privilege class) — v2
- [ ] Schema-driven save formats with versioned migration — v2
- [ ] Schema field validation specifics (e.g., `name` charset, slot duplicate handling, error reporting in `voxlconsl validate`)
- [ ] Multi-cart linking / shared libraries — probably "no" for v1 to keep the format simple
- [ ] Networking — out of scope for v1
- [ ] Touch overlay edge cases — dense action lists (>6 buttons), Axis1D layout, accessibility / left-handed mirroring
- [ ] Browser cart loader UX (drag-and-drop `.voxl`? URL param? built-in cart picker?)
- [ ] Replay file binary format — header layout, frame-delta encoding, compression
- [ ] Richer collision-layer model (16 or 32 layers) — v2
- [ ] CA platform-default tuning constants for `ca_threshold` / `ca_lifetime` / `ca_viscosity` (per-material override locked, defaults TBD by playtesting)

---

## 15. Versioning

This document is **v0.1**. The cart format will be versioned via the header byte; v0.x carts are not expected to load on later major versions until v1.0 freezes the format.
