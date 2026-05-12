#!/usr/bin/env python3
"""Local dev server that serves `web/` with COOP/COEP headers.

The browser only exposes `SharedArrayBuffer` when the page is
cross-origin-isolated, which in turn requires both `Cross-Origin-
Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy:
require-corp`. Python's stock `http.server` doesn't set these — so
use this script for local development of the audio worklet path
(Stage 4b, SPEC.md §5.8).

Usage:
    python3 scripts/dev-server.py [port]

Defaults to port 8765 to match the previous workflow.
"""
import http.server
import socketserver
import sys
from pathlib import Path


class CoiHandler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        # Cross-origin isolation: required for SharedArrayBuffer.
        # `require-corp` is the strictest COEP; safe because every
        # asset we ship is same-origin.
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        # The wasm + JS we serve are also same-origin; advertising
        # COROP isn't strictly required but cheap to add.
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        super().end_headers()


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    repo_root = Path(__file__).resolve().parent.parent
    web_dir = repo_root / "web"
    if not web_dir.is_dir():
        print(f"error: {web_dir} not found", file=sys.stderr)
        sys.exit(1)

    import os
    os.chdir(web_dir)

    socketserver.TCPServer.allow_reuse_address = True
    with socketserver.TCPServer(("0.0.0.0", port), CoiHandler) as httpd:
        print(f"serving {web_dir} on http://localhost:{port}/ (COOP/COEP enabled)")
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            print("\nshutting down")


if __name__ == "__main__":
    main()
