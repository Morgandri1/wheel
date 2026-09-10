#!/usr/bin/env python3
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""End-to-end proof of the API -> host -> engine chain.

Signs up two throwaway users, creates a project, starts its sandbox, and reads the board back
through the authenticated proxy. Also exercises the two failure modes that matter most: an
unauthenticated request, and one user reaching for another user's project.
"""
import json, os, sys, urllib.request, urllib.error, uuid

# Same override `qa/integration/run.sh` and every suite under `qa/integration/` already honour, so
# this script runs against whatever stack is up (a different port, a CI-assigned host) rather than
# only ever the hardcoded default a human runs locally.
API = os.environ.get("WHEEL_API_URL", "http://localhost:8080")


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


def signup() -> str:
    """A real session token from `/v1/auth/signup` (docs/API.md), the only kind the local auth
    backend accepts. Docker-compose's api service runs AUTH_MODE=local (unset defaults there, per
    config.rs), so a hand-minted HS256 JWT for the jwks/dev-bypass path was never actually verified
    here — it only looked like it worked because this script's own PASS/FAIL output never matched
    run.sh's result-line regex, so a 401 on every run went unnoticed until that regex was fixed.
    A random email per run avoids a 409 against a store that persists between runs."""
    email = f"e2e-{uuid.uuid4()}@wheel.test"
    status, body = call("POST", "/v1/auth/signup", body={"email": email, "password": "Correct-Horse-9!"})
    if status != 201:
        print(f"FAIL  signup ({email}) — {(status, body)}")
        sys.exit(1)
    return body["token"]


alice, mallory = signup(), signup()
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
