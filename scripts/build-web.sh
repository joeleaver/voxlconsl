#!/usr/bin/env bash
# Build the browser host and wire up the wasm-bindgen JS shim.
#
# Usage:
#   ./scripts/build-web.sh                          # release, hello-cube
#   ./scripts/build-web.sh release big-world        # release, big-world cart
#   ./scripts/build-web.sh debug                    # debug, hello-cube
#   CART=big-world ./scripts/build-web.sh release   # env-var form
#
# Steps:
#   1. Build the example cart (wasm32, release).
#   2. Bundle it into a .voxl via `voxlconsl-cli bundle`, copying the
#      result to a stable location for `include_bytes!`.
#   3. Build the browser host (wasm32, release or debug).
#   4. Run wasm-bindgen on the host's .wasm into web/pkg.

set -euo pipefail

cd "$(dirname "$0")/.."

PROFILE="${1:-release}"

case "$PROFILE" in
    release)
        BUILD_FLAGS=(--release)
        OUT_DIR="target/wasm32-unknown-unknown/release"
        ;;
    debug)
        BUILD_FLAGS=()
        OUT_DIR="target/wasm32-unknown-unknown/debug"
        ;;
    *)
        echo "usage: $0 [release|debug] [cart-name]" >&2
        exit 1
        ;;
esac

# Cart name: positional second arg wins, then $CART env var, then default.
CART="${2:-${CART:-hello-cube}}"
EMBEDDED_VOXL="crates/host-browser/embedded-cart.voxl"

echo "[build-web] building cart: $CART ($PROFILE)..."
cargo build --target wasm32-unknown-unknown -p "$CART" "${BUILD_FLAGS[@]}"

# The bundler runs `cargo build --release` again from inside the cart
# directory per cart.toml's `[code].build` line. That's redundant when
# we just built above; the second invocation is a no-op rebuild that
# costs only the build-graph walk (~50 ms). Worth it for the
# spec-conformant pipeline.
echo "[build-web] bundling cart -> .voxl..."
cargo run --release -p voxlconsl-cli -- bundle "examples/$CART" \
    --output "$EMBEDDED_VOXL"
echo "[build-web] cart voxl: $(wc -c < "$EMBEDDED_VOXL") bytes -> $EMBEDDED_VOXL"

echo "[build-web] building voxlconsl-host-browser ($PROFILE)..."
cargo build --target wasm32-unknown-unknown -p voxlconsl-host-browser "${BUILD_FLAGS[@]}"

# Stage-4b audio-worklet wasm. Built every time alongside the host
# so the worklet's loaded blob can't drift out of sync with the
# voxlconsl-audio crate it wraps. Output is a raw .wasm (no wasm-
# bindgen) the AudioWorkletProcessor fetches and instantiates
# directly.
echo "[build-web] building voxlconsl-audio-worklet ($PROFILE)..."
cargo build --target wasm32-unknown-unknown -p voxlconsl-audio-worklet "${BUILD_FLAGS[@]}"
cp "$OUT_DIR/voxlconsl_audio_worklet.wasm" web/audio-worklet.wasm
echo "[build-web] worklet wasm: $(wc -c < web/audio-worklet.wasm) bytes -> web/audio-worklet.wasm"

echo "[build-web] running wasm-bindgen..."
wasm-bindgen \
    "$OUT_DIR/voxlconsl_host_browser.wasm" \
    --out-dir web/pkg \
    --target web \
    --no-typescript

echo "[build-web] host wasm:  $(wc -c < web/pkg/voxlconsl_host_browser_bg.wasm) bytes"
echo "[build-web] done. open web/index.html via a static server."
