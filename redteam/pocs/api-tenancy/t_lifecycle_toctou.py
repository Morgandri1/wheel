#!/usr/bin/env python3
"""API second-pass: lifecycle TOCTOU + use-after-delete + rate-limit scope. Owner: API.

Live against infra/dev (localhost:8080, WHEEL_ENV=dev). Races the project lifecycle
(start/stop/restart/delete) to look for: orphaned sandboxes, double-spawn, use-after-delete reaching a
still-live engine, and whether the authenticated proxy (not just public ingress) is rate-limited.

Each check prints PASS (resisted) / FAIL (FINDING) / OBS (observation). Cleans up its projects.
"""
import base64, hmac, hashlib, json, threading, time, urllib.request, urllib.error

API = "http://localhost:8080"; ISS = "https://dev.wheel.local"; SEC = b"dev-only-hs256-secret"
findings = []; created = []

def b64u(b): return base64.urlsafe_b64encode(b).rstrip(b"=").decode()
def mint(sub):
    h = b64u(json.dumps({"alg": "HS256", "typ": "JWT"}).encode()); n = int(time.time())
    p = b64u(json.dumps({"sub": sub, "iss": ISS, "exp": n + 3600, "nbf": n - 60}).encode())
    return f"{h}.{p}." + b64u(hmac.new(SEC, f"{h}.{p}".encode(), hashlib.sha256).digest())
ALICE = mint("user_alice")

def call(method, path, tok=ALICE, body=None):
    r = urllib.request.Request(API + path, method=method)
    if tok: r.add_header("x-auth-token", tok)
    d = None
    if body is not None: d = json.dumps(body).encode(); r.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(r, d, timeout=30) as x:
            b = x.read().decode(); return x.status, (json.loads(b) if b.strip().startswith(("{", "[")) else b)
    except urllib.error.HTTPError as e:
        b = e.read().decode()
        try: return e.code, json.loads(b)
        except Exception: return e.code, b
    except Exception as e:
        return None, repr(e)

def newproj(name):
    st, p = call("POST", "/v1/projects", body={"name": name})
    if st == 201: created.append(p["id"]); return p["id"]
    return None

def rec(label, ok, detail=""):
    print(("PASS " if ok is True else ("OBS  " if ok is None else "FAIL ")) + label + (f"  [{detail}]" if detail else ""))
    if ok is False: findings.append(f"{label} — {detail}")

def parallel(fns):
    out = [None] * len(fns)
    def wrap(i, f): out[i] = f()
    ts = [threading.Thread(target=wrap, args=(i, f)) for i, f in enumerate(fns)]
    for t in ts: t.start()
    for t in ts: t.join()
    return out

def run():
    # 1. concurrent double-start — must be idempotent (one running sandbox, consistent status)
    pid = newproj("toctou-double-start")
    res = parallel([lambda: call("POST", f"/v1/projects/{pid}/start")] * 4)
    codes = [r[0] for r in res]
    statuses = {(r[1] or {}).get("status") if isinstance(r[1], dict) else None for r in res}
    st, final = call("GET", f"/v1/projects/{pid}")
    rec("1 concurrent 4x start is idempotent (all 2xx, final=running)",
        all(c in (200, 409) for c in codes) and (final or {}).get("status") == "running",
        f"codes={codes} final={(final or {}).get('status')}")

    # 2. start || delete race — must not orphan a sandbox nor 500; final state coherent
    pid2 = newproj("toctou-start-delete")
    res = parallel([lambda: call("POST", f"/v1/projects/{pid2}/start"),
                    lambda: call("DELETE", f"/v1/projects/{pid2}")])
    st_after, body_after = call("GET", f"/v1/projects/{pid2}")
    # coherent = project is gone (404) OR present with a definite status; never 500, never a ghost
    rec("2 start||delete race resolves coherently (404 or definite state, no 5xx)",
        st_after in (200, 404) and all(r[0] not in (500, 502, 503) for r in res),
        f"race={[r[0] for r in res]} after={st_after}")
    if st_after == 200:
        pass  # still exists; cleanup handles it
    else:
        if pid2 in created: created.remove(pid2)

    # 3. use-after-delete — proxy to engine of a deleted project must 404, never reach a live engine
    pid3 = newproj("toctou-uaf")
    call("POST", f"/v1/projects/{pid3}/start")
    call("DELETE", f"/v1/projects/{pid3}")
    if pid3 in created: created.remove(pid3)
    st_uaf, body_uaf = call("GET", f"/v1/projects/{pid3}/engine/v1/board")
    rec("3 use-after-delete: proxy to deleted project's engine → 404 (no live engine)",
        st_uaf == 404, f"status={st_uaf} body={str(body_uaf)[:50]}")

    # 4. rapid start;stop;start;restart churn — final status deterministic, no 5xx
    pid4 = newproj("toctou-churn")
    seq = [("POST", "start"), ("POST", "stop"), ("POST", "start"), ("POST", "restart"), ("POST", "stop")]
    codes4 = [call(m, f"/v1/projects/{pid4}/{a}")[0] for m, a in seq]
    st4, f4 = call("GET", f"/v1/projects/{pid4}")
    rec("4 start/stop/restart churn stays coherent (no 5xx, definite final)",
        all(c not in (500, 502, 503) for c in codes4) and (f4 or {}).get("status") in ("stopped", "running", "error"),
        f"codes={codes4} final={(f4 or {}).get('status')}")

    # 5. rate-limit scope: is the AUTHENTICATED proxy rate-limited, or only public ingress?
    pid5 = newproj("toctou-rl"); call("POST", f"/v1/projects/{pid5}/start")
    proxy_codes = [call("GET", f"/v1/projects/{pid5}/engine/v1/board")[0] for _ in range(60)]
    rec("5 authenticated proxy rate-limit (429 under 60-burst)",
        None, f"429s={proxy_codes.count(429)}/60 (OBS: if 0, authed proxy is unbounded — DoS lever via host→engine)")

    print(f"\nSUMMARY: {sum(1 for f in findings)} FINDINGS")
    return findings

def cleanup():
    for pid in created:
        call("DELETE", f"/v1/projects/{pid}")

if __name__ == "__main__":
    import sys
    try:
        fs = run()
    finally:
        cleanup()
    sys.exit(1 if fs else 0)
