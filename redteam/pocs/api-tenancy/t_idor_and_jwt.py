#!/usr/bin/env python3
"""001 API tenancy: cross-tenant IDOR + JWT alg=none. Owner: API. → THREAT-MODEL TB1.
Secure outcomes: (a) user A + project B → 404 (no enumeration). (b) alg=none token → 401.
Fill USER_A_TOKEN / PROJECT_B (another owner's id) from env when the stack is up."""
import os, sys; sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "lib"))
import harness as h

def run(_):
    a = os.environ.get("WHEEL_TOKEN_A"); pb = os.environ.get("WHEEL_PROJECT_B")
    if not (a and pb): return None  # data not provisioned yet; skeleton no-ops safely
    st, _, _ = h.req("GET", f"/v1/projects/{pb}", token=a, project=pb)
    if st != 404: return f"cross-tenant read of project B returned {st}, expected 404 (IDOR/enumeration)"
    none_tok = h.jwt({"alg": "none", "typ": "JWT"}, {"sub": "attacker", "iss": "https://clerk"})
    st2, _, _ = h.req("GET", "/v1/projects", token=none_tok)
    if st2 != 401: return f"alg=none token accepted (status {st2}, expected 401)"
    return None

if __name__ == "__main__": h.finish(run)
