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
const SW_VERSION = "coi-v1";

self.addEventListener("install", (event) => {
    self.skipWaiting();
});

self.addEventListener("activate", (event) => {
    event.waitUntil(self.clients.claim());
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
            const response = await fetch(event.request);
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
