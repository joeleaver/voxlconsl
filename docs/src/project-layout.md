# Project Layout

Top-level directory tree:

```
voxlconsl/
├── SPEC.md                 # platform specification (v0.1)
├── Cargo.toml              # workspace
├── crates/                 # platform crates
│   ├── types/              # shared types: Vec3, Material, ActionDecl, ...
│   ├── svo/                # sparse voxel octree (§13 of the spec)
│   ├── host/               # runtime: renderer, palette, world, sandbox, input
│   ├── host-browser/       # browser port (wasm32 cdylib)
│   ├── sdk/                # cart-side crate (no_std)
│   ├── bundler/            # `.voxl` cart bundler (skeleton)
│   └── cli/                # `voxlconsl` binary (skeleton)
├── examples/
│   └── hello-cube/         # first cart — 3.5 KB no_std WASM
├── docs/                   # this site (mdBook)
├── web/                    # browser shell — index.html, main.js, style.css
└── scripts/
    └── build-web.sh        # build cart, build host, run wasm-bindgen
```

## Crate dependency graph

```
                           voxlconsl-types
                          /        |        \
                         /         |         \
                voxlconsl-svo  voxlconsl-sdk  voxlconsl-bundler
                       \                            /
                        \                          /
                         voxlconsl-host           /
                              |                  /
                              |                 /
              voxlconsl-host-browser    voxlconsl-cli
```

Carts depend on **`voxlconsl-sdk` only**. The SDK is `no_std` and re-exports
the shared types; cart authors never reach into the host.

The host crate depends on `voxlconsl-svo` and is consumed by both the
browser host (which compiles to WASM and is loaded by JS) and — eventually
— hardware ports.

## What's wasm32-only

| Crate | Native build | wasm32 build |
|---|---|---|
| `voxlconsl-types` | ✓ | ✓ |
| `voxlconsl-svo` | ✓ | ✓ |
| `voxlconsl-host` | ✓ | ✓ |
| `voxlconsl-bundler` | ✓ | — |
| `voxlconsl-cli` | ✓ | — |
| `voxlconsl-sdk` | ✓ (no_std) | ✓ (cart side) |
| `voxlconsl-host-browser` | — | ✓ (cdylib) |
| `examples/hello-cube` | — | ✓ (cdylib) |

The wasm32-only crates are excluded from `default-members` in the workspace
manifest, so plain `cargo build` / `cargo check` / `cargo test` from the
workspace root work without the wasm target installed.
