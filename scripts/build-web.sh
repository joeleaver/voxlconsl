#!/usr/bin/env bash
# Build the browser host and wire up the wasm-bindgen JS shim.
#
# Usage:
#   ./scripts/build-web.sh                          # release, hello-cube
#   ./scripts/build-web.sh release big-world        # release, big-world cart
#   ./scripts/build-web.sh release big-world,hello-cube
#                                                   # release, picker with both
#   ./scripts/build-web.sh debug                    # debug, hello-cube
#   CART=big-world ./scripts/build-web.sh release   # env-var form
#
# Steps:
#   1. Build each cart's wasm.
#   2. Bundle each to a .voxl, copy to web/carts/<name>.voxl for the
#      runtime picker, and the *first* cart in the list also lands in
#      crates/host-browser/embedded-cart.voxl as the no-args fallback.
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
        echo "usage: $0 [release|debug] [cart-name|cart-list]" >&2
        exit 1
        ;;
esac

# Cart spec: comma-separated list of cart names (e.g. "big-world,hello-cube").
# Positional second arg wins, then $CART env var, then a sensible default
# that ships both example carts in the picker.
CART_SPEC="${2:-${CART:-big-world,voxdude,hello-cube}}"
IFS=',' read -r -a CARTS <<< "$CART_SPEC"

EMBEDDED_VOXL="crates/host-browser/embedded-cart.voxl"
CARTS_DIR="web/carts"

mkdir -p "$CARTS_DIR"
# Prune stale .voxl from previous builds so the picker only lists what
# the current build actually produced.
find "$CARTS_DIR" -maxdepth 1 -name '*.voxl' -delete

for i in "${!CARTS[@]}"; do
    CART="${CARTS[$i]}"
    echo "[build-web] building cart: $CART ($PROFILE)..."
    cargo build --target wasm32-unknown-unknown -p "$CART" "${BUILD_FLAGS[@]}"

    # The bundler runs `cargo build --release` again from inside the cart
    # directory per cart.toml's `[code].build` line. That's redundant when
    # we just built above; the second invocation is a no-op rebuild that
    # costs only the build-graph walk (~50 ms). Worth it for the
    # spec-conformant pipeline.
    OUT_VOXL="$CARTS_DIR/$CART.voxl"
    echo "[build-web] bundling cart -> $OUT_VOXL..."
    cargo run --release -p voxlconsl-cli -- bundle "examples/$CART" \
        --output "$OUT_VOXL"
    echo "[build-web] cart voxl: $(wc -c < "$OUT_VOXL") bytes -> $OUT_VOXL"

    # First cart in the list is also the no-args / `include_bytes!`
    # fallback. The JS picker fetches /carts/<name>.voxl at runtime,
    # but anything that boots `BrowserHost::new()` with no args (CLI,
    # tests, ad-hoc native runners) gets this one.
    if [ "$i" -eq 0 ]; then
        cp "$OUT_VOXL" "$EMBEDDED_VOXL"
        echo "[build-web] embedded fallback -> $EMBEDDED_VOXL"
    fi
done

# Write a JSON manifest the JS picker enumerates at boot. Simpler than
# probing /carts/ via directory listing (which the dev server allows but
# GitHub Pages does not).
CARTS_JSON="$CARTS_DIR/index.json"
{
    echo "{"
    echo "  \"carts\": ["
    for i in "${!CARTS[@]}"; do
        if [ "$i" -gt 0 ]; then echo "    ,"; fi
        echo "    { \"name\": \"${CARTS[$i]}\", \"file\": \"${CARTS[$i]}.voxl\" }"
    done
    echo "  ]"
    echo "}"
} > "$CARTS_JSON"
echo "[build-web] picker manifest -> $CARTS_JSON"

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
