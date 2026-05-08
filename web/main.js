// voxlconsl browser shim.
//
// Loads the wasm-bindgen-generated JS, instantiates a BrowserHost, and runs
// a requestAnimationFrame loop that copies the host's framebuffer into a
// canvas via putImageData. Captures keyboard + mouse events and forwards
// them to the host as input.

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

    // Mouse delta: only accumulate when the canvas has pointer lock or
    // when the mouse is over the canvas.
    canvas.addEventListener("mousemove", (e) => {
        // movementX/Y is the per-event delta, available without lock.
        host.add_mouse_delta(e.movementX, e.movementY);
    });

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
            status.textContent = `running ${w}×${h} · ${fps.toFixed(1)} fps · WASD pan, mouse aim`;
        }

        requestAnimationFrame(frame);
    }
    requestAnimationFrame(frame);
}

start();
