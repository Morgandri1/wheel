"""Shared red-team PoC harness. Stdlib only (no pip). Skips cleanly with no stack.

A probe convention: define run(h) that returns None on 'resisted' or a str finding on 'broken'.
Call finish(run) at module bottom. PASS = resisted; FAIL = live finding (exit 1).
"""
import os, sys, json, base64, hmac, hashlib, urllib.request, urllib.error

API = os.environ.get("WHEEL_STACK")        # e.g. http://localhost:8080
HOST = os.environ.get("WHEEL_HOST")         # host API base, if exposed in dev
SKIP = 77                                   # exit code = skipped (no stack)

def have_stack():
    return bool(API)

def _b64u(b): return base64.urlsafe_b64encode(b).rstrip(b"=").decode()

def jwt(header, payload, secret=b"", raw_sig=None):
    """Mint a JWT for attack variants: alg=none, HS256-with-pubkey confusion, wrong iss, exp/nbf."""
    h = _b64u(json.dumps(header, separators=(",", ":")).encode())
    p = _b64u(json.dumps(payload, separators=(",", ":")).encode())
    signing_input = f"{h}.{p}".encode()
    if raw_sig is not None:
        sig = raw_sig
    elif header.get("alg") == "none":
        sig = ""
    elif header.get("alg") == "HS256":
        sig = _b64u(hmac.new(secret, signing_input, hashlib.sha256).digest())
    else:
        sig = ""
    return f"{h}.{p}.{sig}"

def req(method, path, token=None, project=None, headers=None, body=None, base=None, follow=False):
    """HTTP with redirect-follow OFF by default (so we can inspect 3xx). Returns (status, headers, body)."""
    url = (base or API).rstrip("/") + path
    hdrs = dict(headers or {})
    if token is not None:  hdrs["x-auth-token"] = token
    if project is not None: hdrs["x-project-id"] = project
    data = body.encode() if isinstance(body, str) else body
    r = urllib.request.Request(url, data=data, method=method, headers=hdrs)
    opener = urllib.request.build_opener() if follow else urllib.request.build_opener(_NoRedirect)
    try:
        resp = opener.open(r, timeout=10)
        return resp.status, dict(resp.headers), resp.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()

class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k): return None

def finish(run):
    if not have_stack():
        print("SKIP (PENDING-STACK): set WHEEL_STACK to the local API base to run"); sys.exit(SKIP)
    try:
        finding = run(sys.modules["__main__"])
    except Exception as e:
        print(f"ERROR: probe crashed: {e!r}"); sys.exit(2)
    if finding:
        print(f"FAIL: {finding}"); sys.exit(1)
    print("PASS: resisted"); sys.exit(0)
