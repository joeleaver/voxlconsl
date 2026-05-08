# Crate Index

Seven workspace crates plus one example cart. The full dependency graph
is in [Project Layout](project-layout.md).

## `voxlconsl-types`

Shared types used by every other crate. `no_std` + `libm` for math, with
optional `bytemuck` POD impls for zero-copy memcpy of materials, palette
colors, and vector types.

Core types: `Vec3`, `UVec3`, `IVec3`, `U8Vec3`, `Material`,
`MaterialFlags`, `Ramp`, `PaletteColor`, `ActionDecl`, `ActionHandle`,
`ActionKind`, `BindingHint`, `Projection`, `Orientation`, `BodyKind`,
`Shape`, `Hit`, `SweepHit`, `BodyState`, `CollisionEvent`, `CaParam`,
`PatchKind`, `OscMode`, `FilterMode`, `LfoShape`, `LfoTarget`,
`VoiceId`, `ActorId`, `PrefabId`, `ActorMask`.

[`crates/types/`](https://github.com/joeleaver/voxlconsl/tree/main/crates/types)

## `voxlconsl-svo`

Sparse voxel octree per [§13](spec.md#13-sparse-voxel-octree-svo-format).
`from_dense()` builder, front-to-back DFS raycast, unit tests for
node bit-layout and ray-intersection correctness.

[`crates/svo/`](https://github.com/joeleaver/voxlconsl/tree/main/crates/svo)

## `voxlconsl-host`

The runtime. Module layout mirrors the spec sections:

| Module | Spec section |
|---|---|
| `renderer` | §3 Rendering |
| `palette` | §4 Color |
| `audio` | §5 Audio |
| `input` | §6 Input |
| `physics` | §10 Physics |
| `actors` | §11 Actors |
| `world` | World state shared between cart mutation and the renderer |
| `sandbox` | wasmi-based cart loader |
| `runtime` | Cart lifecycle / per-frame loop driver |

[`crates/host/`](https://github.com/joeleaver/voxlconsl/tree/main/crates/host)

## `voxlconsl-host-browser`

The browser port (SPEC.md §9). Compiles to `wasm32-unknown-unknown` and
is loaded by `web/main.js` via wasm-bindgen. v0.0.x embeds the cart
binary via `include_bytes!`; once the §7 cart format lands the host
will load `.voxl` files dynamically.

[`crates/host-browser/`](https://github.com/joeleaver/voxlconsl/tree/main/crates/host-browser)

## `voxlconsl-sdk`

What carts depend on. `no_std`. Re-exports types from `voxlconsl-types`,
declares `extern "C"` host imports, and wraps them in safe Rust
functions per [§8.4](spec.md#84-wasm-abi-conventions).

[`crates/sdk/`](https://github.com/joeleaver/voxlconsl/tree/main/crates/sdk)

## `voxlconsl-bundler`

Skeleton. Will turn a cart project directory (cart.toml + materials.toml
+ patches.toml + .vxv files) into a `.voxl` cart binary per [§7](spec.md#7-cart-format-voxl).

[`crates/bundler/`](https://github.com/joeleaver/voxlconsl/tree/main/crates/bundler)

## `voxlconsl-cli`

Skeleton. Will be the `voxlconsl` binary with subcommands `new`,
`bundle`, `run`, `serve`, `validate`, `import` per [§12.4](spec.md#124-the-voxlconsl-cli).

[`crates/cli/`](https://github.com/joeleaver/voxlconsl/tree/main/crates/cli)

## `examples/hello-cube`

The first cart. Demonstrates the v0.0.6 SDK surface end-to-end:
materials, world geometry, prefabs (player + barrel), `actor_spawn_from`
at three orientations, and a `Flipbook`-driven walk cycle.

[`examples/hello-cube/`](https://github.com/joeleaver/voxlconsl/tree/main/examples/hello-cube)
