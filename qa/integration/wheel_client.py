"""Shared client for the QA integration suite.

Drives the public API exactly as a browser would — over HTTP, through the real gateway — so the
tests exercise the deployed chain (api -> host -> engine) rather than any in-process shortcut.

Dev tokens are HS256, honoured only because the stack runs with WHEEL_ENV=dev and AUTH_DEV_SECRET
set. The API refuses to boot with that secret under WHEEL_ENV=prod, which is itself asserted by
API's own config-interlock test; QA does not re-test it here, but does assert that the dev path
cannot be used to forge anything (wrong key, alg:none, wrong issuer).
"""
import base64, hashlib, hmac, json, os, time, urllib.error, urllib.request

API = os.environ.get("WHEEL_API", "http://localhost:8080")
ISSUER = os.environ.get("CLERK_ISSUER", "https://dev.wheel.local")
DEV_SECRET = os.environ.get("AUTH_DEV_SECRET", "dev-only-hs256-secret").encode()


def b64u(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def mint(sub, secret=None, alg="HS256", issuer=None, exp_delta=3600, nbf_delta=-60, sign=True):
    """Mint a dev token. Every parameter is a knob a forgery test needs to turn."""
    header = {"alg": alg, "typ": "JWT"}
    now = int(time.time())
    payload = {"sub": sub, "iss": issuer if issuer is not None else ISSUER,
               "exp": now + exp_delta, "nbf": now + nbf_delta}
    h, p = b64u(json.dumps(header).encode()), b64u(json.dumps(payload).encode())
    if not sign:
        return f"{h}.{p}."
    sig = hmac.new(secret if secret is not None else DEV_SECRET,
                   f"{h}.{p}".encode(), hashlib.sha256).digest()
    return f"{h}.{p}.{b64u(sig)}"


class Response:
    __slots__ = ("status", "body", "headers", "elapsed")

    def __init__(self, status, body, headers, elapsed):
        self.status, self.body, self.headers, self.elapsed = status, body, headers, elapsed

    @property
    def json(self):
        try:
            return json.loads(self.body)
        except (ValueError, TypeError):
            return None

    def __repr__(self):
        return "<%s %r>" % (self.status, self.body[:120])


def call(method, path, token=None, body=None, project=None, headers=None, timeout=20):
    req = urllib.request.Request(API + path, method=method)
    if token:
        req.add_header("x-auth-token", token)
    if project:
        req.add_header("x-project-id", project)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("content-type", "application/json")
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, data, timeout=timeout) as r:
            return Response(r.status, r.read().decode(), dict(r.headers), time.time() - t0)
    except urllib.error.HTTPError as e:
        return Response(e.code, e.read().decode(), dict(e.headers), time.time() - t0)


def api_up(timeout=1.0):
    try:
        return call("GET", "/healthz", timeout=timeout).status == 200
    except Exception:
        return False


def new_project(token, name="qa-project"):
    r = call("POST", "/v1/projects", token=token, body={"name": name})
    assert r.status in (200, 201), "could not create project: %r" % r
    return r.json
