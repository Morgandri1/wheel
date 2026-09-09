#!/usr/bin/env python3

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""Answers one question about a live board: will #22's migration move any node visibly?

Rounding cannot. It moves a node at most 0.5 cells per axis, one cell renders as one CSS
pixel, and the board caps zoom at 1.8 (web/src/components/board/canvas.tsx), so the worst
a human can be shown is 1.27 px. That bound is geometry and holds for every board.

CLAMPING can, and is unbounded: a row already outside +/-32767 lands on the bound from
wherever it was, which is a node teleporting across the screen. Whether that happens is
not a question about the code -- it is a question about the DATA, and it can only be
answered by looking at the rows that exist.

  # against a sqlite file (on the host, or a volume copy)
  python3 qa/tools/position_audit.py /data/wheel.db

  # against a running engine's control plane
  WHEEL_ENGINE_SECRET=... python3 qa/tools/position_audit.py http://127.0.0.1:7000

Exit 0 = no row clamps, the migration is safe on rounding grounds alone.
Exit 1 = at least one node WILL visibly jump; they are named, with the distance in pixels.
"""
import json
import os
import sqlite3
import sys
import urllib.request

I16_MAX, I16_MIN = 32767, -32768
MAX_ZOOM = 1.8


def cell(v):
    """Round to nearest, symmetric about zero, then clamp — PM's ruling."""
    import math
    r = math.floor(v + 0.5) if v >= 0 else math.ceil(v - 0.5)
    return max(I16_MIN, min(I16_MAX, int(r)))


def from_sqlite(path):
    db = sqlite3.connect(path)
    return [(n, x, y) for n, x, y in db.execute("SELECT name, x, y FROM nodes")]


def from_engine(base):
    secret = os.environ.get("WHEEL_ENGINE_SECRET")
    if not secret:
        print("set WHEEL_ENGINE_SECRET to read a running engine", file=sys.stderr)
        raise SystemExit(2)
    req = urllib.request.Request(base.rstrip("/") + "/v1/board")
    req.add_header("Authorization", "Bearer " + secret)
    with urllib.request.urlopen(req, timeout=20) as r:
        board = json.loads(r.read().decode())
    out = []
    for n in board.get("nodes", []):
        p = n.get("position") or {}
        out.append((n.get("name"), p.get("x"), p.get("y")))
    return out


def main():
    if len(sys.argv) < 2:
        print(__doc__.strip())
        return 2
    src = sys.argv[1]
    rows = from_engine(src) if src.startswith("http") else from_sqlite(src)
    if not rows:
        print("no nodes found — an empty board is not an answer, check the source")
        return 2

    clamped, moved = [], 0.0
    for name, x, y in rows:
        if x is None or y is None:
            continue
        nx, ny = cell(float(x)), cell(float(y))
        d = max(abs(nx - float(x)), abs(ny - float(y)))
        moved = max(moved, d)
        if abs(float(x)) > I16_MAX or abs(float(y)) > I16_MAX:
            clamped.append((name, float(x), float(y), nx, ny, d))

    print("%d nodes; largest movement %.3f cells = %.2f px at max zoom %.1f"
          % (len(rows), moved, moved * MAX_ZOOM, MAX_ZOOM))
    if not clamped:
        print("\nNO node clamps. Every row is inside +/-32767, so the migration moves "
              "nothing an operator can see (worst case %.2f px, well under one pixel of "
              "perceptible movement)." % (moved * MAX_ZOOM))
        return 0

    print("\n%d NODE(S) WILL VISIBLY JUMP — these are outside the i16 bound and clamp:"
          % len(clamped))
    for name, x, y, nx, ny, d in clamped:
        print("  %-24s (%.1f, %.1f) -> (%d, %d)   moves %.0f cells = %.0f px"
              % (name, x, y, nx, ny, d, d * MAX_ZOOM))
    print("\nSDK should clamp AND LOG these in #22 rather than let them land silently; "
          "POS-migration-clamp-is-reported asserts the log names them.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
