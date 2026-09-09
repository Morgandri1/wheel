#!/usr/bin/env python3

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""HEALTH-implies-* — /healthz answering 200 must MEAN something.

Named for the shape, not the bugs, because the point is the sixth instance. Five in one
day, every one of them a system that was up, answering, and not doing its job:

    the escaper panic     /healthz 200, delivery task dead (tokio unwinds the TASK)
    the ingress drain     agent `idle`, queue not moving
    the ephemeral restart status `starting`, turns completing underneath
    BUG-031               engine boots, /healthz 200, /v1/board returns 500
    Web's auth default    NEXT_PUBLIC_AUTH_MODE unset -> `mock`; builds, renders,
                          looks deployed, 401s on the first real request

Each FIX made the failure quieter rather than absent. That is the class, and it is worse
than the bugs: a loud failure is found in minutes by whoever is looking at it, and a quiet
one is found in 52 minutes by the operator on his own board.

THE PREDICATE (PM):

    for each capability the engine claims to serve, /healthz answering 200 must IMPLY
    that capability works.

WHAT THIS SUITE MUST NOT CAUSE, and it is the whole of the ruling. The obvious conclusion
from these five is "healthz should check more things". It is WRONG. The host restarts a
sandbox whose healthz fails (ARCHITECTURE.md 4b, 10s budget), so a healthz that touched the
board would turn one slow read or one unparseable row into a RESTART LOOP — and we watched
a poison message take a board down through repeated reboots this morning. A health check
that fails on a transient makes an outage out of a hiccup.

So: /healthz stays cheap and stays a liveness claim. THIS SUITE asserts the implication its
greenness is supposed to carry. The lie gets caught in CI, where a false negative costs a
rerun, instead of in production, where it costs a restart loop.

TWO RULES BUILT IN, both from PM and both learned the hard way tonight:

  1. A capability that CANNOT BE PROBED fails, it does not skip. "Could not check the
     board" and "the board is fine" read identically to a human, which is this suite's own
     failure mode one level up.
  2. Failures name the capability AND what it returned. BUG-031's value was entirely in
     `/v1/board -> 500 "invalid character: found n at 0"` — the error text is what told SDK
     it was id parsing rather than a lock or a timeout.

NON-VACUITY. On a clean engine every capability works, so a suite that only probed a
healthy board would pass forever and prove nothing. Each capability is therefore checked in
TWO states: clean, and after a PERTURBATION that has historically produced exactly this lie
— a single node row whose id is unparseable. healthz stays 200 in both. Anything that stops
serving in the second state while healthz stays green is the class, caught.
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, configure_fakes, free_port, pin_image  # noqa: E402

SKIP = 77
R = Results()
PORT = free_port(0)
BASE = "http://127.0.0.1:%d" % PORT
NAME = "qa-engine-health-%s" % uuid.uuid4().hex[:8]
VOLUME = NAME + "-data"
SECRET = "qa-health-secret-at-least-16chars"
IMAGE_TAG = os.environ.get("WHEEL_HEALTH_IMAGE", "wheel-engine:test")
IMAGE = None
IDS = {}


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, body=None, timeout=25):
    """Returns (status, parsed_or_text). status 0 means the request itself failed —
    a hang or a refused connection, which is a capability failure like any other."""
    req = urllib.request.Request(BASE + path, method=method)
    req.add_header("Authorization", "Bearer " + SECRET)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=timeout) as r:
            txt = r.read().decode(errors="replace")
            try:
                return r.status, json.loads(txt) if txt.strip() else None
            except ValueError:
                return r.status, txt[:300]
    except urllib.error.HTTPError as e:
        try:
            return e.code, e.read().decode(errors="replace")[:300]
        except Exception:
            return e.code, None
    except Exception as e:
        return 0, "%s: %s" % (type(e).__name__, str(e)[:160])


def healthz():
    return http("GET", "/healthz")[0] == 200


def start_engine():
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    sh("docker", "run", "-d", "--name", NAME,
       "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
       "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
       "-e", "WHEEL_VAULT_KEY=" + key,
       "-e", "WHEEL_ROLE=engine",
       "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
       "-e", "WHEEL_LOG=json",
       "-v", "%s:/data" % VOLUME,
       "-p", "%d:7000" % PORT, IMAGE or IMAGE_TAG)
    for _ in range(60):
        if healthz():
            return True
        time.sleep(0.5)
    return False


def stop_engine():
    sh("docker", "stop", "-t", "15", NAME)
    sh("docker", "rm", "-f", NAME)


def build_board():
    made = {}
    for name, ntype, cfg in (
        ("notes", "ctx", {"markdown": "# notes"}),
        ("readings", "table", {"columns": [{"name": "v", "type": "text"}]}),
        ("secrets", "vault", {"keys": ["API_KEY"]}),
        ("files", "chest", {}),
        ("worker", "agent", {"harness": "claude", "system_prompt": "health gate",
                             "run_on_startup": False, "ephemeral_context": False}),
    ):
        st, node = http("POST", "/v1/nodes",
                        {"name": name, "type": ntype,
                         "position": {"x": 0, "y": 0}, "config": cfg})
        if 200 <= st < 300 and isinstance(node, dict):
            made[name] = node["id"]
    return made


# Probe verdicts. The middle one is the distinction PM's rule needs and my first version
# lacked: the predicate is "each capability the engine CLAIMS to serve", and a route that
# is documented for a later milestone and returns 404 is not a claim. Asserting it made
# HEALTH-implies-chest-read fail against an engine behaving exactly to spec, and — worse —
# labelled it PENDING BUG-031 in the poisoned state, attributing an unimplemented M2 route
# to an open S1. A gate that misattributes is not better than one that misses.
OK, LIE, UNCLAIMED = "ok", "lie", "unclaimed"


def verdict(status, detail):
    """404 on a route whose NODE WE JUST CREATED means the route is absent, not the node.
    5xx, a hang, or a refused connection is the lie this suite hunts.

    KNOWN GAP, deliberately left rather than papered over (PM, 2026-09-07): this judges the
    STATUS, not the SHAPE. A capability that is only half-implemented and answers `200 {}`
    is reported OK here, and that is the exact failure mode the invariant forbids — an
    unimplemented capability must never answer with a success shape, because the honest 404
    is detectable and the stub 200 is not.

    The `board` probe already does the right thing (200 with zero nodes on a board we just
    populated is a LIE, not OK). The others do not, because I do not yet know the response
    shape of a capability that has not landed — chest is M2 and currently, correctly, 404s.
    When it lands, each probe gets a shape assertion and this comment goes away. Writing it
    down beats a gate that silently accepts a stub."""
    if status == 200:
        return OK, detail
    if status == 404:
        return UNCLAIMED, detail
    return LIE, detail


def capabilities():
    """(label, callable) -> (verdict, description_of_what_it_returned).

    Every probe reports what it SAW, not just whether it liked it, because the error text is
    the thing that tells an owner which subsystem lied.
    """
    def board():
        st, body = http("GET", "/v1/board")
        nodes = (body or {}).get("nodes") if isinstance(body, dict) else None
        d = "GET /v1/board -> %s, %s" % (st, ("%d nodes" % len(nodes))
                                          if isinstance(nodes, list) else repr(body)[:200])
        if st == 200 and not (isinstance(nodes, list) and nodes):
            return LIE, d + " (200 with no nodes on a board we just populated)"
        return verdict(st, d)

    def table_rows():
        tid = IDS.get("readings")
        if not tid:
            return LIE, "the table node was never created, so the capability is UNPROBED — and unprobed must not read as fine"
        st, body = http("GET", "/v1/tables/%s/rows?limit=10" % tid)
        return verdict(st, "GET /v1/tables/:id/rows -> %s, %s" % (st, repr(body)[:200]))

    def chest_ls():
        cid = IDS.get("files")
        if not cid:
            return LIE, "the chest node was never created, so the capability is UNPROBED — and unprobed must not read as fine"
        st, body = http("GET", "/v1/chests/%s/ls" % cid)
        return verdict(st, "GET /v1/chests/:id/ls -> %s, %s (chest routes are M2 in "
                           "PROTOCOL.md and not implemented yet)" % (st, repr(body)[:160]))

    def vault_write():
        vid = IDS.get("secrets")
        if not vid:
            return LIE, "the vault node was never created, so the capability is UNPROBED — and unprobed must not read as fine"
        st, body = http("PUT", "/v1/vault/%s/API_KEY" % vid, {"value": "health-probe"})
        st = 200 if 200 <= st < 300 else st
        # WRITE, not read: vault values are read agent-side (`wheel secret get`) and never
        # served by the control plane, so a read probe here would be testing a route that
        # is not supposed to exist. engine-vault covers the agent path. Named accurately
        # rather than labelled "vault read" to match PM's list.
        return verdict(st, "PUT /v1/vault/:id/:key -> %s, %s" % (st, repr(body)[:160]))

    def agent_inbox():
        aid = IDS.get("worker")
        if not aid:
            return LIE, "the agent node was never created, so the capability is UNPROBED — and unprobed must not read as fine"
        st, body = http("GET", "/v1/agents/%s/inbox?limit=10" % aid)
        return verdict(st, "GET /v1/agents/:id/inbox -> %s, %s" % (st, repr(body)[:200]))

    def agent_log():
        aid = IDS.get("worker")
        if not aid:
            return LIE, "the agent node was never created, so the capability is UNPROBED — and unprobed must not read as fine"
        st, body = http("GET", "/v1/agents/%s/log" % aid)
        return verdict(st, "GET /v1/agents/:id/log -> %s, %s" % (st, repr(body)[:200]))

    return [("board-read", board), ("table-read", table_rows), ("chest-read", chest_ls),
            ("vault-write", vault_write), ("agent-inbox-read", agent_inbox),
            ("agent-log-read", agent_log)]


POISON = r'''
import sqlite3, json, datetime
db = sqlite3.connect("/data/wheel.db")
now = datetime.datetime.now(datetime.timezone.utc).isoformat()
db.execute("INSERT INTO nodes (id,name,type,config,x,y,created_at,updated_at) "
           "VALUES (?,?,?,?,?,?,?,?)",
           ("not-a-uuid", "legacy-row", "ctx",
            json.dumps({"markdown": "id is not a uuid"}), 1.0, 1.0, now, now))
db.commit()
print("poisoned")
'''


def assert_all(state, pending_bug=None):
    """Assert the implication for every capability in the current state."""
    alive = healthz()
    if not R.control("HEALTH/healthz-green-%s" % state, alive,
                     "/healthz is NOT 200 in the %s state. The implication under test has a "
                     "false antecedent, so nothing below would mean anything — and an engine "
                     "that is honestly down is not this suite's bug." % state):
        return
    for label, probe in capabilities():
        got, saw = probe()
        tid = "HEALTH-implies-%s/%s" % (label, state)
        if got == UNCLAIMED:
            # The engine makes no claim here, so healthz's greenness promises nothing about
            # it and there is no implication to violate. Self-arming: the day the route is
            # implemented it returns something other than 404 and this asserts for real,
            # so it cannot rot into a permanently excused capability.
            R.skip(tid, "not claimed by this engine — %s" % saw)
            continue
        detail = ("/healthz says 200 but %s does not work in the %s state: %s. That is the "
                  "class this suite exists for -- the engine is up, answering, and lying "
                  "about what it can do. A caller has no way to tell this from healthy."
                  % (label, state, saw))
        if pending_bug and got == LIE:
            R.pending(tid, False, pending_bug, detail)
        else:
            R.check(tid, got == OK, detail)


def main():
    global IMAGE, IDS
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", IMAGE_TAG).returncode != 0:
        print("%s not built — run `make engine-image-test`" % IMAGE_TAG)
        return SKIP
    IMAGE = pin_image(IMAGE_TAG)
    print("image: %s -> %s" % (IMAGE_TAG, (IMAGE or "?")[:19]))

    try:
        if not R.control("HEALTH/engine-up", start_engine(),
                         "the engine never started, so there is no health claim to test"):
            return R.report("health-implies")
        configure_fakes(NAME, transcript="/data/health.jsonl")

        IDS = build_board()
        if not R.control("HEALTH/board-built", len(IDS) == 5,
                         "could not create the node set (%s of 5: %s), so several "
                         "capabilities would be reported unprobed and an unprobed "
                         "capability is a FAILURE here, not a skip -- which would produce "
                         "a confusing red about the wrong thing"
                         % (len(IDS), sorted(IDS))):
            return R.report("health-implies")

        # STATE 1: clean. The baseline the implication is supposed to hold in trivially.
        assert_all("clean")

        # STATE 2: one unparseable node row — the perturbation that has actually produced
        # this lie in production. healthz stays 200 through it.
        stop_engine()
        rc, out = in_db_poison()
        if not R.control("HEALTH/poison-applied", rc == 0 and "poisoned" in out,
                         "could not write the malformed row, so the interesting half of "
                         "this suite did not run: %s" % out.strip()[-200:]):
            return R.report("health-implies")
        if not R.control("HEALTH/engine-up-poisoned", start_engine(),
                         "the engine did not come back up with one malformed row. That is "
                         "a boot failure rather than a health lie -- loud, and covered by "
                         "POS-migration-boots-past-unparseable-id."):
            return R.report("health-implies")
        # No pending_bug any more: BUG-031 is fixed (0417a5e), so a failure here is a
        # REGRESSION and must be red. Leaving the marker in place would quietly re-pend a
        # bug that had come back — a gate that downgrades its own findings.
        assert_all("one-bad-row")

        return R.report("health-implies")
    finally:
        stop_engine()
        sh("docker", "volume", "rm", "-f", VOLUME)


def in_db_poison():
    p = sh("docker", "run", "--rm", "-v", "%s:/data" % VOLUME,
           "--entrypoint", "python3", IMAGE or IMAGE_TAG, "-c", POISON)
    return p.returncode, (p.stdout or "") + (p.stderr or "")


if __name__ == "__main__":
    sys.exit(main())
