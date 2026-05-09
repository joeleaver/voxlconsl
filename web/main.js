// voxlconsl browser shim.
//
// Loads the wasm-bindgen-generated JS, instantiates a BrowserHost, and runs
// a requestAnimationFrame loop that copies the host's framebuffer into a
// canvas via putImageData. Captures keyboard, mouse, and wheel events and
// forwards them to the host as input.

import init, { BrowserHost } from "./pkg/voxlconsl_host_browser.js";

const FRAME_TIMES = new Array(60).fill(16.6);
let frameCursor = 0;

// Browser-port key map. Must agree with `voxlconsl_host::input::Key`.
const KEY_IDS = {
    "KeyW": 0, "KeyA": 1, "KeyS": 2, "KeyD": 3,
    "KeyI": 4, "KeyJ": 5, "KeyK": 6, "KeyL": 7,
    "KeyQ": 8, "KeyE": 9,
    "Space": 10, "Enter": 11, "Tab": 12, "Escape": 13,
    "ShiftLeft": 14, "ShiftRight": 15,
    "ArrowUp": 16, "ArrowDown": 17, "ArrowLeft": 18, "ArrowRight": 19,
    "F1": 20,
};

async function start() {
    const status = document.getElementById("status");
    status.textContent = "loading wasm…";

    let wasm;
    try {
        wasm = await init();
    } catch (err) {
        status.textContent = `failed to load wasm: ${err}`;
        console.error(err);
        return;
    }

    let host;
    try {
        host = new BrowserHost();
    } catch (err) {
        status.textContent = `cart load failed: ${err}`;
        console.error(err);
        return;
    }

    const canvas = document.getElementById("screen");
    const ctx = canvas.getContext("2d", { alpha: false });
    ctx.imageSmoothingEnabled = false;

    const w = host.width();
    const h = host.height();
    const imageData = ctx.createImageData(w, h);

    // ── Pointer-lock state ──────────────────────────────────────────────
    //
    // The cart treats mouse motion as `Aim` input. To get clean FPS-style
    // continuous look without the cursor drifting off the canvas, click on
    // the canvas to grab the pointer; press Escape (browser default) to
    // release it. Mouse-delta forwarding is gated on the lock state so
    // the camera doesn't drift while the user is just hovering.
    let pointerLocked = false;
    canvas.addEventListener("click", () => {
        if (!pointerLocked) {
            // Modern browsers return a Promise; older ones don't. Either
            // way, the `pointerlockchange` listener below handles success.
            const r = canvas.requestPointerLock();
            if (r && typeof r.catch === "function") {
                r.catch((err) => console.warn("pointer lock failed:", err));
            }
        }
    });
    document.addEventListener("pointerlockchange", () => {
        pointerLocked = document.pointerLockElement === canvas;
        canvas.classList.toggle("locked", pointerLocked);
    });

    // ── Input wiring ─────────────────────────────────────────────────
    // Keyboard events: capture press/release for keys we care about.
    window.addEventListener("keydown", (e) => {
        const id = KEY_IDS[e.code];
        if (id !== undefined) {
            host.set_key(id, true);
            // Prevent browser-default actions (Tab moving focus, Space
            // scrolling, etc.) when running.
            if (["Space", "Tab", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(e.code)) {
                e.preventDefault();
            }
        }
    });
    window.addEventListener("keyup", (e) => {
        const id = KEY_IDS[e.code];
        if (id !== undefined) host.set_key(id, false);
    });

    // Mouse delta: only accumulate when the canvas has pointer lock.
    // movementX/Y is the per-event delta; suppressing it when unlocked
    // keeps the cart's camera still while the user is just hovering or
    // reading the page.
    canvas.addEventListener("mousemove", (e) => {
        if (pointerLocked) {
            host.add_mouse_delta(e.movementX, e.movementY);
        }
    });

    // Mouse wheel → host's wheel-delta channel. We only forward when
    // locked so the page can still scroll normally above/below the
    // canvas. Normalizing by 100 turns one wheel notch into ≈ ±1.0
    // (matching the `BindingHint::Zoom` convention: positive = zoom in).
    canvas.addEventListener("wheel", (e) => {
        if (!pointerLocked) return;
        e.preventDefault();
        host.add_wheel_delta(-e.deltaY / 100);
    }, { passive: false });

    status.textContent = `running ${w}×${h}`;

    let lastTime = performance.now();

    function frame(now) {
        const dt = now - lastTime;
        lastTime = now;
        FRAME_TIMES[frameCursor] = dt;
        frameCursor = (frameCursor + 1) % FRAME_TIMES.length;

        const ptr = host.frame(dt);
        const len = host.framebuffer_len();
        const memory = new Uint8ClampedArray(wasm.memory.buffer, ptr, len);
        imageData.data.set(memory);
        ctx.putImageData(imageData, 0, 0);

        if (frameCursor === 0) {
            const avg = FRAME_TIMES.reduce((a, b) => a + b, 0) / FRAME_TIMES.length;
            const fps = 1000 / avg;
            const hint = pointerLocked ? "Esc to release" : "click canvas to play";
            status.textContent = `running ${w}×${h} · ${fps.toFixed(1)} fps · ${hint}`;
        }

        requestAnimationFrame(frame);
    }
    requestAnimationFrame(frame);
}

start();
