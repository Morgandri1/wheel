#!/usr/bin/env python3
"""Mock cloud-metadata + DNS-rebinding target (RoE: never hit the real 169.254.169.254).

Run:  python3 metadata.py 9199
Then point an SSRF probe's base_url at http://127.0.0.1:9199/... . The tool executor / mcp.url / egress
filter under test MUST refuse to reach it once it resolves to a blocked IP; this mock only exists so a
FAILING (unfiltered) request has something harmless to hit and reveal the leak, instead of the real
metadata service.

/latest/meta-data/...            -> fake IMDS response (a canary string, never real creds)
/rebind                          -> flips: first call returns a public-looking payload, later calls a
                                    'secret' payload — models a TTL=0 A-record flip after the pre-check.
"""
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

CANARY = "SSRF-CANARY-do-not-trust: if a test saw this, egress/SSRF filtering FAILED"
_hits = {"rebind": 0}

class H(BaseHTTPRequestHandler):
    def log_message(self, *a):  # quiet
        pass
    def do_GET(self):
        if self.path == "/rebind":
            _hits["rebind"] += 1
            payload = "public-ok" if _hits["rebind"] == 1 else "REBOUND-" + CANARY
        elif self.path.startswith("/latest/meta-data"):
            payload = CANARY
        else:
            payload = CANARY
        b = payload.encode()
        self.send_response(200)
        self.send_header("content-type", "text/plain")
        self.send_header("content-length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9199
    print(f"mock metadata on 127.0.0.1:{port} (canary only, no real secrets)")
    HTTPServer(("127.0.0.1", port), H).serve_forever()
