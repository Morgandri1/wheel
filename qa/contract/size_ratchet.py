#!/usr/bin/env python3
"""The size ratchet decides correctly — checked without a release build.

DEP-binary-size needs a fat-LTO build of the workspace to MEASURE, which is why it runs
only in CI. But the measurement is the easy half. The half that goes wrong is the
DECISION: a ratchet with its direction inverted quietly raises the ceiling to whatever was
last built, and then it is not a budget at all, it is a record of the largest binary we
have ever shipped. That failure is silent and permanent, and nothing would catch it,
because the gate would be green every single time.

So the decision is a pure function and this exercises it in both directions, including the
one that only matters when it is broken: the ceiling must never rise on its own.
"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                                "tools"))
from size_gate import TOLERANCE, verdict  # noqa: E402

MB = 1048576


def main():
    cases = []

    failures, _, budget, changed = verdict({"wheel-cli": 5 * MB}, {})
    cases.append(("an unseeded binary seeds its ceiling rather than failing",
                  not failures and budget["wheel-cli"] == 5 * MB and changed))

    failures, _, budget, changed = verdict({"wheel-cli": 6 * MB}, {"wheel-cli": 5 * MB})
    cases.append(("a regression fails AND leaves the ceiling where it was",
                  len(failures) == 1 and budget["wheel-cli"] == 5 * MB and not changed))

    noise = int(5 * MB * (1 + TOLERANCE / 2))
    failures, _, budget, _ = verdict({"wheel-cli": noise}, {"wheel-cli": 5 * MB})
    cases.append(("toolchain noise inside the tolerance neither fails nor raises it",
                  not failures and budget["wheel-cli"] == 5 * MB))

    failures, _, budget, changed = verdict({"wheel-cli": 4 * MB}, {"wheel-cli": 5 * MB})
    cases.append(("an improvement ratchets the ceiling DOWN and banks it",
                  not failures and budget["wheel-cli"] == 4 * MB and changed))

    _, _, budget, _ = verdict({"wheel-cli": int(5 * MB * 1.001)}, {"wheel-cli": 5 * MB})
    cases.append(("the ceiling never rises on its own — the one that is silent when broken",
                  budget["wheel-cli"] == 5 * MB))

    bad = [why for why, ok in cases if not ok]
    for why, ok in cases:
        print("  %s %s" % ("ok  " if ok else "FAIL", why))
    if bad:
        print("\nsize ratchet: FAILED")
        return 1
    print("size ratchet: %d properties hold" % len(cases))
    return 0


if __name__ == "__main__":
    sys.exit(main())
