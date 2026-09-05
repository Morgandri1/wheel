"""Shared client for the integration suite: dev-auth tokens + API/engine calls.

Deliberately stdlib-only and deliberately NOT importing anything from the product.
A test suite that reuses the implementation's own client shares its bugs and its
assumptions — if the API serialises a field wrongly, a shared client serialises it
wrongly in both directions and the test passes.

mint() is copied from infra/dev/e2e.py (API's implementation, per their instruction to
reuse rather than reinvent the token format).
"""
import base64, hashlib, hmac, json, os, time, urllib.error, urllib.request

API = os.environ.get("WHEEL_API_URL", "http://localhost:8080")
ISSUER = os.environ.get("CLERK_ISSUER", "https://dev.wheel.local")
DEV_SECRET = os.environ.get("AUTH_DEV_SECRET", "dev-only-hs256-secret").encode()
BACKEND = os.environ.get("SANDBOX_BACKEND", "docker")


def b64u(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def mint(sub, iss=None, secret=None, alg="HS256", exp_delta=3600, nbf_delta=-60):
    """Mint a dev token. Non-default args exist so the auth NEGATIVE cases are expressible."""
    header = b64u(json.dumps({"alg": alg, "typ": "JWT"}).encode())
    now = int(time.time())
    payload = b64u(json.dumps({
        "sub": sub, "iss": iss or ISSUER,
        "exp": now + exp_delta, "nbf": now + nbf_delta}).encode())
    signing_input = ("%s.%s" % (header, payload)).encode()
    sig = b64u(hmac.new(secret if secret is not None else DEV_SECRET,
                        signing_input, hashlib.sha256).digest())
    return "%s.%s.%s" % (header, payload, sig)


def call(method, path, token=None, body=None, headers=None, base=None, timeout=60, raw_body=None):
    req = urllib.request.Request((base or API) + path, method=method)
    if token:
        req.add_header("x-auth-token", token)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    data = raw_body
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=timeout) as r:
            txt = r.read().decode(errors="replace")
            return r.status, (json.loads(txt) if txt.strip() else None), dict(r.headers)
    except urllib.error.HTTPError as e:
        txt = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(txt), dict(e.headers)
        except Exception:
            return e.code, txt, dict(e.headers)
    except urllib.error.URLError as e:
        raise RuntimeError("cannot reach %s%s — is the stack up? (%s)" % (base or API, path, e))


def engine(method, project_id, path, token, body=None):
    """Call a project's engine through the authenticated API proxy."""
    return call(method, "/v1/projects/%s/engine%s" % (project_id, path), token, body)


def wait_for(predicate, timeout=90, interval=1.0, what="condition"):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            last = predicate()
            if last:
                return last
        except Exception as e:
            last = e
        time.sleep(interval)
    raise AssertionError("timed out after %ss waiting for %s (last: %r)" % (timeout, what, last))


def api_healthy(timeout=120):
    return wait_for(lambda: call("GET", "/healthz")[0] == 200,
                    timeout=timeout, what="API /healthz")


class Results:
    """Minimal result collector that reports by TESTPLAN ID."""

    def __init__(self):
        self.passed, self.failed, self.skipped = [], [], []

    def check(self, tid, cond, detail=""):
        if cond:
            self.passed.append(tid)
            print("  ok    %-28s" % tid)
        else:
            self.failed.append((tid, detail))
            print("  FAIL  %-28s %s" % (tid, detail))
        return bool(cond)

    def skip(self, tid, why):
        self.skipped.append((tid, why))
        print("  skip  %-28s %s" % (tid, why))

    def report(self, suite):
        print("\n%s: %d passed, %d failed, %d skipped (backend=%s)"
              % (suite, len(self.passed), len(self.failed), len(self.skipped), BACKEND))
        if self.failed:
            print("FAILED:")
            for tid, detail in self.failed:
                print("  - %s %s" % (tid, detail))
            return 1
        return 0
