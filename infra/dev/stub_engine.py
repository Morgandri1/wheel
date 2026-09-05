"""Stub engine for end-to-end testing of the API -> host -> engine chain.

Stands in for SDK's `wheel-engine` until the real one lands. It implements only what the chain
needs to be provable: an unauthenticated /healthz for the host's readiness probe, and a
bearer-gated /v1/board so we can show that the engine secret is being injected by the host and
that an unauthenticated request is refused.
"""

import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

SECRET = os.environ.get("WHEEL_ENGINE_SECRET", "")
PROJECT_ID = os.environ.get("WHEEL_PROJECT_ID", "")


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        # Readiness is deliberately unauthenticated: the host polls it before it has any reason to
        # trust the container, and the real engine's spawn contract (ARCHITECTURE 4b) says the same.
        if self.path == "/healthz":
            return self._send(200, {"ok": True})

        presented = self.headers.get("authorization", "")
        if presented != f"Bearer {SECRET}":
            return self._send(401, {"error": "unauthorized"})

        if self.path.startswith("/v1/board"):
            return self._send(200, {
                "nodes": [],
                "project": {"id": PROJECT_ID, "stub": True},
            })
        return self._send(404, {"error": "not_found"})

    def log_message(self, fmt, *args):
        print("stub-engine: " + (fmt % args), flush=True)


if __name__ == "__main__":
    print(f"stub-engine listening on :7000 (project {PROJECT_ID})", flush=True)
    HTTPServer(("0.0.0.0", 7000), Handler).serve_forever()
