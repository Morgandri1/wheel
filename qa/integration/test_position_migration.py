#!/usr/bin/env python3

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""POS-migration-* — the rows that already exist, not the ones a client sends.

PM asked the question this file answers: ~20 float rows are already stored on the
operator's board, and #22 turns `x`/`y` into integer cells. Does any node move where he
can see it?

WHY test_node_position.py DOES NOT COVER THIS, which is the whole reason this file
exists. That gate drives the WRITE path: a client sends 10.6, the engine rounds, the write
response and a later refetch must agree. Every case it has is a value travelling INWARD
through the API. A migration touches none of that. It rewrites rows that are already on
disk, written months ago by an engine that accepted floats, and it runs at boot before any
client says anything. A gate that only sends values can be fully green while the migration
mangles every row already there.

THE THREE WAYS THIS GOES WRONG, in increasing order of how much the operator notices:

  1. TRUNCATION INSTEAD OF ROUNDING. `nodes` is a STRICT table with `x REAL NOT NULL`.
     sqlite cannot change a column's type in place, so REAL -> INTEGER is a table rebuild,
     and the obvious way to write the copy is `CAST(x AS INTEGER)`. CAST TRUNCATES TOWARD
     ZERO: 10.6 becomes 10, not 11, and -10.6 becomes -10, not -11. The engine's write path
     ROUNDS. So the migration and the write path would disagree by one cell -- the exact
     "two halves drift apart" shape the POS gate was built around, arriving through the one
     door that gate does not watch.

  2. A ROW THAT WILL NOT LOAD. The i16 bound is enforced in Rust, not in sqlite, whose
     INTEGER is 64-bit. A stored 99999.0 migrates to a perfectly legal sqlite 99999 and
     then fails to deserialize into an i16 position. That does not move a node; it drops
     one off the board, or refuses the boot. Worse than a jump and much harder to read.

  3. A VISIBLE JUMP, which is the one PM asked about and the only one bounded by geometry.
     Rounding moves a node at most 0.5 cells per axis (0.707 diagonal). The board renders
     one cell as one CSS pixel and caps zoom at 1.8 (canvas.tsx maxZoom), so the worst case
     a human can be shown is 1.27 px. That is not visible. CLAMPING is unbounded and very
     visible -- a node stored past the bound lands on it from wherever it was -- but it can
     only fire on a row already outside +/-32767.

So the honest summary is: rounding is safe by geometry and needs no gate to promise it;
what needs gating is that no row is truncated, no row is lost, and any row that DOES clamp
is reported rather than silently teleported.

CONTROL. The interesting assertions only mean something once #22 has landed. Before it,
the engine stores and returns floats, every displacement is exactly 0, and every check
below would pass while testing nothing -- a vacuous green of precisely the kind that put
BUG-024 in this repo. So `POS-migration/is-integer` is a CONTROL: if positions come back
as floats the migration has not run, and everything downstream SKIPS instead of passing.
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
from wheel_client import Results, free_port, pin_image  # noqa: E402

SKIP = 77
R = Results()
PORT = free_port(int(os.environ.get("WHEEL_POSMIG_PORT", "0")))
BASE = "http://127.0.0.1:%d" % PORT
NAME = "qa-engine-posmig-%s" % uuid.uuid4().hex[:8]
VOLUME = NAME + "-data"
SECRET = "qa-posmig-secret-at-least-16chars"
IMAGE_TAG = os.environ.get("WHEEL_POSMIG_IMAGE", "wheel-engine:test")
IMAGE = None

I16_MAX, I16_MIN = 32767, -32768
MAX_ZOOM = 1.8          # web/src/components/board/canvas.tsx
VISIBLE_PX = 3.0        # below this a "jump" is not something an operator can perceive

# (x, y, expect_clamped, why this row is here)
# The float values are the shape a dragged node actually has: xyflow reports fractional
# coordinates, so a board the operator has touched is full of these.
ROWS = [
    (120.0,    340.0,    False, "a whole number that must not move at all"),
    (120.4,    340.4,    False, "rounds down; CAST would agree here, which is why this "
                                "case alone cannot detect truncation"),
    (120.6,    340.6,    False, "rounds UP to 121 -- CAST gives 120 and this is the case "
                                "that catches a truncating migration"),
    (-120.6,   -340.6,   False, "negative rounds AWAY from zero to -121; CAST gives -120"),
    (0.5,      -0.5,     False, "the tie case at the origin, where a sign error hides"),
    (32766.6,  -32767.6, False, "rounds to exactly the bound without crossing it"),
    (99999.0,  -99999.0, True,  "already out of range: must clamp, and must SAY it did"),
]


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, timeout=20):
    req = urllib.request.Request(BASE + path, method=method)
    req.add_header("Authorization", "Bearer " + SECRET)
    try:
        with urllib.request.urlopen(req, None, timeout=timeout) as r:
            txt = r.read().decode(errors="replace")
            return r.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        return e.code, None
    except Exception:
        return 0, None


def run_engine():
    sh("docker", "run", "-d", "--name", NAME,
       "-e", "WHEEL_PROJECT_ID=" + PROJECT,
       "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
       "-e", "WHEEL_VAULT_KEY=" + VAULT_KEY,
       "-e", "WHEEL_ROLE=engine",
       "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
       "-v", "%s:/data" % VOLUME,
       "-p", "%d:7000" % PORT, IMAGE or IMAGE_TAG)
    for _ in range(60):
        if http("GET", "/healthz")[0] == 200:
            return True
        time.sleep(0.5)
    return False


def boot_log():
    return sh("docker", "logs", NAME).stdout + sh("docker", "logs", NAME).stderr


def stop_engine():
    sh("docker", "stop", "-t", "20", NAME)
    sh("docker", "rm", "-f", NAME)


def in_db(script):
    """Run python against /data/wheel.db inside a throwaway container on the same volume.

    Not `docker exec`: the engine has to be STOPPED while the rows are rewritten, or the
    test is racing the process it is setting up. A separate container on the same volume is
    the only way to touch the file with nothing else holding it.
    """
    p = sh("docker", "run", "--rm", "-v", "%s:/data" % VOLUME,
           "--entrypoint", "python3", IMAGE or IMAGE_TAG, "-c", script)
    return p.returncode, (p.stdout or "") + (p.stderr or "")


# Ids are real UUIDs. The engine parses `nodes.id` as a Uuid on load, so a readable
# fixture id like "mig-0000" makes the engine refuse to boot with
# `Conversion error from type Text at index: 0, invalid character: found 'm'` -- which
# looks exactly like "the migration cannot read its own data" and is entirely the test's
# own doing. Caught on the first real run; the migration had in fact worked.
SEED = r'''
import sqlite3, json, datetime
rows = json.loads(%r)
db = sqlite3.connect("/data/wheel.db")
now = datetime.datetime.now(datetime.timezone.utc).isoformat()
for nid, name, x, y in rows:
    db.execute("INSERT INTO nodes (id,name,type,config,x,y,created_at,updated_at) "
               "VALUES (?,?,?,?,?,?,?,?)",
               (nid, name, "ctx",
                json.dumps({"markdown": "seeded by POS-migration"}), x, y, now, now))
db.commit()
print("seeded", len(rows))
'''

BAD_ID_SEED = r'''
import sqlite3, json, datetime
db = sqlite3.connect("/data/wheel.db")
now = datetime.datetime.now(datetime.timezone.utc).isoformat()
db.execute("INSERT INTO nodes (id,name,type,config,x,y,created_at,updated_at) "
           "VALUES (?,?,?,?,?,?,?,?)",
           ("mig-0000-not-a-uuid", "mig-legacy-row", "ctx",
            json.dumps({"markdown": "id is not a uuid"}), 5.5, 5.5, now, now))
db.commit()
print("seeded 1")
'''

READ = r'''
import sqlite3, json
db = sqlite3.connect("/data/wheel.db")
out = {}
for nid, x, y in db.execute("SELECT id,x,y FROM nodes WHERE name LIKE 'mig-node-%'"):
    out[nid] = [x, y, type(x).__name__, type(y).__name__]
print(json.dumps(out))
'''


def expected_cell(v):
    """The ruling: round to nearest whole cell, then clamp. Deliberately NOT `round()` --
    python rounds halves to even, so round(0.5) is 0 and round(1.5) is 2. The ruling says
    nearest cell, and a tie has to go somewhere predictable and symmetric."""
    import math
    r = math.floor(v + 0.5) if v >= 0 else math.ceil(v - 0.5)
    return max(I16_MIN, min(I16_MAX, int(r)))


def main():
    global IMAGE, PROJECT, VAULT_KEY
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", IMAGE_TAG).returncode != 0:
        print("%s not built — run `make engine-image-test`" % IMAGE_TAG)
        return SKIP
    IMAGE = pin_image(IMAGE_TAG)
    PROJECT = str(uuid.uuid4())
    VAULT_KEY = sh("openssl", "rand", "-base64", "32").stdout.strip()
    print("image: %s -> %s" % (IMAGE_TAG, (IMAGE or "?")[:19]))

    global IDS
    IDS = [str(uuid.uuid4()) for _ in ROWS]

    try:
        # 1. A board that exists, so the schema is real rather than hand-built.
        if not R.control("POS-migration/engine-up", run_engine(),
                         "the engine never started, so there is no database to migrate "
                         "and nothing below is evidence"):
            return R.report("position-migration")
        stop_engine()

        # 2. Rows exactly as a pre-#22 engine left them: floats, straight into sqlite.
        seed_rows = [[IDS[i], "mig-node-%04d" % i, x, y]
                     for i, (x, y, _c, _w) in enumerate(ROWS)]
        rc, out = in_db(SEED % json.dumps(seed_rows))
        if not R.control("POS-migration/rows-seeded", rc == 0 and "seeded" in out,
                         "could not write float rows into the database, so the migration "
                         "has nothing to act on: %s" % out.strip()[-300:]):
            return R.report("position-migration")

        # 3. Boot again. This is where a migration runs.
        booted = run_engine()
        log = boot_log()
        if not R.control("POS-migration/engine-restarts", booted,
                         "the engine did NOT come back up after float rows were written. "
                         "If a migration refuses to boot on the data already on disk that "
                         "is the finding, not a skip. Boot log tail:\n%s"
                         % log[-1200:]):
            return R.report("position-migration")

        rc, out = in_db(READ)
        stored = json.loads(out.strip().splitlines()[-1]) if rc == 0 else {}
        st, board = http("GET", "/v1/board")
        by_id = {n["id"]: n for n in (board or {}).get("nodes", [])}

        # 4. THE CONTROL. Everything after this is vacuous while positions are floats.
        # WHOLE VALUES, not integer STORAGE. sqlite keeps the column's REAL affinity and
        # the migration snaps the values inside it, so a correctly migrated 341 reads back
        # from python as 341.0 -- type `float`. Asserting the python type failed here
        # against an engine that had done exactly the right thing, and the boot log said so
        # in the same run: "snapped stored positions to whole cells, nodes: 6". The Rust
        # type is i16 either way; what the migration owes us is that no fraction survives.
        def whole(v):
            return isinstance(v, (int, float)) and float(v).is_integer()
        all_whole = bool(stored) and all(whole(v[0]) and whole(v[1])
                                         for v in stored.values())
        R.control("POS-migration/is-integer", all_whole,
                  "stored positions still carry fractions, so the migration has not run "
                  "and every check below would pass by doing nothing. Values seen: %s"
                  % sorted({(v[0], v[1]) for v in stored.values()})[:6])

        # 5. No row may vanish. A rebuild that drops rows is the worst outcome and the
        #    easiest to miss, because a board with 19 of 20 nodes still looks like a board.
        R.gated("POS-migration-loses-no-node", "POS-migration/is-integer",
                len(stored) == len(ROWS) and len(by_id) >= len(ROWS),
                "seeded %d rows, %d survived on disk and %d are on the board. A table "
                "rebuild that drops rows leaves a board that still looks plausible."
                % (len(ROWS), len(stored), len(by_id)))

        # 6. Rounding, not truncation. The 120.6 and -120.6 rows are the ones that tell
        #    these two apart; the rest agree under either rule.
        for i, (x, y, want_clamp, why) in enumerate(ROWS):
            nid = IDS[i]
            got = stored.get(nid)
            tag = "mig-node-%04d/%g,%g" % (i, x, y)
            if not got:
                R.gated("POS-migration-loses-no-node", "POS-migration/is-integer", False,
                        "row %s (%s) is gone after migration" % (tag, why))
                continue
            gx, gy, _, _ = got
            wx, wy = expected_cell(x), expected_cell(y)
            trunc = (int(x), int(y))
            detail = ("%s: stored (%g,%g) became (%s,%s); the ruling says (%d,%d). "
                      "%s" % (tag, x, y, gx, gy, wx, wy, why))
            if (gx, gy) == trunc and (wx, wy) != trunc:
                detail += (" This is exactly CAST(x AS INTEGER) truncating toward zero "
                           "instead of rounding, so the migration and the write path "
                           "now disagree by one cell on every dragged node.")
            R.gated("POS-migration-rounds-not-truncates", "POS-migration/is-integer",
                    (gx, gy) == (wx, wy), detail)

            # 7. What the API serves must equal what is on disk. A migration that fixes
            #    the row but leaves a cached board is the same drift, one layer up.
            node = by_id.get(nid)
            pos = (node or {}).get("position") or {}
            R.gated("POS-migration-board-matches-disk", "POS-migration/is-integer",
                    node is not None and pos.get("x") == gx and pos.get("y") == gy,
                    "%s: disk says (%s,%s) but /v1/board says %s. A row that is right on "
                    "disk and wrong in the response is still a node in the wrong place."
                    % (tag, gx, gy, pos or "the node is absent"))

            # 8. The only case an operator can actually see.
            moved = max(abs(gx - x), abs(gy - y)) if isinstance(gx, (int, float)) else 0
            if want_clamp:
                # PROMOTED from PENDING. BUG-029 is fixed (6852068: the migration names
                # every node it clamps, and where it moved from). The marker did exactly
                # what it was built to do -- it went RED the moment the fix landed and
                # demanded this promotion, rather than staying quietly green on a gate
                # that had stopped gating.
                # ASSERT WHAT THE LINE CARRIES, not that the word appears. The pending
                # marker used `name in log or "clamp" in log.lower()`. The second disjunct
                # was fine for DETECTING the bug -- it only had to notice nothing specific
                # was logged -- but as a check it passes on the old COUNT line, i.e. on
                # exactly the output BUG-029 was filed against. An assertion that cannot
                # fail for the thing it names is the shape 0b refuses, and PM caught it.
                #
                # SDK emits node=<name> from_x/from_y/to_x/to_y moved_cells, so the
                # specific assertion is available: the node, where it was, where it went.
                name_i = "mig-node-%04d" % i
                logged = (name_i in log
                          and str(expected_cell(x)) in log and str(expected_cell(y)) in log
                          and str(int(x)) in log and str(int(y)) in log)
                R.gated("POS-migration-clamp-is-reported", "POS-migration/is-integer",
                        logged,
                        "%s was outside the bound and moved %.0f cells (%.0f px at the "
                        "board's max zoom of %.1f). That is a node teleporting across the "
                        "screen, and the boot log never mentions it. Clamping is correct; "
                        "doing it silently is not." % (tag, moved, moved * MAX_ZOOM,
                                                       MAX_ZOOM))
            else:
                R.gated("POS-migration-no-visible-jump", "POS-migration/is-integer",
                        moved * MAX_ZOOM < VISIBLE_PX,
                        "%s moved %.3f cells = %.2f px at max zoom %.1f, over the %.0f px "
                        "an operator can notice. Rounding alone should never exceed 0.5 "
                        "cells." % (tag, moved, moved * MAX_ZOOM, MAX_ZOOM, VISIBLE_PX))

        # 9. Idempotence. A migration that re-applies its transform every boot walks a
        #    node one cell at a time, which looks like nothing until it looks like chaos.
        stop_engine()
        again = run_engine()
        rc2, out2 = in_db(READ)
        second = json.loads(out2.strip().splitlines()[-1]) if rc2 == 0 else {}
        R.gated("POS-migration-is-idempotent", "POS-migration/is-integer",
                again and second == stored,
                "a second boot changed the positions again. A migration that runs on every "
                "start drifts a board one cell per restart. First boot: %s. Second: %s"
                % (json.dumps(stored)[:300], json.dumps(second)[:300]))

        # A ROW WHOSE ID IS NOT A UUID. This started as a mistake in my fixture -- I seeded
        # ids like `mig-0000` for readability -- and it stopped the engine booting at all:
        # board::list parsed every row's id and failed WHOLE on the first unparseable one.
        # SDK fixed the intolerance and asked me NOT to quietly switch to uuids, because
        # the unrealistic id is what made it visible, and a partial restore or a hand-edited
        # row would produce exactly this shape without a test around it. So the uuids stay
        # for the arithmetic above, and the awkward id comes back here as its own case.
        stop_engine()
        rc, out = in_db(BAD_ID_SEED)
        if R.control("POS-migration/bad-id-seeded", rc == 0 and "seeded" in out,
                     "could not write a row with an unparseable id: %s"
                     % out.strip()[-200:]):
            booted_bad = run_engine()
            log2 = boot_log()
            # control(), not check(): this is both a finding in its own right AND the
            # thing the next assertion depends on. Registered with check() it failed the
            # suite correctly but left `gated` below claiming the control "did not pass"
            # when it had passed — a false statement in my own output.
            R.control("POS-migration-boots-past-unparseable-id", booted_bad,
                    "the engine refuses to BOOT because one row's id is not a uuid. One "
                    "bad row takes the whole board down, and the board is the thing that "
                    "tells you which row is bad. A partial restore, a hand-edited row or "
                    "an older schema all produce this. Boot log tail:\n%s" % log2[-800:])
            st2, board2 = http("GET", "/v1/board")
            names = {n.get("name") for n in ((board2 or {}).get("nodes") or [])}
            # PENDING for the same reason as the clamp above: BUG-031 is filed and open,
            # and this gate has never reached main. Landing it red would repeat exactly
            # the violation that froze four lanes an hour ago, with my own name on both.
            # PROMOTED. BUG-031 is fixed (0417a5e: board::list is per-row and logs the
            # id as stored). Asserting all three halves, not just the 200: the board
            # SERVES, the good nodes are STILL THERE, and the skipped row is NAMED —
            # because skipping quietly would trade "board is a 500" for "a node vanished
            # and nothing says why", which is the same quiet-failure trade this whole
            # class is about. SDK's log line is what makes the third assertable.
            R.gated("POS-migration-bad-id-does-not-hide-good-nodes",
                    "POS-migration-boots-past-unparseable-id",
                    st2 == 200
                    and any(n and n.startswith("mig-node-") for n in names)
                    and "mig-0000-not-a-uuid" in log2,
                    "expected /v1/board -> 200 listing the well-formed nodes AND the "
                    "skipped row named in the log. Got status %s, nodes %s, and the "
                    "stored id %s in the log. One node missing and NAMED beats every node "
                    "missing and unexplained; a silent skip trades a 500 for a vanished "
                    "node, which is the same bad trade one step quieter."
                    % (st2, sorted(names)[:8],
                       "present" if "mig-0000-not-a-uuid" in log2 else "ABSENT"))

        return R.report("position-migration")
    finally:
        stop_engine()
        sh("docker", "volume", "rm", "-f", VOLUME)


if __name__ == "__main__":
    sys.exit(main())
