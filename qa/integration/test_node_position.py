#!/usr/bin/env python3
"""POS-* — a position that comes back different from what was stored is a UI bug.

PM's ruling (contract 710239f, "Position is an integer cell"):
  1. Cell size is 1. The engine ROUNDS on the way in, so a client sending 10.5 lands on 11
     and keeps working. Not a 40px grid -- a coarser cell would make every node visibly
     jump after save.
  2. Out of range CLAMPS, it does not reject, and the engine RETURNS the clamped value it
     stored. A node that appears to save, 400s, and springs back on the next refetch is the
     worst shape a UI bug can have.

WHAT THIS GATE ASSERTS, and it is deliberately not the arithmetic: THE TWO VIEWS MUST
AGREE. Whatever the engine decides a position is, the response to the write and a later
board refetch must say the SAME thing. Rounding and clamping are each implemented twice --
once in the engine, once in the client -- and the failure that reaches an operator is not
"the maths is wrong", it is the two halves drifting apart so a node moves on reload.

So every case below is checked in three places: what the write returned, what the board
returns afterwards, and -- for a value the client also clamps -- that they are identical.
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
from wheel_client import Results, free_port, pin_image, StaleImage  # noqa: E402

SKIP = 77
R = Results()
PORT = free_port(int(os.environ.get("WHEEL_POS_PORT", "17480")))
BASE = "http://127.0.0.1:%d" % PORT
NAME = "qa-engine-pos-%s" % uuid.uuid4().hex[:8]
SECRET = "qa-pos-secret-at-least-16chars"
IMAGE_TAG = os.environ.get("WHEEL_POS_IMAGE", "wheel-engine:test")
IMAGE = None

I16_MAX = 32767
I16_MIN = -32768

# (sent, expected, why it is here)
CASES = [
    (10.0, 10, "a whole number must survive untouched"),
    (10.4, 10, "rounds down to the nearest cell"),
    (10.6, 11, "rounds up to the nearest cell"),
    (-10.4, -10, "rounding is symmetric about zero"),
    (-10.6, -11, "rounding is symmetric about zero"),
    (0.0, 0, "zero is not a special case"),
    (99999.0, I16_MAX, "past the bound it CLAMPS and returns what it stored"),
    (-99999.0, I16_MIN, "the negative bound clamps too"),
    (float(I16_MAX), I16_MAX, "exactly the bound is in range"),
    (float(I16_MIN), I16_MIN, "exactly the bound is in range"),
]


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, body=None, timeout=20):
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


def start():
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


def board_position(node_id):
    st, board = http("GET", "/v1/board")
    if st != 200 or not board:
        return None
    for n in board.get("nodes", []):
        if n.get("id") == node_id:
            return n.get("position")
    return None


def main():
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", IMAGE_TAG).returncode != 0:
        print("%s not built — run `make engine-image-test`" % IMAGE_TAG)
        return SKIP
    global IMAGE
    IMAGE = pin_image(IMAGE_TAG)
    print("image: %s -> %s" % (IMAGE_TAG, (IMAGE or "?")[:19]))

    try:
        if not R.control("POS/engine-up", start(),
                         "the engine did not start, so nothing below is evidence"):
            return R.report("node-position")

        for sent, expected, why in CASES:
            tag = ("%g" % sent).replace("-", "neg").replace(".", "_")
            st, created = http("POST", "/v1/nodes",
                               {"name": "pos-%s" % tag.lower(), "type": "ctx",
                                "position": {"x": sent, "y": 0},
                                "config": {"markdown": "x"}})
            if not (200 <= st < 300 and created):
                # A REJECTION is itself a ruling violation: out of range clamps, it does
                # not 400. Reported as the failure it is, not skipped.
                R.check("POS-clamps-not-rejects/%s" % tag, False,
                        "sending x=%g was REJECTED with %s. The ruling is that out of "
                        "range clamps and returns what it stored: a 400 mid-drag is not "
                        "an improvement on a node that springs back. (%s)"
                        % (sent, st, why))
                continue

            wrote = (created.get("position") or {}).get("x")
            after = (board_position(created["id"]) or {}).get("x")

            # THE ASSERTION PM ASKED FOR: the two views must agree, whatever the value is.
            R.check("POS-write-matches-refetch/%s" % tag, wrote == after,
                    "the write returned x=%r and a board refetch returned x=%r for a node "
                    "created with x=%g. This is the drift that moves a node on reload, and "
                    "it is worse than either value being wrong on its own." % (wrote, after, sent))

            # And the arithmetic, second, so a disagreement is never reported as a rounding
            # bug and a rounding bug is never reported as a disagreement.
            R.check("POS-rounds-and-clamps/%s" % tag, wrote == expected,
                    "x=%g should be stored as %d (%s) but the engine returned %r"
                    % (sent, expected, why, wrote))

            # An integer cell means an INTEGER comes back, not 11.0 -- a float here is what
            # lets a client re-send 11.0 and start the cycle again.
            R.check("POS-is-an-integer/%s" % tag, isinstance(wrote, int),
                    "position came back as %r (%s); the contract says an integer cell"
                    % (wrote, type(wrote).__name__))

        # The same rules on the MOVE path, which is the one an operator actually exercises
        # by dragging, and which is a different code path from create.
        st, node = http("POST", "/v1/nodes",
                        {"name": "pos-move", "type": "ctx", "position": {"x": 0, "y": 0},
                         "config": {"markdown": "x"}})
        if not R.control("POS/move-node-created", 200 <= st < 300 and bool(node),
                         "could not create the node to drag (%s)" % st):
            return R.report("node-position")
        st, patched = http("PATCH", "/v1/nodes/%s" % node["id"],
                           {"position": {"x": 99999.0, "y": -99999.0}})
        if 200 <= st < 300 and patched:
            moved = patched.get("position") or {}
            refetched = board_position(node["id"]) or {}
            R.check("POS-move-write-matches-refetch",
                    moved.get("x") == refetched.get("x") and moved.get("y") == refetched.get("y"),
                    "PATCH returned %r and the board returned %r — dragging a node past the "
                    "bound leaves the two views disagreeing" % (moved, refetched))
            R.check("POS-move-clamps",
                    moved.get("x") == I16_MAX and moved.get("y") == I16_MIN,
                    "dragging past the bound should clamp to (%d, %d); PATCH returned %r"
                    % (I16_MAX, I16_MIN, moved))
        else:
            R.check("POS-move-clamps", False,
                    "PATCH with an out-of-range position answered %s; the ruling is clamp, "
                    "not reject" % st)

        return R.report("node-position")
    finally:
        sh("docker", "rm", "-f", NAME)


if __name__ == "__main__":
    from wheel_client import run_suite
    sys.exit(run_suite(main, "node-position", container=NAME))
