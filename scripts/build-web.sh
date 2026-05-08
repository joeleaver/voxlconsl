#!/usr/bin/env bash
# Build the browser host and wire up the wasm-bindgen JS shim.
#
# Steps:
#   1. Build the example cart (wasm32, release).
#   2. Copy the cart's .wasm into a stable location for `include_bytes!`.
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
        echo "usage: $0 [release|debug]" >&2
        exit 1
        ;;
esac

CART="${CART:-hello-cube}"
EMBEDDED_WASM="crates/host-browser/embedded-cart.wasm"

echo "[build-web] building cart: $CART ($PROFILE)..."
cargo build --target wasm32-unknown-unknown -p "$CART" "${BUILD_FLAGS[@]}"

CART_OUT="$OUT_DIR/${CART//-/_}.wasm"
cp "$CART_OUT" "$EMBEDDED_WASM"
echo "[build-web] cart wasm: $(wc -c < "$EMBEDDED_WASM") bytes -> $EMBEDDED_WASM"

echo "[build-web] building voxlconsl-host-browser ($PROFILE)..."
cargo build --target wasm32-unknown-unknown -p voxlconsl-host-browser "${BUILD_FLAGS[@]}"

echo "[build-web] running wasm-bindgen..."
wasm-bindgen \
    "$OUT_DIR/voxlconsl_host_browser.wasm" \
    --out-dir web/pkg \
    --target web \
    --no-typescript

echo "[build-web] host wasm:  $(wc -c < web/pkg/voxlconsl_host_browser_bg.wasm) bytes"
echo "[build-web] done. open web/index.html via a static server."
