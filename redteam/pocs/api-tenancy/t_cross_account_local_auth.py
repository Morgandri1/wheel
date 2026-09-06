#!/usr/bin/env python3
"""Cross-account MUTATION probe via REAL local-auth (PM S1). Owner: API. Boundary TB1.
Two DISTINCT accounts created through /v1/auth/signup (opaque `local.<uuid>` session tokens),
so unlike the dev-HS256 path (which collapses every token to owner_id='user_mock' on this
instance) these are genuinely different principals. Then B tries to delete/patch/start/stop A's
project. Contract §5: verify token -> load project -> assert owner==caller -> else 404.
PASS = resisted. Exit 1 if any cross-account mutation succeeded or A's project was harmed.
"""
import json, os, sys, time, urllib.request, urllib.error
API = os.environ.get("WHEEL_API", "http://127.0.0.1:8787")

def call(method, path, token=None, body=None):
    req = urllib.request.Request(API + path, method=method)
    if token: req.add_header("x-auth-token", token)
    data = None
    if body is not None:
        data = json.dumps(body).encode(); req.add_header("content-type","application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=60) as r:
            raw = r.read().decode(); return r.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try: return e.code, json.loads(raw)
        except Exception: return e.code, raw
    except Exception as e:
        return None, repr(e)

def signup(tag):
    email = f"{tag}-{int(time.time()*1000)}@adv.local"
    st, b = call("POST", "/v1/auth/signup", None, {"email": email, "password": "CorrectHorse9!xyz"})
    if st in (200,201) and isinstance(b, dict): return b["token"], b["user"]["id"]
    st, b = call("POST", "/v1/auth/login", None, {"email": email, "password": "CorrectHorse9!xyz"})
    return (b["token"], b["user"]["id"]) if isinstance(b, dict) else (None, None)

findings = []
def check(label, resisted, detail=""):
    print(("PASS " if resisted else "FAIL ") + label + (f"  [{detail}]" if detail else ""))
    if not resisted: findings.append(f"{label} — {detail}")

def main():
    (ta, ida), (tb, idb) = signup("adva"), signup("advb")
    if not (ta and tb):
        print("PENDING: local-auth signup unavailable"); return 0
    if ida == idb:
        print(f"INVALID: both signups returned the same user id {ida} — not distinct principals"); return 0
    print(f"A={ida}  B={idb}  (distinct principals)")

    st, proj = call("POST", "/v1/projects", ta, {"name":"adv-a-owned"})
    if st != 201 or not isinstance(proj, dict):
        print(f"could not create A's project: {st} {proj}"); return 0
    pid = proj["id"]; print(f"A's project = {pid} owner={proj.get('owner_id')}")
    call("POST", f"/v1/projects/{pid}/start", ta)

    st, _ = call("GET", f"/v1/projects/{pid}", tb);          check("B GET A -> 404", st==404, f"status={st}")
    st,b = call("DELETE", f"/v1/projects/{pid}", tb);        check("B DELETE A refused", st in (403,404), f"status={st} {str(b)[:50]}")
    st,b = call("PATCH", f"/v1/projects/{pid}", tb, {"name":"pwned"}); check("B PATCH A refused", st in (403,404), f"status={st}")
    st,b = call("POST", f"/v1/projects/{pid}/stop", tb);     check("B STOP A refused", st in (403,404), f"status={st}")
    st,b = call("POST", f"/v1/projects/{pid}/start", tb);    check("B START A refused", st in (403,404), f"status={st}")
    st,b = call("POST", f"/v1/projects/{pid}/restart", tb);  check("B RESTART A refused", st in (403,404), f"status={st}")

    st, after = call("GET", f"/v1/projects/{pid}", ta)
    intact = st==200 and isinstance(after,dict) and after.get("name")=="adv-a-owned"
    check("A's project intact (exists + name unchanged)", intact,
          f"status={st} name={after.get('name') if isinstance(after,dict) else after}")

    call("DELETE", f"/v1/projects/{pid}", ta)  # A cleans up its own
    if findings:
        print(f"\n{len(findings)} FINDING(S) — CROSS-ACCOUNT MUTATION POSSIBLE (S1)")
        for f in findings: print("  -", f)
        return 1
    print("\nALL RESISTED — no cross-account delete/patch/start/stop"); return 0

if __name__ == "__main__":
    sys.exit(main())
