#!/usr/bin/env python3
"""004 Tool-node SSRF. Owner: SDK. → THREAT-MODEL TB8 / finding 004.
Secure outcome: /v1/tools/import + /v1/tools/:id/call reject base_url/redirect resolving to
loopback/RFC1918/link-local/*.internal, across IP encodings, AND after a redirect (resolve-and-pin).
Start mocks/metadata.py first; a hit on it = SSRF succeeded."""
import os, sys; sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "lib"))
import harness as h

BLOCKED_BASES = ["http://127.0.0.1:8169/latest", "http://169.254.169.254/latest",
                 "http://[::ffff:127.0.0.1]:8169", "http://2130706433:8169",       # decimal 127.0.0.1
                 "http://0x7f000001:8169", "http://wheel-host.railway.internal:7100",
                 "http://wheel-p-other:7000", "http://127.0.0.1.nip.io:8169"]

def run(_):
    tok = os.environ.get("WHEEL_TOKEN_A"); pid = os.environ.get("WHEEL_PROJECT_A")
    if not (tok and pid): return None
    for base in BLOCKED_BASES:
        st, _, _ = h.req("POST", f"/v1/projects/{pid}/engine/v1/tools/import", token=tok, project=pid,
                         headers={"content-type": "application/json"},
                         body='{"format":"manual","raw":"{}","base_url":"%s"}' % base)
        if st not in (400, 403, 422): return f"tool import accepted SSRF base_url {base} (status {st})"
    # TODO(stack): also register a redirect (public->loopback) target and assert per-redirect re-validation.
    return None

if __name__ == "__main__": h.finish(run)
