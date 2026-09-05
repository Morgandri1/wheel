"""Mock cloud-metadata + rebinding target on localhost (RoE: never hit real 169.254.169.254).

Run: python3 metadata.py [port]   → serves a fake IMDS token at /latest/... on 127.0.0.1:<port>.
SSRF probes point the tool node's base_url / a redirect at this, asserting the engine's deny-list
blocks it (the engine should refuse BEFORE the request reaches here; a hit here = SSRF succeeded).
"""
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

HIT = {"count": 0}
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        HIT["count"] += 1
        self.send_response(200); self.end_headers()
        self.wfile.write(b"FAKE-IMDS-TOKEN-should-never-be-reachable")
    def log_message(self, *a): pass

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8169
    print(f"mock metadata on 127.0.0.1:{port} (a hit here means SSRF was NOT blocked)")
    HTTPServer(("127.0.0.1", port), H).serve_forever()
