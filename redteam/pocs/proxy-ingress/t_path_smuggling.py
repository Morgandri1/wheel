#!/usr/bin/env python3
"""Proxy/ingress path traversal. Owner: API. → THREAT-MODEL TB2.
Secure outcomes: authed proxy cannot reach /v1/cli/* (node-token path); public ingress cannot reach
/v1/* (control plane). Try raw '..', %2e%2e, double-encode, mixed separators."""
import os, sys; sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "lib"))
import harness as h

TRAVERSALS = ["/../v1/cli/whoami", "/%2e%2e/v1/cli/whoami", "/%252e%252e/v1/cli/whoami",
              "/..%2fv1%2fcli%2fwhoami", "/engine/../../v1/cli/whoami"]

def run(_):
    tok = os.environ.get("WHEEL_TOKEN_A"); pid = os.environ.get("WHEEL_PROJECT_A")
    if not (tok and pid): return None
    for t in TRAVERSALS:  # authed proxy must not expose the node-token CLI plane
        st, _, _ = h.req("GET", f"/v1/projects/{pid}/engine{t}", token=tok, project=pid)
        if st not in (400, 403, 404): return f"authed proxy reached cli plane via {t} (status {st})"
    for t in ["/../v1/board", "/%2e%2e/v1/board"]:  # public ingress must not reach control plane
        st, _, _ = h.req("GET", f"/p/{pid}{t}")
        if st not in (400, 403, 404): return f"public ingress reached control plane via {t} (status {st})"
    return None

if __name__ == "__main__": h.finish(run)
