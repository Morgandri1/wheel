"""Shared PoC harness. Stdlib only (no deps assumed on the host).

Rules of engagement enforced here: probes talk ONLY to the local stack named by WHEEL_STACK
(the API base URL) and WHEEL_HOST (the host base URL). If unset, skip() so probes never hit a
remote target. Redirects are never auto-followed (SSRF probes must inspect each hop).
"""
import base64, hashlib, hmac, json, os, sys, time, urllib.request, urllib.error

def stack() -> str | None:
    return os.environ.get("WHEEL_STACK")

def skip_if_no_stack(name: str) -> bool:
    if not stack():
        print(f"PENDING-STACK: {name} (set WHEEL_STACK to a local dev API to run)")
        return True
    return False

def _b64u(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()

def jwt(header: dict, payload: dict, secret: bytes = b"", *, sig: bytes | None = None) -> str:
    """Build a JWT. Used to mint ATTACK variants (alg=none, HS256-with-public-key, bad iss/exp).
    Never used to forge a real Clerk token against anything but the local stack."""
    h = _b64u(json.dumps(header, separators=(",", ":")).encode())
    p = _b64u(json.dumps(payload, separators=(",", ":")).encode())
    signing_input = f"{h}.{p}".encode()
    if sig is not None:
        s = sig
    elif header.get("alg") == "none":
        return f"{h}.{p}."
    else:
        s = hmac.new(secret, signing_input, hashlib.sha256).digest()
    return f"{h}.{p}.{_b64u(s)}"

def request(method: str, path: str, *, headers: dict | None = None, body: bytes | None = None,
            follow_redirects: bool = False):
    """Return (status, headers, body_bytes). Does NOT raise on 4xx/5xx. No redirect-follow by default."""
    url = stack().rstrip("/") + path
    req = urllib.request.Request(url, method=method, data=body, headers=headers or {})
    class _NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *a, **k):
            return None
    opener = urllib.request.build_opener() if follow_redirects else urllib.request.build_opener(_NoRedirect)
    try:
        r = opener.open(req, timeout=10)
        return r.status, dict(r.headers), r.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()

def result(passed: bool, msg: str) -> int:
    print(("PASS: resisted — " if passed else "FAIL: FINDING — ") + msg)
    return 0 if passed else 1


# --- compatibility API used by the campaign probes (req/finish) ---------------

def req(method: str, path: str, *, token: str | None = None, project: str | None = None,
        headers: dict | None = None, body=None, follow_redirects: bool = False):
    """Like request(), but sets the tenancy headers and encodes a str body. Returns (status, headers, bytes)."""
    hdrs = dict(headers or {})
    if token is not None:
        hdrs["x-auth-token"] = token
    if project is not None:
        hdrs["x-project-id"] = project
    data = body.encode() if isinstance(body, str) else body
    return request(method, path, headers=hdrs, body=data, follow_redirects=follow_redirects)

def finish(run):
    """Run a probe's run(argv)->None|str. None = resisted (PASS); str = the finding (FAIL).
    Skips cleanly with a clear reason when the stack or its env is not set."""
    import os as _os
    if not stack():
        print("PENDING-STACK: set WHEEL_STACK (+ per-probe WHEEL_TOKEN_A/WHEEL_PROJECT_A etc.) to run")
        sys.exit(0)
    try:
        finding = run(sys.argv[1:])
    except Exception as e:  # a probe crash is not a PASS
        print(f"ERROR: probe raised {e!r}")
        sys.exit(2)
    if finding is None:
        sys.exit(result(True, "no leak/SSRF observed"))
    sys.exit(result(False, finding))
