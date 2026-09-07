#!/usr/bin/env python3
"""EPH-* — an ephemeral agent must settle after a turn, exactly like a normal one.

PM's measurement on the live deployment, and it is the operator's own agent:

    pm         ephemeral=True    -> stuck in `starting` indefinitely
    sdk/api/web/qa/adversary     ephemeral=False -> all settle normally

One agent has the flag. One agent is stuck. Five others on the same engine and the same
deploy are fine. And the timing is the tell: the turn COMPLETED (the operator got his
reply at 01:08:11) and status went to `starting` two seconds later. That is not a start
that never finished -- it is a RESTART immediately after turn completion, which is what
ephemeral context clearing does. So the agent does not pass through `starting`; it LIVES
there, between every turn, forever.

THREE ASSERTIONS, AND THE ORDER MATTERS:

  1. CONTROL, non-ephemeral settles. If an ordinary agent cannot finish a turn on this
     engine, nothing below is about the flag and claiming it would be wrong. The pair is
     identical in every other respect and runs in the same engine, same run, so the flag
     is the only difference left to blame.

  2. NON-VACUITY, the clear actually happened. If `ephemeral_context` were silently
     ignored, the agent would settle perfectly and this suite would pass while testing
     nothing -- BUG-024's shape. The session id must CHANGE across the turn, because a
     cleared context is a new session. A pass without that is not evidence.

  3. Only then: the ephemeral agent reaches a settled status.

Deliberately NOT asserted: that the status never touches `starting`. Restarting is what
the flag is FOR, and a gate forbidding it would forbid the feature. The defect is not the
transition, it is failing to leave it -- which is also why ADVERSARY's in_flight-keyed
deadline is right and a time-in-`starting` deadline would kill this agent every turn.
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
NAME = "qa-engine-eph-%s" % uuid.uuid4().hex[:8]
SECRET = "qa-eph-secret-at-least-16chars"
IMAGE_TAG = os.environ.get("WHEEL_EPH_IMAGE", "wheel-engine:test")
IMAGE = None

SETTLED = {"idle", "parked", "stopped", "error", "needs_auth", "budget_exhausted", "running"}
SETTLE_SECS = float(os.environ.get("WHEEL_EPH_SETTLE_SECS", "90"))


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, body=None, timeout=30):
    req = urllib.request.Request(BASE + path, method=method)
    req.add_header("Authorization", "Bearer " + SECRET)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, data, timeout=timeout) as r:
            txt = r.read().decode(errors="replace")
            return r.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        return e.code, None
    except Exception:
        return 0, None


def state_of(node_id):
    st, board = http("GET", "/v1/board")
    for n in ((board or {}).get("nodes") or []):
        if n.get("id") == node_id:
            return n.get("state") or {}
    return {}


def settle(node_id, timeout=SETTLE_SECS):
    t0 = time.time()
    last = None
    while time.time() - t0 < timeout:
        last = state_of(node_id)
        if last.get("status") in SETTLED and last.get("queued_messages", 0) == 0:
            return True, last, time.time() - t0
        time.sleep(1.0)
    return False, last, time.time() - t0


def make_agent(name, ephemeral):
    st, node = http("POST", "/v1/nodes",
                    {"name": name, "type": "agent", "position": {"x": 0, "y": 0},
                     "config": {"harness": "claude", "system_prompt": "eph gate",
                                "run_on_startup": False,
                                "ephemeral_context": ephemeral}})
    return node["id"] if (200 <= st < 300 and node) else None


def start_engine():
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    sh("docker", "run", "-d", "--name", NAME,
       "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
       "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
       "-e", "WHEEL_VAULT_KEY=" + key,
       "-e", "WHEEL_ROLE=engine",
       "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
       "-p", "%d:7000" % PORT, IMAGE or IMAGE_TAG)
    for _ in range(60):
        if http("GET", "/healthz")[0] == 200:
            return True
        time.sleep(0.5)
    return False


def run_one_turn(aid):
    """Start, send one message, wait for it to be consumed. Returns (ok, state_before)."""
    http("POST", "/v1/agents/%s/start" % aid)
    for _ in range(60):
        if state_of(aid).get("status") in SETTLED:
            break
        time.sleep(1.0)
    before = state_of(aid)
    http("POST", "/v1/agents/%s/send" % aid, {"body": "turn-%s" % uuid.uuid4().hex[:6]})
    t0 = time.time()
    while time.time() - t0 < SETTLE_SECS:
        if state_of(aid).get("queued_messages", 0) == 0 and \
           state_of(aid).get("last_activity") != before.get("last_activity"):
            return True, before
        time.sleep(1.0)
    return False, before


def main():
    global IMAGE
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", IMAGE_TAG).returncode != 0:
        print("%s not built — run `make engine-image-test`" % IMAGE_TAG)
        return SKIP
    IMAGE = pin_image(IMAGE_TAG)
    print("image: %s -> %s" % (IMAGE_TAG, (IMAGE or "?")[:19]))

    try:
        if not R.control("EPH/engine-up", start_engine(),
                         "the engine never started, so nothing below is evidence"):
            return R.report("ephemeral-context")
        if err := configure_fakes(NAME, transcript="/data/eph.jsonl"):
            R.skip("EPH/fakes", err)
            return R.report("ephemeral-context")

        plain = make_agent("plain", False)
        eph = make_agent("ephemeral", True)
        if not R.control("EPH/agents-created", bool(plain and eph),
                         "could not create the agent pair; `ephemeral_context` may not be "
                         "an accepted config key, which is itself the finding"):
            return R.report("ephemeral-context")

        # 1. CONTROL — an ordinary agent completes a turn and settles on this engine.
        ran_plain, _ = run_one_turn(plain)
        ok_plain, st_plain, waited_plain = settle(plain)
        if not R.control("EPH/plain-settles", ran_plain and ok_plain,
                         "the NON-ephemeral agent did not settle after a turn either "
                         "(status %r after %.0fs). Whatever is wrong is not the flag, so "
                         "attributing it to ephemeral_context would be wrong."
                         % ((st_plain or {}).get("status"), waited_plain)):
            return R.report("ephemeral-context")

        # 2. NON-VACUITY — the clear actually happened.
        before_eph = state_of(eph)
        ran_eph, pre = run_one_turn(eph)
        after_eph = state_of(eph)
        sid_before = pre.get("session_id")
        sid_after = after_eph.get("session_id")
        R.control("EPH/context-was-cleared",
                  bool(sid_before) and sid_after != sid_before,
                  "the session id did not change across the turn (%r -> %r), so the "
                  "ephemeral clear did not happen. Either the flag is being ignored -- in "
                  "which case the settle check below would pass while testing nothing -- "
                  "or a cleared context reuses the session id and this control needs a "
                  "different signal. Not reported as a settle failure either way."
                  % (sid_before, sid_after))

        # 3. THE ASSERTION.
        ok_eph, st_eph, waited_eph = settle(eph)
        R.gated("EPH-settles-after-turn", "EPH/context-was-cleared", ok_eph,
                "the ephemeral agent is %r with %s queued after %.0fs, while the identical "
                "non-ephemeral agent settled to %r in %.0fs on the same engine in the same "
                "run. The turn COMPLETED -- this is the restart that follows it failing to "
                "arm whatever sets status past `starting`, so the agent LIVES in a "
                "transitional state between turns rather than passing through it."
                % ((st_eph or {}).get("status"), (st_eph or {}).get("queued_messages"),
                   waited_eph, (st_plain or {}).get("status"), waited_plain))

        R.gated("EPH-second-turn-still-works", "EPH/context-was-cleared",
                run_one_turn(eph)[0],
                "the ephemeral agent could not complete a SECOND turn. One turn proves the "
                "first clear survived; the operator's agent does this every turn, forever.")
        return R.report("ephemeral-context")
    finally:
        sh("docker", "rm", "-f", NAME)


if __name__ == "__main__":
    sys.exit(main())
