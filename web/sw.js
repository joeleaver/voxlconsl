// Service worker that adds COOP/COEP headers to every same-origin
// response. Needed because GitHub Pages can't set HTTP headers
// directly, but `SharedArrayBuffer` (and the Stage-4b AudioWorklet
// path) requires the page to be cross-origin-isolated.
//
// Pattern: intercept fetches, rebuild the Response with the headers
// added. On first load the page isn't yet isolated — index.html
// detects that and reloads after registering the SW. From then on
// every request goes through this worker.
//
// Cache version bump forces all clients to pick up worker changes.
const SW_VERSION = "coi-v2";

self.addEventListener("install", (event) => {
    self.skipWaiting();
});

self.addEventListener("activate", (event) => {
    // On every activation also nuke the HTTP cache for build artefacts —
    // the wasm-bindgen JS and the host `.wasm` aren't query-string
    // versioned, so a stale browser cache from a previous deploy can
    // leave the page calling host methods that no longer exist
    // (or, equivalently, missing methods the new JS expects).
    event.waitUntil((async () => {
        if (self.caches) {
            const keys = await caches.keys();
            await Promise.all(keys.map((k) => caches.delete(k)));
        }
        await self.clients.claim();
    })());
});

self.addEventListener("fetch", (event) => {
    // Don't intercept cross-origin requests — they'd fail COEP anyway
    // and we don't want to break extension / analytics traffic.
    if (event.request.cache === "only-if-cached" &&
        event.request.mode !== "same-origin") {
        return;
    }
    event.respondWith((async () => {
        try {
            // Force the network for our own build artefacts so the
            // HTTP cache can't pin the page to a stale deploy. Other
            // requests follow normal caching.
            const url = new URL(event.request.url);
            const sameOrigin = url.origin === self.location.origin;
            const isBuildArtefact = sameOrigin && (
                url.pathname.endsWith(".js") ||
                url.pathname.endsWith(".wasm") ||
                url.pathname.endsWith(".voxl") ||
                url.pathname.endsWith(".json") ||
                url.pathname.endsWith(".css") ||
                url.pathname.endsWith("/") ||
                url.pathname.endsWith(".html")
            );
            const req = isBuildArtefact
                ? new Request(event.request, { cache: "reload" })
                : event.request;
            const response = await fetch(req);
            // Don't touch opaque (no-cors) responses; modifying them
            // strips most metadata and isn't useful anyway.
            if (response.type !== "basic" && response.type !== "default") {
                return response;
            }
            const newHeaders = new Headers(response.headers);
            newHeaders.set("Cross-Origin-Opener-Policy", "same-origin");
            newHeaders.set("Cross-Origin-Embedder-Policy", "require-corp");
            newHeaders.set("Cross-Origin-Resource-Policy", "same-origin");
            return new Response(response.body, {
                status: response.status,
                statusText: response.statusText,
                headers: newHeaders,
            });
        } catch (e) {
            return new Response(`SW fetch error: ${e}`, { status: 502 });
        }
    })());
});
