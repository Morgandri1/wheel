"""Shared client for the integration suite: dev-auth tokens + API/engine calls.

Deliberately stdlib-only and deliberately NOT importing anything from the product.
A test suite that reuses the implementation's own client shares its bugs and its
assumptions — if the API serialises a field wrongly, a shared client serialises it
wrongly in both directions and the test passes.

mint() is copied from infra/dev/e2e.py (API's implementation, per their instruction to
reuse rather than reinvent the token format).
"""
import base64, hashlib, hmac, json, os, subprocess, time, urllib.error, urllib.parse, urllib.request, uuid
from collections import namedtuple

# Tuple-unpackable AND attribute-addressable: `st, body, hdrs = call(...)` and
# `call(...).status` both work. Two suites were written against different shapes of this
# return value; rather than rewrite one of them (and break whichever I did not run), the
# value satisfies both.
Response = namedtuple("Response", "status body headers")
Response.json = property(lambda self: self.body)

API = os.environ.get("WHEEL_API_URL", "http://localhost:8080")
ISSUER = os.environ.get("CLERK_ISSUER", "https://dev.wheel.local")
DEV_SECRET = os.environ.get("AUTH_DEV_SECRET", "dev-only-hs256-secret").encode()
BACKEND = os.environ.get("SANDBOX_BACKEND", "docker")


def b64u(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def unique_sub(prefix="qa"):
    """A fresh tenant id per call.

    The API team hit this against a shared database: every test authenticating as the
    literal "user_alice" piled projects under one id across tests AND across runs, until
    a per-user project cap tripped. Worse, the resulting failure message claimed the list
    endpoint had leaked another user's projects — the boundary was fine, the fixture was
    not. A test whose failure message accuses the product of a leak it did not commit
    costs someone an afternoon, so QA mints a unique sub rather than truncating tables.
    """
    return "%s_%s" % (prefix, uuid.uuid4().hex[:16])


def sub_of(token):
    """The `sub` claim of a token we hold, read from the token rather than remembered.

    `owner_id == the JWT sub` is the property worth asserting; `owner_id == "user_alice"` is
    that property with a dev-mode implementation detail baked in, and it fails under local
    auth where the sub is a generated uuid. Reading the claim keeps the assertion true in
    both modes. No signature check here on purpose — the API verified it, and this is a test
    reading its own token.
    """
    payload = token.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    return json.loads(base64.urlsafe_b64decode(payload.encode()))["sub"]


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


def session_for(sub):
    """A token that authenticates against whichever provider the API is actually running.

    `mint()` produces a dev HS256 token, which only works under AUTH_MODE=jwks with
    AUTH_DEV_SECRET set. The API now defaults an unset AUTH_MODE to `local` in dev
    (config.rs:143), so a freshly built stack rejects every minted token with 401 while a
    long-running container built before local auth accepts them. That difference is why
    this suite passed on my machine and failed on its first real CI run.

    Rather than pin the mode — which would make the suite assert against a configuration
    nobody deploys — sign up a local account when the minted token is refused. A suite
    that only works under one auth provider is testing the provider, not the product.
    """
    tok = mint(sub)
    st, _, _ = call("GET", "/v1/projects", tok)
    # 2xx, not `!= 401`. A stack that is still coming up answers 502/503, and treating that
    # as "dev tokens are accepted here" hands back a token that starts failing the moment
    # the API is actually ready — which is how this suite passed locally against a warm
    # stack and failed in CI against a cold one, reporting it as an auth bug in the API.
    if 200 <= (st or 0) < 300:
        return tok
    email = "%s@qa.wheel.local" % sub.replace("|", "-")
    password = "qa-integration-password"
    st, body, _ = call("POST", "/v1/auth/signup", None, {"email": email, "password": password})
    if st == 409 or (st and st >= 400):
        st, body, _ = call("POST", "/v1/auth/login", None,
                           {"email": email, "password": password})
    tok = (body or {}).get("token") if isinstance(body, dict) else None
    if not tok:
        raise RuntimeError(
            "cannot authenticate against this API: the dev token was refused (AUTH_MODE is "
            "probably `local`) and local signup answered %s %r" % (st, body))
    return tok


_LOOPBACK = ("127.0.0.1", "localhost", "::1", "[::1]", "0.0.0.0")
ALLOW_REMOTE = os.environ.get("WHEEL_ALLOW_REMOTE") == "1"


def assert_local(url):
    """Refuse to send a test request anywhere but this machine.

    The integration suites create and DELETE projects. Every base URL they use defaults to
    loopback, but a base is overridable by environment (WHEEL_API_URL) and our own docs
    show `export WHEEL_API=https://wheel-api-production...` as a happy path, so one stale
    variable in one shell is all it takes for a teardown sweep to run against production
    with whatever account happens to be configured.

    Loopback is not a restriction on what tests may do -- it is the reason they are allowed
    to be destructive at all. Set WHEEL_ALLOW_REMOTE=1 deliberately if you really mean it.
    """
    host = urllib.parse.urlsplit(url).hostname or ""
    if host in _LOOPBACK or ALLOW_REMOTE:
        return
    raise RuntimeError(
        "refusing to send a test request to %r.\n"
        "These suites create, mutate and DELETE projects, and they are only safe because "
        "they talk to a throwaway local stack. Point them at a shared or production host "
        "and they will delete somebody's work.\n"
        "If you genuinely mean to run against %s, set WHEEL_ALLOW_REMOTE=1." % (url, host))


def call(method, path, token=None, body=None, headers=None, base=None, timeout=60, raw_body=None):
    url = (base or API) + path
    assert_local(url)
    req = urllib.request.Request(url, method=method)
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
            return Response(r.status, (json.loads(txt) if txt.strip() else None), dict(r.headers))
    except urllib.error.HTTPError as e:
        txt = e.read().decode(errors="replace")
        try:
            return Response(e.code, json.loads(txt), dict(e.headers))
        except Exception:
            return Response(e.code, txt, dict(e.headers))
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


def api_up(timeout=3):
    """Is the API reachable right now? Used by pytest suites to skip rather than error."""
    try:
        return call("GET", "/healthz", timeout=timeout)[0] == 200
    except Exception:
        return False


def new_project(token, name="qa-project"):
    """Create a project and return it, raising with the server's own words on failure."""
    st, proj, _ = call("POST", "/v1/projects", token, {"name": name})
    if st not in (200, 201) or not isinstance(proj, dict):
        raise AssertionError("could not create project %r: %s %r" % (name, st, proj))
    return proj


def pin_image(tag="wheel-engine:test"):
    """Resolve an image TAG to the immutable ID it points at right now.

    Six agents share one docker daemon on this host, and several of them build
    `wheel-engine:test`. A suite that runs `docker run wheel-engine:test` twenty times
    over four minutes can therefore test twenty containers from more than one build:
    the tag is a mutable pointer, and somebody else's `make engine-image-test` moves it
    mid-run. This bit me for real — a suite reported F015 unfixed, in detail, with
    /proc evidence, against an image another agent had just replaced under me. The fix
    was already on main and the report would have been false.

    Returns the sha256 ID, which every `docker run` in the run should use instead of the
    tag, so a result describes one build and says which.
    """
    p = subprocess.run(["docker", "image", "inspect", "--format", "{{.Id}}", tag],
                       capture_output=True, text=True)
    return p.stdout.strip() if p.returncode == 0 else None


def free_port(preferred):
    """`preferred` if it is bindable right now, otherwise any free port.

    Suites use a fixed default port so a human can find the engine while debugging, and
    qa/contract/suite_isolation.py keeps those defaults distinct. Distinct defaults stop
    two SUITES colliding; they do nothing about the same suite running twice, which is
    routine on a host shared by six agents. A wheel-on-wheel run lost its engine that way
    mid-clone, and the retry could not start at all because the first run still held 17426.
    """
    import socket
    with socket.socket() as s:
        try:
            s.bind(("127.0.0.1", preferred))
            return preferred
        except OSError:
            pass
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


SKIP_RC = 77


def run_suite(main, name, cleanup=None, container=None):
    """Entry point wrapper: an unexpected exception is a reported failure, not a traceback.

    A suite that dies mid-way prints a stack trace and exits 1. Exit 1 reads as "the
    subject is broken" when what actually happened is "the suite fell over" -- and when
    the output is captured rather than watched, a truncated log can leave no visible
    reason at all. Naming the suite and the exception keeps the two apart, and cleanup
    still runs so the next run does not inherit a stray container.
    """
    rc = 1
    try:
        rc = main()
        return rc
    except KeyboardInterrupt:
        rc = 130
        print("%s: interrupted" % name)
        return rc
    except Exception as e:
        import traceback
        traceback.print_exc()
        rc = 1
        print("\n%s: the SUITE failed (%s: %s) — this is a broken test, not a verdict on "
              "the subject. Nothing below it ran." % (name, type(e).__name__, e))
        return rc
    finally:
        # Save the engine's own account of a bad run BEFORE tearing it down.
        #
        # Teardown that runs on failure destroys the evidence for the failure. A
        # wheel-on-wheel run died with "connection reset by peer" mid-turn -- either the
        # engine fell over or it dropped a long poll, and those want opposite fixes -- and
        # the cleanup I had just added removed the container, and with it the only log that
        # could tell them apart. Cleanup on success; on failure, keep the account first.
        if container and rc not in (0, SKIP_RC):
            try:
                art = os.path.join("qa", "artifacts")
                os.makedirs(art, exist_ok=True)
                dest = os.path.join(art, "%s-engine.log" % name)
                logs = subprocess.run(["docker", "logs", "--tail", "400", container],
                                      capture_output=True, text=True)
                with open(dest, "w") as fh:
                    fh.write(logs.stdout + logs.stderr)
                print("engine log for the failed run saved to %s" % dest)
            except Exception:
                pass
        if cleanup:
            try:
                cleanup()
            except Exception:
                pass


# Fake-harness steering keys, mapped from the env var each suite used to set.
FAKE_ENV_TO_KEY = {
    "WHEEL_FAKE_ENV_DUMP": "env_dump",
    "WHEEL_FAKE_TRANSCRIPT": "transcript",
    "WHEEL_FAKE_SCRIPT": "script",
    "WHEEL_FAKE_ENV_DUMP_KEYS": "env_dump_keys",
    "WHEEL_FAKE_ENV_SENTINELS": "env_sentinels",
    "WHEEL_FAKE_AUTH": "auth",
    "WHEEL_FAKE_SESSION_ID": "session_id",
}


def configure_fakes(container, **opts):
    """Steer the fake harnesses through /data/wheel-fake.json inside `container`.

    Since ADVERSARY F015 the engine gives a child an empty environment plus a short
    allowlist, so WHEEL_FAKE_* set on the ENGINE stops at the engine and never reaches the
    harness. Suites that still set it get a child that spawns perfectly and records nothing,
    which surfaces as "the child never spawned" or a delivery that never arrives -- a failure
    that points at the engine and is entirely the test's own.

    I made that change, converted two suites with it, and left three behind. The gate
    qa/contract/fake_steering.py exists so the fourth one cannot happen.
    """
    cfg = json.dumps({k: v for k, v in opts.items() if v is not None})
    p = subprocess.run(["docker", "exec", "-i", container, "sh", "-c",
                        "cat > /data/wheel-fake.json"],
                       input=cfg, capture_output=True, text=True)
    if p.returncode != 0:
        return "could not write /data/wheel-fake.json: " + (p.stderr or "")[:160]
    return None
