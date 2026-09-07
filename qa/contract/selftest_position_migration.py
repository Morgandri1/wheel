#!/usr/bin/env python3
"""Proves POS-migration-* can actually FAIL, without docker and without #22.

A gate whose logic has never been watched going red is not a gate — BUG-024 was two
panic checks that passed against the engine that took the board down, because the inputs
could not reach the defect. The migration suite has the same exposure in a quieter form:
its row set is what makes it able to detect anything, and a later tidy-up that drops the
"awkward" rows would leave a suite that still runs, still passes, and proves nothing.

So this asserts two things every time `make check` runs:

  1. `expected_cell` agrees with PM's ruling (round to nearest whole cell, symmetric
     about zero, then clamp to the i16 bounds).
  2. NON-VACUITY: the row set still contains rows where CAST(x AS INTEGER) — the
     truncating migration the suite exists to catch — disagrees with the ruling. If it
     ever does not, the suite would pass against a truncating migration and this fails
     instead, naming what was lost.
"""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "qa", "integration"))

from test_position_migration import ROWS, I16_MAX, I16_MIN, expected_cell  # noqa: E402

# (input, expected) straight from the ruling in docs/ARCHITECTURE.md "Position is an
# integer cell". Written out rather than computed, so this is a second opinion and not
# the implementation checking itself.
RULING = [
    (10.0, 10), (10.4, 10), (10.6, 11),
    (-10.4, -10), (-10.6, -11),
    (0.5, 1), (-0.5, -1),
    (0.0, 0),
    (99999.0, I16_MAX), (-99999.0, I16_MIN),
    (float(I16_MAX), I16_MAX), (float(I16_MIN), I16_MIN),
]


def main():
    bad = [(v, expected_cell(v), want) for v, want in RULING if expected_cell(v) != want]
    for v, got, want in bad:
        print("  ✗ expected_cell(%r) = %r, the ruling says %r" % (v, got, want))

    # CAST truncates toward zero; int() is the same operation in python.
    detectors = [(x, y) for (x, y, _c, _w) in ROWS
                 if (int(x), int(y)) != (expected_cell(x), expected_cell(y))]
    if not detectors:
        print("  ✗ NON-VACUOUS ROWS ARE GONE. Every row in ROWS now rounds the same way "
              "CAST(x AS INTEGER) truncates, so POS-migration-rounds-not-truncates would "
              "pass against a truncating migration. Restore a row whose fraction is ≥ .5 "
              "(120.6 and -120.6 were the originals) before trusting that suite again.")

    if bad or not detectors:
        print("\nposition-migration selftest: FAILED")
        return 1
    print("position-migration selftest: ruling matched on %d values; %d of %d rows would "
          "catch a truncating migration" % (len(RULING), len(detectors), len(ROWS)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
