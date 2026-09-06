#!/usr/bin/env python3
"""Engine-side rejection of the configs the schema wrongly accepts — TESTPLAN NODE-*.

This is the second half of BUG-001. The exported JSON Schema accepts twelve node configs
the contract forbids, which by itself only means the schema cannot be the validation gate.
Whether anything ELSE rejects them is the question that decides whether BUG-001 is a
documentation defect or a hole, and until now nobody had asserted it.

ADVERSARY probed it live and found all twelve rejected (finding 013/009). That was a
one-off. This turns it into a regression: their probes, my permanent assertions, which is
the split we agreed. If validate.rs loses a branch — and it is the crate's least-covered
file — this goes red rather than the property quietly evaporating.

Each fixture carries the TESTPLAN criterion it exists to prove, so a failure names the
rule that broke rather than a filename.
"""
import glob, json, os, subprocess, sys, time, uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results

SKIP = 77
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
INVALID = os.path.join(ROOT, "qa", "fixtures", "nodes", "invalid")
VALID = os.path.join(ROOT, "qa", "fixtures", "nodes", "valid")
IMAGE = os.environ.get("WHEEL_ENGINE_IMAGE", "wheel-engine:test")
NAME = "qa-validation-%s" % uuid.uuid4().hex[:8]
PORT = int(os.environ.get("WHEEL_VALIDATION_PORT", "17428"))
SECRET = "qa-engine-secret-at-least-16-chars"

R = Results()


def http(method, path, body=None):
    import urllib.error, urllib.request
    req = urllib.request.Request("http://127.0.0.1:%d%s" % (PORT, path), method=method)
    req.add_header("authorization", "Bearer " + SECRET)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=30) as r:
            txt = r.read().decode(errors="replace")
            return r.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        txt = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(txt)
        except Exception:
            return e.code, txt
    except Exception as e:
        return 0, repr(e)


def start_engine():
    subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)
    p = subprocess.run([
        "docker", "run", "-d", "--name", NAME,
        "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
        "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
        "-e", "WHEEL_VAULT_KEY=" + "A" * 43 + "=",
        "-e", "WHEEL_ROLE=engine",
        "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
        "-p", "%d:7000" % PORT, IMAGE,
    ], capture_output=True, text=True)
    if p.returncode != 0:
        return "docker run failed: " + (p.stderr or p.stdout)[-400:]
    for _ in range(60):
        if http("GET", "/v1/board")[0] == 200:
            return None
        time.sleep(1)
    logs = subprocess.run(["docker", "logs", "--tail", "20", NAME],
                          capture_output=True, text=True)
    return "engine never became healthy: " + (logs.stdout + logs.stderr)[-400:]


def payload(doc):
    """Strip QA bookkeeping and the server-assigned id."""
    return {k: v for k, v in doc.items()
            if not k.startswith("_") and k != "id"}


def main():
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    if subprocess.run(["docker", "image", "inspect", IMAGE], capture_output=True).returncode != 0:
        print("%s not built — run `make engine-image-test`" % IMAGE)
        return SKIP

    err = start_engine()
    if err:
        print(err)
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)
        return SKIP

    try:
        # Control: the engine must ACCEPT what the contract allows. Without this, an engine
        # that rejected everything would score a perfect 12/12 below and look secure.
        accepted = 0
        for p in sorted(glob.glob(os.path.join(VALID, "*.json"))):
            doc = payload(json.load(open(p)))
            doc["name"] = "ok-%s" % uuid.uuid4().hex[:8]
            doc["wires"] = []
            st, _ = http("POST", "/v1/nodes", doc)
            if 200 <= st < 300:
                accepted += 1
        R.check("NODE-valid-accepted", accepted > 0,
                "the engine accepted NONE of the %d valid fixtures, so the rejection "
                "results below prove nothing"
                % len(glob.glob(os.path.join(VALID, "*.json"))))

        # The twelve the schema lets through.
        engine_enforced = []
        for p in sorted(glob.glob(os.path.join(INVALID, "*.json"))):
            doc = json.load(open(p))
            if doc.get("_enforced_by") == "engine":
                engine_enforced.append((os.path.basename(p)[:-5], doc))

        for name, doc in engine_enforced:
            crit = doc.get("_expect_reject", "?")
            body = payload(doc)
            body.setdefault("wires", [])
            st, resp = http("POST", "/v1/nodes", body)
            R.check("%s/%s" % (crit, name), 400 <= st < 500,
                    "engine ACCEPTED a config the contract forbids (-> %s %s). The schema "
                    "already accepts it (BUG-001), so nothing rejects it."
                    % (st, json.dumps(resp)[:120]))

        R.check("NODE-engine-enforced-count", len(engine_enforced) == 12,
                "expected 12 engine-enforced fixtures, found %d — a fixture was retagged "
                "and the BUG-001 coverage claim no longer holds" % len(engine_enforced))
    finally:
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)

    return R.report("engine-validation")


if __name__ == "__main__":
    sys.exit(main())
