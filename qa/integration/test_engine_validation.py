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

        # ---- a table node's name must already be a sqlite identifier ------------
        #
        # §3 permits '-' in ANY node name, and the engine narrows that for table nodes
        # only, because the name becomes `t_<name>` and '-' is subtraction in SQL. That
        # divergence is deliberate and is with PM to write into the contract; what is
        # asserted here is the BEHAVIOUR, so whichever way the ruling goes this test has
        # to be updated on purpose rather than drifting.
        st, body = http("POST", "/v1/nodes",
                        {"name": "bad-table", "type": "table", "position": {"x": 0, "y": 0},
                         "config": {"columns": [{"name": "col", "type": "text"}]}})
        R.check("VAL-table-name-identifier", 400 <= st < 500,
                "a hyphenated table name answered %s; it becomes `t_bad-table`" % st)
        R.check("VAL-table-name-identifier/says-why",
                "-" in json.dumps(body) and "_" in json.dumps(body),
                "the refusal should name the character AND the fix: %s" % json.dumps(body)[:160])

        # Atomic: the refusal must not leave a node on the board with nowhere to put rows.
        st, board = http("GET", "/v1/board")
        names = [n.get("name") for n in (board or {}).get("nodes", [])]
        R.check("VAL-table-name-atomic", "bad-table" not in names,
                "a table node survived a failed table creation: %s" % names)

        # No SILENT RENAME (PM ruling 2026-09-06). Refusing and then quietly creating
        # `bad_table` would satisfy the check above while giving the user a node at an
        # address they did not choose and cannot predict — and every peer's wires,
        # preamble and `wheel read` would use the name the user never typed. A refusal
        # has to refuse.
        renamed = [n for n in names if n and n.replace("_", "-") == "bad-table"]
        R.check("VAL-table-name-no-silent-rename", not renamed,
                "the engine refused `bad-table` and then created %s anyway — the user gets "
                "a node at an address they never chose" % renamed)

        # The SAME name is fine for a type that is not backed by a sqlite table — the
        # narrowing is table-specific, not a general retreat from §3's name rule.
        # A DIFFERENT name: reusing "bad-table" made this assert uniqueness, not naming.
        # It failed with 409 and read as "the engine refuses hyphens everywhere", which
        # would have been a much more alarming report than the truth.
        st, _ = http("POST", "/v1/nodes",
                     {"name": "hyphen-ok-ctx", "type": "ctx", "position": {"x": 0, "y": 0},
                      "config": {"markdown": "hyphens are legal in a node address"}})
        R.check("VAL-hyphen-legal-elsewhere", st in (200, 201),
                "a hyphenated CTX name was refused (%s) — §3 allows it" % st)

        R.check("NODE-engine-enforced-count", len(engine_enforced) == 12,
                "expected 12 engine-enforced fixtures, found %d — a fixture was retagged "
                "and the BUG-001 coverage claim no longer holds" % len(engine_enforced))
    finally:
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)



    return R.report("engine-validation")


if __name__ == "__main__":
    sys.exit(main())
