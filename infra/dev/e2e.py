#!/usr/bin/env python3
"""End-to-end proof of the API -> host -> engine chain.

Mints a dev HS256 token, creates a project, starts its sandbox, and reads the board back through
the authenticated proxy. Also exercises the two failure modes that matter most: an unauthenticated
request, and one user reaching for another user's project.
"""
import base64, hmac, hashlib, json, sys, time, urllib.request, urllib.error

API = "http://localhost:8080"
ISSUER = "https://dev.wheel.local"
DEV_SECRET = b"dev-only-hs256-secret"


def b64u(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def mint(sub: str) -> str:
    header = b64u(json.dumps({"alg": "HS256", "typ": "JWT"}).encode())
    now = int(time.time())
    payload = b64u(json.dumps({"sub": sub, "iss": ISSUER, "exp": now + 3600, "nbf": now - 60}).encode())
    signing_input = f"{header}.{payload}".encode()
    sig = b64u(hmac.new(DEV_SECRET, signing_input, hashlib.sha256).digest())
    return f"{header}.{payload}.{sig}"


def call(method, path, token=None, body=None):
    req = urllib.request.Request(API + path, method=method)
    if token:
        req.add_header("x-auth-token", token)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=60) as r:
            raw = r.read().decode()
            return r.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, raw


def check(label, cond, detail=""):
    print(f"{'PASS' if cond else 'FAIL'}  {label}{(' — ' + str(detail)) if detail and not cond else ''}")
    return cond


alice, mallory = mint("user_alice"), mint("user_mallory")
ok = True

status, health = call("GET", "/healthz")
ok &= check("healthz", status == 200, (status, health))

status, proj = call("POST", "/v1/projects", alice, {"name": "e2e board"})
ok &= check("create project", status == 201, (status, proj))
if status != 201:
    sys.exit(1)
pid = proj["id"]
print(f"      project {pid} status={proj['status']}")

status, started = call("POST", f"/v1/projects/{pid}/start", alice)
ok &= check("start sandbox", status == 200 and started.get("status") == "running", (status, started))

status, board = call("GET", f"/v1/projects/{pid}/engine/v1/board", alice)
ok &= check("proxied GET engine/v1/board", status == 200 and "nodes" in (board or {}), (status, board))
if status == 200:
    print(f"      board={json.dumps(board)}")

# --- the boundary ---------------------------------------------------------------------------
status, _ = call("GET", f"/v1/projects/{pid}/engine/v1/board")
ok &= check("no token -> 401", status == 401, status)

status, _ = call("GET", f"/v1/projects/{pid}", mallory)
ok &= check("another user's project -> 404 (not 403)", status == 404, status)

status, _ = call("GET", f"/v1/projects/{pid}/engine/v1/board", mallory)
ok &= check("another user cannot proxy -> 404", status == 404, status)

status, _ = call("GET", "/p/" + pid + "/anything")
ok &= check("ingress disabled by default -> 403", status == 403, status)

print("\nRESULT:", "ALL PASS" if ok else "FAILURES ABOVE")
sys.exit(0 if ok else 1)
