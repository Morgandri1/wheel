#!/usr/bin/env python3
"""004 Tool-node SSRF — the re-validate-AFTER-DNS-AND-EVERY-REDIRECT vector.

This is the "cheap confirmed finding" spot: a base_url that passes the pre-filter (public host) but
then (a) 30x-redirects to a blocked target, or (b) DNS-rebinds to loopback on the SECOND resolution.
Secure engine behaviour (finding 004 / SDK plan risk): resolve-once-PIN-the-IP for the connection AND
re-validate every redirect hop's resolved IP; a pre-filter on the hostname string alone is NOT enough.

Setup: run mocks/metadata.py on 127.0.0.1:9199 (its /rebind flips public->CANARY on the 2nd hit; a
seen CANARY = the block failed). Point a redirector you control at http://127.0.0.1:9199/latest.
Env: WHEEL_STACK, WHEEL_TOKEN_A, WHEEL_PROJECT_A, and REDIRECTOR (a public URL that 302s to the
blocked target — modelled locally, never a real public host per RoE)."""
import os, sys
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "lib"))
import harness as h

CANARY = "SSRF-CANARY"

def run(_):
    tok, pid = os.environ.get("WHEEL_TOKEN_A"), os.environ.get("WHEEL_PROJECT_A")
    if not (tok and pid):
        return None
    redirector = os.environ.get("REDIRECTOR", "http://127.0.0.1:9199/redirect-to-blocked")
    base = f"/v1/projects/{pid}/engine/v1/tools/{{tid}}/call"

    # 1) redirect hop: base_url passes pre-filter, then 302 -> blocked IP. Engine MUST re-validate the
    #    redirect target's resolved IP and refuse; a CANARY in the body = it followed into the block.
    st, _, body = h.req("POST", base.format(tid="redir"), token=tok, project=pid,
                        headers={"content-type": "application/json"},
                        body='{"op":"probe","args":{},"base_url":"%s"}' % redirector)
    if st < 400 and CANARY.encode() in (body or b""):
        return f"SSRF via redirect: engine followed a 30x into a blocked IP (status {st}, canary seen)"

    # 2) DNS rebind: same hostname resolves public at check-time, loopback at connect-time. Engine MUST
    #    connect to the PINNED checked IP, not re-resolve. A CANARY here = it re-resolved and connected.
    rebind = os.environ.get("REBIND_HOST", "http://127.0.0.1:9199/rebind")
    st2, _, body2 = h.req("POST", base.format(tid="rebind"), token=tok, project=pid,
                         headers={"content-type": "application/json"},
                         body='{"op":"probe","args":{},"base_url":"%s"}' % rebind)
    if st2 < 400 and (b"REBOUND" in (body2 or b"") or CANARY.encode() in (body2 or b"")):
        return f"SSRF via DNS rebinding: engine re-resolved instead of pinning the checked IP (status {st2})"
    return None

if __name__ == "__main__":
    h.finish(run)
