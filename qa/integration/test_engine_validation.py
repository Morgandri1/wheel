#!/usr/bin/env python3
"""Engine-side node validation — TESTPLAN NODE-*, and the definitive answer on BUG-001.

BUG-001 established that the exported JSON Schema accepts 12 configs the contract
forbids. That is only half a finding: wheel-core also has validate.rs and serde
deny_unknown_fields, either of which may still reject them at runtime. Until someone
checks, "defence in depth" is a claim with one layer verified and one layer assumed —
which is exactly the shape of a security finding that turns out to be either nothing or
everything.

So each fixture tagged `_enforced_by: engine` is POSTed to a REAL engine here. Whatever
the result, we stop guessing.
"""
import glob, json, os, subprocess, sys, time, urllib.error, urllib.request, uuid

SKIP = 77
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)
from wheel_client import Results

IMAGE = os.environ.get("WHEEL_ENGINE_IMAGE", "wheel-engine:test")
SECRET = "qa-engine-secret-at-least-16"
PORT = int(os.environ.get("WHEEL_ENGINE_PORT", "17311"))
BASE = "http://127.0.0.1:%d" % PORT
CONTAINER = "qa-engine-validation"

R = Results()


def req(method, path, body=None, token=SECRET):
    r = urllib.request.Request(BASE + path, method=method)
    if token:
        r.add_header("Authorization", "Bearer " + token)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        r.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(r, data, timeout=30) as resp:
            txt = resp.read().decode(errors="replace")
            return resp.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        txt = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(txt)
        except Exception:
            return e.code, txt


def docker(*args, **kw):
    return subprocess.run(["docker"] + list(args), capture_output=True, text=True, **kw)


def start_engine():
    docker("rm", "-f", CONTAINER)
    key = subprocess.run(["openssl", "rand", "-base64", "32"],
                         capture_output=True, text=True).stdout.strip()
    p = docker("run", "-d", "--name", CONTAINER,
               "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
               "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
               "-e", "WHEEL_VAULT_KEY=" + key,
               "-e", "WHEEL_ROLE=engine",
               "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
               "-p", "%d:7000" % PORT, IMAGE)
    if p.returncode != 0:
        return "could not start %s: %s" % (IMAGE, p.stderr.strip()[:200])
    for _ in range(40):
        try:
            if req("GET", "/healthz", token=None)[0] == 200:
                return None
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


def main():
    if docker("info").returncode != 0:
        print("docker not running")
        return SKIP
    if docker("image", "inspect", IMAGE).returncode != 0:
        print("%s not built yet (SDK: make engine-image-test)" % IMAGE)
        return SKIP

    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        # --------------------------------------------------- auth on the control plane
        st, _ = req("GET", "/v1/board", token=None)
        R.check("ENG-auth-required", st == 401, "no bearer -> %s" % st)
        st, _ = req("GET", "/v1/board", token="wrong-secret-entirely-here")
        R.check("ENG-auth-wrong", st == 401, "wrong bearer -> %s" % st)
        st, board = req("GET", "/v1/board")
        R.check("ENG-board-shape", st == 200 and isinstance(board, dict)
                and "nodes" in board and "project" in board, "-> %s %r" % (st, board))

        # --------------------------------------------------- the BUG-001 question
        # Every fixture the schema wrongly accepts, POSTed to a real engine.
        fixtures = sorted(glob.glob(os.path.join(ROOT, "qa/fixtures/nodes/invalid/*.json")))
        engine_enforced = 0
        for path in fixtures:
            doc = json.load(open(path))
            if doc.pop("_enforced_by", "schema") != "engine":
                continue
            engine_enforced += 1
            name = os.path.basename(path)[:-5]
            crit = doc.pop("_expect_reject", "?")
            doc.pop("_engine_ref", None)
            doc.pop("_known_bug", None)
            # Give each node a unique, legal name so a rejection can only be about the
            # thing under test — not a name collision from an earlier case.
            payload = {k: v for k, v in doc.items() if k in ("type", "config", "position")}
            payload["name"] = "v%s" % uuid.uuid4().hex[:12]
            payload.setdefault("position", {"x": 0.0, "y": 0.0})
            st, body = req("POST", "/v1/nodes", payload)
            R.check("NODE-engine-rejects/%s" % name, 400 <= st < 500,
                    "%s: engine ACCEPTED it (-> %s) but %s says it must be rejected"
                    % (name, st, crit))

        R.check("BUG-001/fixtures-present", engine_enforced >= 12,
                "expected >=12 engine-enforced fixtures, found %d" % engine_enforced)

        # --------------------------------------------------- valid nodes still work
        for path in sorted(glob.glob(os.path.join(ROOT, "qa/fixtures/nodes/valid/*.json"))):
            doc = json.load(open(path))
            if doc.get("type") == "tool":
                continue  # tool nodes are M2
            payload = {"type": doc["type"], "config": doc["config"],
                       "position": doc.get("position", {"x": 0.0, "y": 0.0}),
                       "name": "ok%s" % uuid.uuid4().hex[:12]}
            st, body = req("POST", "/v1/nodes", payload)
            R.check("NODE-engine-accepts/%s" % os.path.basename(path)[:-5],
                    200 <= st < 300, "valid fixture REJECTED -> %s %r" % (st, body))
    finally:
        docker("rm", "-f", CONTAINER)

    return R.report("engine-validation")


if __name__ == "__main__":
    sys.exit(main())
