# voxlconsl

A fantasy console where the only graphics primitive is a voxel.

The platform spec is `SPEC.md` at the project root. Code in `crates/` implements that spec.

## Workspace

| Crate | Role |
|---|---|
| `voxlconsl-types` | Shared types (vectors, materials, actions, etc.) used by every other crate |
| `voxlconsl-svo` | Sparse voxel octree — the canonical voxel storage format (§13) |
| `voxlconsl-host` | Runtime: ray marcher, physics, audio, WASM-cart sandbox |
| `voxlconsl-sdk` | Cart-side crate. Cart authors depend on this |
| `voxlconsl-bundler` | Reads a cart project directory and produces a `.voxl` |
| `voxlconsl-cli` | The `voxlconsl` binary (`new`, `bundle`, `run`, `serve`, `validate`, `import`) |

## Status

Pre-alpha. The spec is mostly locked at v0.1; implementation is just starting.
