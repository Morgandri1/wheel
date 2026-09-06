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
from wheel_client import Results, free_port

SKIP = 77
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
INVALID = os.path.join(ROOT, "qa", "fixtures", "nodes", "invalid")
VALID = os.path.join(ROOT, "qa", "fixtures", "nodes", "valid")
IMAGE = os.environ.get("WHEEL_ENGINE_IMAGE", "wheel-engine:test")
NAME = "qa-validation-%s" % uuid.uuid4().hex[:8]
PORT = free_port(int(os.environ.get("WHEEL_VALIDATION_PORT", "17428")))
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


def wheel(node_id, *argv):
    """Run `wheel ...` in the container with that node's real token file.

    Table row writes have no engine-secret route (`PUT /v1/tables/:id/rows/:row` has never
    existed and never will by design -- §4 only exposes GET rows / POST query on that realm;
    a write is always CLI/node-token gated through `POST /v1/cli/write`, i.e. `wheel write`).
    This file's own `http()` helper only ever sends the engine secret, so the two PUTs this
    replaces were silently 404ing and every "before"/"after" row assertion below them had
    been running against an empty, re-ensured table rather than real data (ADVERSARY, via PM).
    Matches test_engine_cli.py's own `wheel()`: the real binary, the real token file, not a
    reimplementation of the CLI's auth call.
    """
    return subprocess.run(
        ["docker", "exec", "-e", "WHEEL_TOKEN_FILE=/data/run/%s/token" % node_id,
         "-e", "WHEEL_ENGINE_URL=http://127.0.0.1:7000", NAME, "wheel"] + list(argv),
        capture_output=True, text=True)


def wait_token(node_id, timeout=60):
    """The supervisor writes the token file when the agent starts."""
    for _ in range(int(timeout * 2)):
        p = subprocess.run(["docker", "exec", NAME, "test", "-s",
                            "/data/run/%s/token" % node_id], capture_output=True)
        if p.returncode == 0:
            return True
        time.sleep(0.5)
    return False


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
        # ---- a table node must own its table, not merely have created one once -------
        #
        # Found in production by the wheel-dev `pm` agent's first three commands: the
        # sqlite table is created at node CREATE (db/board.rs) and re-ensured nowhere, so a
        # board that survives a database restore keeps its nodes and loses its tables.
        # `wheel read reports` then says "no such table: t_reports" about a node that is
        # plainly on the board. A fresh project is fine, which is why nobody saw it.
        #
        # Dropped out of band with python3's sqlite3 (there is no sqlite3 binary in the
        # image) rather than by asking the engine to do it — the point is the state a
        # restore leaves behind, which the engine never agreed to.
        st, tbl = http("POST", "/v1/nodes",
                       {"name": "reports", "type": "table", "position": {"x": 0, "y": 0},
                        "config": {"columns": [{"name": "title", "type": "text"},
                                               {"name": "count", "type": "integer"}]}})
        tid = (tbl or {}).get("id")
        if not R.check("TBL-restore/setup", 200 <= st < 300 and tid,
                       "could not create the table node: %s %s" % (st, str(tbl)[:120])):
            return R.report("engine-validation")

        # Row writes are CLI/node-token gated (§4: the engine-secret realm exposes only
        # GET rows / POST query, never a write) -- an agent wired write->reports, not the
        # engine secret used everywhere else in this file.
        st, writer = http("POST", "/v1/nodes",
                          {"name": "reports-writer", "type": "agent",
                           "position": {"x": 0, "y": 0},
                           "config": {"harness": "claude", "system_prompt": "writer",
                                      "run_on_startup": False, "ephemeral_context": False}})
        wid = (writer or {}).get("id")
        if not R.check("TBL-restore/writer-setup", 200 <= st < 300 and wid,
                       "could not create the writer agent: %s %s" % (st, str(writer)[:120])):
            return R.report("engine-validation")
        st, _ = http("POST", "/v1/wires", {"from": wid, "to": tid, "type": "write"})
        if not R.check("TBL-restore/writer-wired", 200 <= st < 300,
                       "could not wire the writer agent to reports (write): %s" % st):
            return R.report("engine-validation")
        http("POST", "/v1/agents/%s/start" % wid)
        if not R.check("TBL-restore/writer-token", wait_token(wid),
                       "the writer agent never got a token file — cannot write a real row"):
            return R.report("engine-validation")

        p = wheel(wid, "write", "reports/r1", json.dumps({"title": "before", "count": 1}))
        R.check("TBL-restore/setup-row", p.returncode == 0,
                "the setup write itself failed (exit %d: %s) — every assertion below it "
                "would have been proving something about an empty table"
                % (p.returncode, (p.stderr or p.stdout).strip()[:160]))

        drop = subprocess.run(
            ["docker", "exec", NAME, "python3", "-c",
             "import sqlite3;c=sqlite3.connect('/data/wheel.db');"
             "c.execute('DROP TABLE IF EXISTS t_reports');c.commit();print('dropped')"],
            capture_output=True, text=True)
        if not R.check("TBL-restore/dropped", "dropped" in drop.stdout,
                       "could not drop t_reports out of band: %s" % (drop.stderr or "")[:160]):
            return R.report("engine-validation")

        st, rows = http("GET", "/v1/tables/%s/rows" % tid)
        R.check("WOW-table-survives-restart", st == 200,
                "reading a table node whose sqlite table is gone answered %s %s — the node "
                "exists on the board, so the engine must re-ensure its table rather than "
                "report `no such table` about something it is still showing the user"
                % (st, str(rows)[:160]))

        # Re-created EMPTY, and with the configured columns — a table rebuilt without its
        # columns is a different bug wearing the fix's clothes. Same wire, same token: a
        # re-ensured table is still the same NODE, so nothing about the writer changes.
        if st == 200:
            p2 = wheel(wid, "write", "reports/r2", json.dumps({"title": "after", "count": 2}))
            R.check("WOW-table-survives-restart/columns", p2.returncode == 0,
                    "the table came back but would not accept its own configured columns "
                    "(exit %d: %s) — it was recreated from something other than the node "
                    "config" % (p2.returncode, (p2.stderr or p2.stdout).strip()[:160]))
    finally:
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)

    return R.report("engine-validation")


if __name__ == "__main__":
    sys.exit(main())
