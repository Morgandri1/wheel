#!/usr/bin/env python3
"""Cross-account MUTATION probe (PM S1): can user B delete/patch/start/stop user A's project?
Owner: API. Boundary TB1. Local stack only, two of MY OWN dev accounts.

Order of operations the API must enforce (contract §5): verify JWT -> load project by id ->
assert project.owner_id == jwt.sub -> else 404 (no enumeration). So every B-on-A mutation must be
404 (or 403), and A's project must be UNCHANGED and still RUNNING afterwards.

Dev HS256 mint (infra/dev/e2e.py): issuer https://dev.wheel.local, secret dev-only-hs256-secret.
Auto-detects the API base among :8787/:8080 and confirms the token is accepted before probing.
PASS = attack resisted. Exit 1 if any cross-account mutation succeeded.
"""
import base64, hmac, hashlib, json, sys, time, urllib.request, urllib.error

ISSUER = "https://dev.wheel.local"
DEV_SECRET = b"dev-only-hs256-secret"
BASES = ["http://127.0.0.1:8787", "http://127.0.0.1:8080"]

def b64u(b): return base64.urlsafe_b64encode(b).rstrip(b"=").decode()
def mint(sub):
    h = b64u(json.dumps({"alg":"HS256","typ":"JWT"}).encode())
    now = int(time.time())
    p = b64u(json.dumps({"sub":sub,"iss":ISSUER,"exp":now+3600,"nbf":now-60}).encode())
    sig = b64u(hmac.new(DEV_SECRET, f"{h}.{p}".encode(), hashlib.sha256).digest())
    return f"{h}.{p}.{sig}"

API = None
def call(method, path, token=None, body=None):
    req = urllib.request.Request(API + path, method=method)
    if token: req.add_header("x-auth-token", token)
    data = None
    if body is not None:
        data = json.dumps(body).encode(); req.add_header("content-type","application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=60) as r:
            raw = r.read().decode()
            return r.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try: return e.code, json.loads(raw)
        except Exception: return e.code, raw
    except Exception as e:
        return None, repr(e)

findings = []
def check(label, resisted, detail=""):
    print(("PASS " if resisted else "FAIL ") + label + (f"  [{detail}]" if detail else ""))
    if not resisted: findings.append(f"{label} — {detail}")

def main():
    global API
    alice, mallory = mint("adv_alice"), mint("adv_mallory")
    # detect a base that accepts the dev token
    for b in BASES:
        API = b
        st, _ = call("GET", "/healthz")
        if st != 200: continue
        st, _ = call("GET", "/v1/projects", alice)
        if st == 200:
            print(f"using API={API} (dev HS256 accepted)"); break
    else:
        print("PENDING-STACK: no API on :8787/:8080 accepts a dev HS256 token "
              "(AUTH_MODE=local may reject the dev bypass — then this probe needs signup/login for two accounts).")
        return 0

    # A creates a project and starts it
    st, proj = call("POST", "/v1/projects", alice, {"name":"adv-a-owned"})
    if st != 201 or not isinstance(proj, dict):
        print(f"could not create A's project: status={st} {proj}"); return 0
    pid = proj["id"]

    # GUARD (learned the hard way): confirm A and B are DISTINCT principals before concluding
    # anything is "cross-account". A mock/dev auth that ignores `sub` and stamps a constant owner_id
    # collapses both tokens to ONE identity — then every "cross-account" success is really same-user
    # and proves nothing. Compare the owner_id the API stamps on each token's own project.
    st_b, proj_b = call("POST", "/v1/projects", mallory, {"name":"adv-b-owned"})
    oa = proj.get("owner_id"); ob = proj_b.get("owner_id") if isinstance(proj_b, dict) else None
    if oa is not None and oa == ob:
        print(f"INVALID-STACK: this API maps BOTH tokens to owner_id={oa!r} (mock/dev auth ignoring `sub`). "
              f"A and B are the SAME identity here — cross-account cannot be tested on this instance. "
              f"NOTE: if such a build is in production it is itself an S1 (identity collapse). "
              f"Re-run against a sub-honoring instance (real signup/login).")
        if isinstance(proj_b, dict): call("DELETE", f"/v1/projects/{proj_b['id']}", mallory)
        call("DELETE", f"/v1/projects/{pid}", alice)
        return 0
    if isinstance(proj_b, dict):
        call("DELETE", f"/v1/projects/{proj_b['id']}", mallory)
    print(f"principals distinct: A.owner={oa!r} B.owner={ob!r} — proceeding")
    call("POST", f"/v1/projects/{pid}/start", alice)
    print(f"A's project = {pid}")

    # B must not even see it
    st, _ = call("GET", f"/v1/projects/{pid}", mallory)
    check("B GET A's project -> 404", st == 404, f"status={st}")

    # THE MUTATIONS — each must be 404 (or 403), never a 2xx
    st, b = call("DELETE", f"/v1/projects/{pid}", mallory)
    check("B DELETE A's project refused", st in (403,404), f"status={st} {str(b)[:60]}")
    st, b = call("PATCH", f"/v1/projects/{pid}", mallory, {"name":"pwned-by-mallory"})
    check("B PATCH A's project refused", st in (403,404), f"status={st} {str(b)[:60]}")
    st, b = call("POST", f"/v1/projects/{pid}/stop", mallory)
    check("B STOP A's project refused", st in (403,404), f"status={st} {str(b)[:60]}")
    st, b = call("POST", f"/v1/projects/{pid}/start", mallory)
    check("B START A's project refused", st in (403,404), f"status={st} {str(b)[:60]}")
    st, b = call("POST", f"/v1/projects/{pid}/restart", mallory)
    check("B RESTART A's project refused", st in (403,404), f"status={st} {str(b)[:60]}")

    # A's project must still exist and be unchanged (name intact, still reachable)
    st, after = call("GET", f"/v1/projects/{pid}", alice)
    intact = st == 200 and isinstance(after, dict) and after.get("name") == "adv-a-owned"
    check("A's project intact after B's attempts (exists + name unchanged)", intact,
          f"status={st} name={after.get('name') if isinstance(after,dict) else after}")

    # cleanup: A deletes its own project
    call("DELETE", f"/v1/projects/{pid}", alice)

    if findings:
        print(f"\n{len(findings)} FINDING(S) — CROSS-ACCOUNT MUTATION POSSIBLE (S1)")
        for f in findings: print("  -", f)
        return 1
    print("\nALL RESISTED — no cross-account delete/patch/start/stop")
    return 0

if __name__ == "__main__":
    sys.exit(main())
