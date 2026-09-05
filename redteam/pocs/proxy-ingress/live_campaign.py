#!/usr/bin/env python3
"""Live API-tenancy + proxy/ingress campaign against the infra/dev stack (WHEEL_ENV=dev).

Boot: API :8080, host :7100, stub engine :7000 (infra/dev/e2e.py shows the recipe). Dev auth is
HS256 with a STATIC shared secret + issuer, so the RS256->HS256 confusion and kid-flood cases are
prod-only (mocked JWKS) and are marked SKIP-DEV here — API's own suite covers those.

Each check prints PASS (resisted) / FAIL (FINDING) / SKIP. FAIL is a real observed insecurity, not a
hypothesis. Run: WHEEL_STACK unused here; talks to localhost:8080 directly.
"""
import base64, hmac, hashlib, json, socket, time, urllib.request, urllib.error

API = "http://localhost:8080"
ISS = "https://dev.wheel.local"
SEC = b"dev-only-hs256-secret"
results = []

def b64u(b): return base64.urlsafe_b64encode(b).rstrip(b"=").decode()

def mint(sub, secret=SEC, alg="HS256", **over):
    h = b64u(json.dumps({"alg": alg, "typ": "JWT"}).encode())
    now = int(time.time())
    p = {"sub": sub, "iss": ISS, "exp": now + 3600, "nbf": now - 60}
    p.update(over)
    p = b64u(json.dumps(p).encode())
    if alg == "none":
        return f"{h}.{p}."
    sig = b64u(hmac.new(secret, f"{h}.{p}".encode(), hashlib.sha256).digest())
    return f"{h}.{p}.{sig}"

def call(method, path, token=None, body=None, headers=None, raw_body=None):
    req = urllib.request.Request(API + path, method=method)
    if token:
        req.add_header("x-auth-token", token)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    data = raw_body
    if body is not None:
        data = json.dumps(body).encode(); req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=30) as r:
            b = r.read().decode(errors="replace")
            return r.status, (json.loads(b) if b and b.lstrip().startswith(("{", "[")) else b)
    except urllib.error.HTTPError as e:
        b = e.read().decode(errors="replace")
        try: return e.code, json.loads(b)
        except Exception: return e.code, b
    except Exception as e:
        return None, repr(e)

def raw_request(raw_bytes: bytes, host="localhost", port=8080, read=4096):
    """Send hand-crafted bytes so we can test smuggling axum's parser would normalise."""
    s = socket.create_connection((host, port), timeout=10)
    s.sendall(raw_bytes); time.sleep(0.3)
    try: data = s.recv(read)
    except Exception: data = b""
    s.close()
    return data.decode(errors="replace")

def rec(label, resisted, detail=""):
    results.append((resisted, label, detail))
    tag = "PASS " if resisted is True else ("SKIP " if resisted is None else "FAIL ")
    print(f"{tag} {label}" + ((f"  [{detail}]") if detail else ""))

def setup():
    alice = mint("user_alice"); mallory = mint("user_mallory")
    st, proj = call("POST", "/v1/projects", alice, {"name": "adv-live"})
    assert st == 201, (st, proj)
    pid = proj["id"]
    call("POST", f"/v1/projects/{pid}/start", alice)
    st2, proj2 = call("POST", "/v1/projects", alice, {"name": "adv-live-2"})
    pid2 = proj2["id"] if st2 == 201 else None
    if pid2: call("POST", f"/v1/projects/{pid2}/start", alice)
    return alice, mallory, pid, pid2

def run():
    alice, mallory, pid, pid2 = setup()
    P = f"/v1/projects/{pid}/engine"

    # baseline: proxied board works
    st, b = call("GET", f"{P}/v1/board", alice)
    rec("baseline proxied /v1/board (control)", st == 200 and isinstance(b, dict) and "nodes" in b, f"status={st}")

    # ---- 1. path traversal / double-encoding through the proxy -------------------------------
    for name, seg in [
        ("dotdot literal", "/v1/../v1/board"),
        ("dotdot escape to host root", "/../../../host/v1/projects"),
        ("pct-encoded dotdot %2e%2e", "/%2e%2e/v1/board"),
        ("double-encoded %252e%252e", "/%252e%252e/v1/board"),
        ("encoded slash %2f", "/v1%2f..%2f..%2fboard"),
        ("v1/v1 double-prefix smell", "/v1/v1/board"),
        ("reach api's own /v1/projects via proxy", "/../../../../v1/projects"),
        ("backslash traversal", "/..\\..\\v1/board"),
    ]:
        st, body = call("GET", f"{P}{seg}", alice)
        # resisted = NOT 200-with-engine-data and NOT reaching a different privileged resource.
        leaked = st == 200 and isinstance(body, (dict, list))
        rec(f"1 traversal: {name}", not leaked, f"status={st} body={str(body)[:60]}")

    # host bearer reaching engine /v1/cli/* (proxy forwards? stub 404s but shows the bearer arrives)
    st, body = call("GET", f"{P}/v1/cli/whoami", alice)
    # If the proxy forwards it, the stub (bearer-gated) returns 404 (path unknown) NOT 401 -> bearer reached engine.
    rec("1b proxy forwards /v1/cli/* to engine with host bearer (engine must discriminate — SDK)",
        None, f"status={st} (404=bearer reached engine, 401=bearer scrubbed, 400=proxy-blocked)")

    # ---- 2. header smuggling -----------------------------------------------------------------
    # my own Authorization must be scrubbed (engine gets the real secret, not mine) -> board still 200
    st, b = call("GET", f"{P}/v1/board", alice, headers={"authorization": "Bearer forged-secret"})
    rec("2 authorization header scrubbed upstream", st == 200 and isinstance(b, dict), f"status={st}")
    # x-project-id mismatch vs path must be REJECTED fail-closed (400), not silently honoured.
    if pid2:
        st, _ = call("GET", f"{P}/v1/board", alice, headers={"x-project-id": pid2})
        rec("2 x-project-id mismatch rejected fail-closed", st == 400, f"status={st} (expect 400)")
    # matching x-project-id still serves
    st, _ = call("GET", f"{P}/v1/board", alice, headers={"x-project-id": pid})
    rec("2 x-project-id matching path still serves", st == 200, f"status={st}")

    # ---- 3. WS upgrade on a non-WS route -----------------------------------------------------
    resp = raw_request(
        (f"GET {P}/v1/board HTTP/1.1\r\nHost: localhost:8080\r\nx-auth-token: {alice}\r\n"
         f"Upgrade: websocket\r\nConnection: Upgrade\r\n"
         f"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
    line = resp.splitlines()[0] if resp else ""
    rec("3 WS upgrade on non-WS route not switched", "101" not in line, f"resp={line!r}")

    # ---- 4. ws-ticket replay / cross-project -------------------------------------------------
    st, tick = call("POST", f"/v1/projects/{pid}/ws-ticket", alice)
    if st != 200 or not isinstance(tick, dict) or "ticket" not in tick:
        rec("4 ws-ticket issue", None, f"status={st} body={str(tick)[:60]} (endpoint may be unimpl)")
    else:
        t = tick["ticket"]
        wsp = f"/v1/projects/{pid}/engine/v1/events?ticket={t}"
        s1, _ = call("GET", wsp, None)  # first use (no WS upgrade -> expect non-200 but consumes?)
        s2, _ = call("GET", wsp, None)  # replay
        rec("4 ws-ticket single-use (replay rejected)", not (s1 == s2 == 200), f"s1={s1} s2={s2}")
        if pid2:
            sx, _ = call("GET", f"/v1/projects/{pid2}/engine/v1/events?ticket={t}", None)
            rec("4 ws-ticket cross-project rejected", sx != 200, f"status={sx}")

    # ---- 5. JWT confusion --------------------------------------------------------------------
    prot = f"/v1/projects/{pid}"
    cases = {
        "alg=none": mint("user_alice", alg="none"),
        "wrong secret": mint("user_alice", secret=b"not-the-secret"),
        "empty secret": mint("user_alice", secret=b""),
        "expired": mint("user_alice", exp=int(time.time()) - 10),
        "future nbf": mint("user_alice", nbf=int(time.time()) + 3600),
        "wrong iss": mint("user_alice", iss="https://evil.example"),
        "tampered payload": (lambda t: t.split(".")[0] + "." + b64u(b'{"sub":"user_alice","iss":"'+ISS.encode()+b'","exp":9999999999,"nbf":0,"admin":true}') + "." + t.split(".")[2])(mint("user_alice")),
    }
    for name, tok in cases.items():
        st, _ = call("GET", prot, tok)
        rec(f"5 JWT {name} rejected (401)", st == 401, f"status={st}")
    rec("5 RS256->HS256 confusion / kid-flood", None, "SKIP-DEV: dev uses static HS256; prod-only (API suite covers)")

    # ---- 7. ingress body / rate limits (enable http capability first) ------------------------
    st, _ = call("PATCH", pid and f"/v1/projects/{pid}" or "", alice, {"capabilities": {"http": True}})
    if st not in (200, 204):
        rec("7 enable ingress capability", None, f"PATCH status={st} — skipping ingress body/rate")
    else:
        # oversized body (>5 MiB cap)
        big = b"x" * (6 * 1024 * 1024)
        st, _ = call("POST", f"/p/{pid}/anything", None, headers={"content-type": "application/octet-stream"}, raw_body=big)
        rec("7 ingress body over 5 MiB rejected (413)", st in (413, 400), f"status={st}")
        # rate limit: burst
        codes = [call("GET", f"/p/{pid}/ping")[0] for _ in range(80)]
        rec("7 ingress rate limit engages under burst", any(c == 429 for c in codes),
            f"429s={codes.count(429)}/80 (may be off in dev)")

    passed = sum(1 for r in results if r[0] is True)
    failed = sum(1 for r in results if r[0] is False)
    skipped = sum(1 for r in results if r[0] is None)
    print(f"\nSUMMARY: {passed} resisted · {failed} FINDINGS · {skipped} skip/observe")
    return failed

if __name__ == "__main__":
    import sys
    sys.exit(1 if run() else 0)
