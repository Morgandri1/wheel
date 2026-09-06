#!/usr/bin/env python3
"""Loopback witness server for the tool-executor allowed-path probe. Runs INSIDE the sandbox
container on 127.0.0.1:<port> (an allowlisted target). It RECORDS what the executor actually sent —
the whole point is a witness for header-CRLF and body-not-replayed, not just a pass/fail.
Routes:
  /echo        -> 200 JSON {method, headers (as received), body} — the witness
  /redir-meta  -> 302 Location: http://169.254.169.254/   (per-hop revalidation must refuse hop 2)
  /redir-2nd   -> 302 Location: http://127.0.0.1:<other>/echo (body-not-replayed target; <other> allowlisted)
  /redir-bad   -> 302 Location: http://127.0.0.1:19999/    (not allowlisted -> refused)
  /c1 -> /c2 -> /c3  (redirect-limit chain)
  /big         -> ~6 MiB body (5 MiB cap must trip)
  /slow        -> sleep 35s (30s timeout must trip)
"""
import json, os, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]); OTHER = sys.argv[2] if len(sys.argv) > 2 else str(PORT)

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _redir(self, loc):
        self.send_response(302); self.send_header("Location", loc); self.end_headers()
    def handle_one(self):
        n = self.path.split("?")[0]
        clen = int(self.headers.get("content-length", 0) or 0)
        body = self.rfile.read(clen).decode("latin1") if clen else ""
        if n == "/echo":
            # Received headers verbatim — the witness. A raw-name list captures any CRLF-split header.
            hdrs = {k: v for k, v in self.headers.items()}
            raw_names = list(self.headers.keys())
            out = json.dumps({"method": self.command, "headers": hdrs,
                              "raw_header_names": raw_names, "body": body}).encode()
            self.send_response(200); self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(out))); self.end_headers(); self.wfile.write(out)
        elif n == "/redir-meta": self._redir("http://169.254.169.254/")     # per-hop revalidation must refuse
        elif n == "/redir-2nd":  self._redir(f"http://echo2.test:{OTHER}/echo")  # 2nd allowlisted hop (body must not follow)
        elif n == "/redir-bad":  self._redir("http://127.0.0.1:19999/")     # unallowlisted literal -> refused
        elif n == "/c1": self._redir("/c2")
        elif n == "/c2": self._redir("/c3")
        elif n == "/c3": self._redir("/c4")
        elif n == "/c4": self._redir("/c5")   # c1..c5 = 4 redirects > MAX_REDIRECTS(3) -> must bail
        elif n == "/big":
            blob = b"A" * (6 * 1024 * 1024)
            self.send_response(200); self.send_header("content-length", str(len(blob))); self.end_headers()
            try: self.wfile.write(blob)
            except Exception: pass
        elif n == "/slow":
            time.sleep(35)
            self.send_response(200); self.end_headers()
        else:
            self.send_response(404); self.end_headers()
    def do_GET(self): self.handle_one()
    def do_POST(self): self.handle_one()

ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
