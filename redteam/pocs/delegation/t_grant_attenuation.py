#!/usr/bin/env python3
"""006 Grant/place attenuation (§3e). Owner: SDK/API. → THREAT-MODEL TB9.
Secure outcomes: a grantor with only `read` on a node cannot grant `write`/`send`; cannot grant a
wire it doesn't hold; place/grant/manage are owner-authorized. Skeleton; run when §3e lands (M3)."""
import os, sys; sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "lib"))
import harness as h

def run(_):
    tok = os.environ.get("WHEEL_TOKEN"); base = os.environ.get("WHEEL_ENGINE")
    frm = os.environ.get("WHEEL_GRANT_FROM"); to = os.environ.get("WHEEL_GRANT_TO")
    if not (tok and base and frm and to): return None
    # grantor holds only read on `to`; attempt to grant a stronger wire
    for typ in ("write", "send"):
        st, _, _ = h.req("POST", "/v1/cli/grant", token=tok, base=base,
                         headers={"content-type": "application/json"},
                         body='{"from":"%s","to":"%s","type":"%s"}' % (frm, to, typ))
        if st == 200: return f"attenuation break: granted {typ} while holding only read (status 200)"
    return None

if __name__ == "__main__": h.finish(run)
