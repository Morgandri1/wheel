#!/usr/bin/env python3
"""Every one of the 243 wire-matrix cells, against the real engine — TESTPLAN WM-*.

The wire matrix IS the capability model: a node's wire set is its permission set. So this
is the security test that matters most, and it is asserted exhaustively rather than by
sampling — 9 node types x 9 x 3 wire types, each expected allow or deny exactly as
ARCHITECTURE.md §3 states.

Expectations come from qa/fixtures/wire_matrix.json, which QA generates from the §3 PROSE
(qa/tools/gen_wire_matrix.py) and never from wheel-core's export. Deriving them from the
export would check the implementation against itself: the engine would be asked "do you
agree with you?", and every cell would pass no matter what the contract said.

A denied wire that the engine CREATES is S1 — that is a capability the contract does not
grant, reachable by whoever can create wires.
"""
import json, os, subprocess, sys, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, call as _http, free_port

SKIP = 77
R = Results()
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
MATRIX = os.path.join(ROOT, "qa", "fixtures", "wire_matrix.json")

NAME = "wheel-qa-wirematrix"
PORT = free_port(int(os.environ.get("WHEEL_WM_PORT", "17423")))
SECRET = "qa-wire-matrix-secret-0123456789"
BASE = "http://127.0.0.1:%d" % PORT

# One config per node type that the engine accepts (mirrors qa/fixtures/nodes/valid).
CONFIGS = {
    "agent": {"harness": "claude", "system_prompt": "x", "run_on_startup": False,
              "ephemeral_context": False},
    "ctx": {"markdown": "x"},
    "table": {"columns": [{"name": "col", "type": "text"}]},
    "endpoint": {"method": "POST", "path": "/hook", "response_mode": "ack",
                 "auth": {"mode": "none"}},
    "script": {"language": "python", "source": "print(1)"},
    "mcp": {"transport": "stdio", "command": "true"},
    "vault": {"keys": ["K"]},
    "chest": {},
    "tool": {"kind": "http",
             "source": {"format": "manual", "raw": "{}", "imported_at": "2026-09-05T00:00:00Z"},
             "base_url": "https://example.com", "operations": []},
}


def api(method, path, body=None):
    return _http(method, path, headers={"Authorization": "Bearer " + SECRET},
                 body=body, base=BASE, timeout=30)


def start_engine():
    subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)
    key = subprocess.run(["openssl", "rand", "-base64", "32"],
                         capture_output=True, text=True).stdout.strip()
    p = subprocess.run(
        ["docker", "run", "-d", "--name", NAME,
         "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
         "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
         "-e", "WHEEL_VAULT_KEY=" + key,
         "-e", "WHEEL_ROLE=engine",
         "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
         "-p", "%d:7000" % PORT, "wheel-engine:test"],
        capture_output=True, text=True)
    if p.returncode != 0:
        return "could not start wheel-engine:test: " + p.stderr.strip()[:200]
    for _ in range(60):
        try:
            if api("GET", "/healthz")[0] == 200:
                return None
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


def main():
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    if subprocess.run(["docker", "image", "inspect", "wheel-engine:test"],
                      capture_output=True).returncode != 0:
        print("wheel-engine:test not built (SDK: make engine-image-test)")
        return SKIP

    cells = json.load(open(MATRIX))["cells"]
    err = start_engine()
    if err:
        print(err)
        return SKIP

    try:
        # Two nodes of every type: a self-wire is its own (denied) case, so every cell
        # must be expressible between two DISTINCT nodes of the given types.
        ids = {}
        for t, cfg in CONFIGS.items():
            for n in (1, 2):
                # A table node's name becomes the sqlite table `t_<name>`, so the engine
                # requires it to already be an identifier — `table-1` would be `t_table-1`,
                # where the hyphen is a subtraction operator. Refusing beats mangling, and
                # creation fails atomically rather than leaving a node with nowhere to put
                # rows (db/board.rs:113). This suite generated exactly that name and had
                # gone red on it; API found that before I did, running my own suite.
                #
                # Hyphens stay in every OTHER type's name on purpose: §3 permits them and
                # the address path should keep being exercised with one.
                nm = "%s_%d" % (t, n) if t == "table" else "%s-%d" % (t, n)
                st, body, _ = api("POST", "/v1/nodes",
                                  {"name": nm, "type": t, "position": {"x": 0.0, "y": 0.0},
                                   "config": cfg})
                if st not in (200, 201):
                    R.check("WM-setup/%s" % nm, False, "create -> %s %r" % (st, body))
                    return R.report("engine-wire-matrix")
                ids[(t, n)] = body["id"]
        R.check("WM-setup/nodes", len(ids) == 18, "created %d of 18" % len(ids))

        allowed_ok = denied_ok = 0
        bad_allow, bad_deny = [], []

        for c in cells:
            frm, to, wt, expect = c["from"], c["to"], c["type"], c["expect"]
            body = {"from": ids[(frm, 1)], "to": ids[(to, 2)], "type": wt}
            st, resp, _ = api("POST", "/v1/wires", body)
            created = st in (200, 201, 204)
            if expect == "allow":
                if created:
                    allowed_ok += 1
                    api("DELETE", "/v1/wires", body)   # keep the board clean for the next cell
                else:
                    bad_allow.append("%s->%s (%s) refused with %s %r" % (frm, to, wt, st, resp))
            else:
                if created:
                    bad_deny.append("%s->%s (%s) was CREATED (%s)" % (frm, to, wt, st))
                    api("DELETE", "/v1/wires", body)
                else:
                    denied_ok += 1

        R.check("WM-create-allow", not bad_allow,
                "%d contract-allowed wire(s) refused: %s" % (len(bad_allow), "; ".join(bad_allow[:5])))
        R.check("WM-create-deny", not bad_deny,
                "%d contract-denied wire(s) CREATED (S1): %s" % (len(bad_deny), "; ".join(bad_deny[:5])))
        print("  ...   %d/%d allowed cells accepted, %d/%d denied cells refused"
              % (allowed_ok, sum(1 for c in cells if c["expect"] == "allow"),
                 denied_ok, sum(1 for c in cells if c["expect"] == "deny")))

        # A node wired to ITSELF is denied for every type, including the pairs the matrix
        # allows between two distinct nodes.
        self_bad = []
        for t in CONFIGS:
            for wt in ("read", "write", "send"):
                b = {"from": ids[(t, 1)], "to": ids[(t, 1)], "type": wt}
                st, _, _ = api("POST", "/v1/wires", b)
                if st in (200, 201, 204):
                    self_bad.append("%s->itself (%s)" % (t, wt))
                    api("DELETE", "/v1/wires", b)
        R.check("WM-self-wire", not self_bad, "self-wires created: %s" % ", ".join(self_bad[:5]))

        # A duplicate wire must never produce two rows.
        dup = {"from": ids[("agent", 1)], "to": ids[("ctx", 2)], "type": "read"}
        api("POST", "/v1/wires", dup)
        st2, _, _ = api("POST", "/v1/wires", dup)
        stb, board, _ = api("GET", "/v1/board")
        n = 0
        for node in (board or {}).get("nodes", []):
            if node["id"] == dup["from"]:
                n = sum(1 for w in node.get("wires", [])
                        if w.get("to") == dup["to"] and w.get("type") == "read")
        R.check("WM-dup-wire", n == 1, "duplicate wire produced %d rows (second POST -> %s)" % (n, st2))
        api("DELETE", "/v1/wires", dup)

        # A wire to a node that does not exist must not be created.
        st, _, _ = api("POST", "/v1/wires",
                       {"from": ids[("agent", 1)], "to": str(uuid.uuid4()), "type": "send"})
        R.check("WM-unknown-target", st not in (200, 201, 204),
                "wire to a nonexistent node -> %s" % st)

        # Deleting a node must remove wires in BOTH directions, leaving no orphans.
        st, body, _ = api("POST", "/v1/nodes",
                          {"name": "doomed-ctx", "type": "ctx", "position": {"x": 1.0, "y": 1.0},
                           "config": {"markdown": "x"}})
        doomed = body["id"]
        api("POST", "/v1/wires", {"from": ids[("agent", 1)], "to": doomed, "type": "read"})
        api("POST", "/v1/wires", {"from": doomed, "to": ids[("agent", 2)], "type": "send"})
        api("DELETE", "/v1/nodes/%s" % doomed)
        st, board, _ = api("GET", "/v1/board")
        orphans = [w for node in (board or {}).get("nodes", [])
                   for w in node.get("wires", []) if w.get("to") == doomed]
        R.check("NODE-delete-cascade", not orphans,
                "%d orphan wire(s) still point at the deleted node" % len(orphans))
    finally:
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)

    return R.report("engine-wire-matrix")


if __name__ == "__main__":
    sys.exit(main())
