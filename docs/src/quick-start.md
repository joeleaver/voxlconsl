# Quick Start

## Prerequisites

- **Rust** (stable, 1.85+) with the `wasm32-unknown-unknown` target:
  ```sh
  rustup target add wasm32-unknown-unknown
  ```
- **`wasm-bindgen-cli`**, version-pinned to match the `wasm-bindgen` dependency:
  ```sh
  cargo install wasm-bindgen-cli --version 0.2.100
  ```
- A static HTTP server. Anything works — `python3 -m http.server`,
  `npx serve`, etc.

## Clone and build

```sh
git clone https://github.com/joeleaver/voxlconsl
cd voxlconsl
./scripts/build-web.sh release
```

The build script:
1. Compiles the `hello-cube` cart for `wasm32-unknown-unknown`.
2. Copies the cart's `.wasm` to a stable path so the host can `include_bytes!` it.
3. Compiles the browser host crate.
4. Runs `wasm-bindgen` to produce JS bindings in `web/pkg/`.

## Run it

```sh
cd web && python3 -m http.server 8765
```

Open [http://localhost:8765/](http://localhost:8765/) and you should see
the same scene as the [embedded emulator](emulator.md).

## Run the tests

```sh
cargo test --workspace
```

The `voxlconsl-svo` crate has unit tests for the SVO node bit-layout,
chunk uniformity collapsing, single-voxel insertion, and ray-vs-chunk
intersection.

## Native check

`voxlconsl-host-browser` and `examples/hello-cube` are wasm32-only
cdylibs — they're excluded from the workspace's `default-members`, so a
plain `cargo build` from the workspace root works on Linux/macOS without
needing the wasm target installed.

To rebuild with debug-friendlier output (longer panic messages, no LTO):

```sh
./scripts/build-web.sh debug
```
