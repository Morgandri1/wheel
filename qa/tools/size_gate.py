#!/usr/bin/env python3
"""DEP-binary-size — what Railway actually ships is a number with a ceiling.

PM's A10. Release binaries are what run in production and what get pulled on every deploy,
so their size is a running cost. Like the crate budget this is a CEILING THAT RATCHETS
DOWN: an improvement lowers it and locks itself in, a regression is red.

Separated from deps_gate.py because it needs a real release build (fat LTO, one codegen
unit) and that is minutes and gigabytes. It is NOT in `make check` for the same reason
coverage is not: on a laptop with six agents resident it is the thing that gets OOM-killed.
Run it deliberately with `make size`, and in CI, which is building anyway.

THE BUDGET STARTS EMPTY ON PURPOSE. A ceiling I have not measured is a number I cannot
defend, so the first run seeds it and says so rather than inventing one.
"""
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BUDGET = os.path.join(ROOT, "qa", "size-budget.json")
# What is actually deployed: the engine and cli ship inside the sandbox image, the host and
# api are the Railway services. Test-only binaries are not a running cost.
SHIPPED = ["wheel-engine", "wheel-cli", "wheel-host", "wheel-api"]
SKIP = 77
# Percent a binary may grow before the gate objects. Release size moves a little with
# toolchain patches, and a gate that fires on 200 bytes gets ignored.
TOLERANCE = 0.02


def verdict(measured, budget):
    """(failures, notes, budget, changed) — the ratchet, as a pure function.

    Separated so it can be exercised without a fat-LTO release build. The measurement half
    needs minutes and gigabytes; the DECISION half is where a ratchet gets its direction
    wrong, and a gate whose logic has never run is not a gate. Same reason `staleness` is
    split out of the image-freshness check.
    """
    failures, notes, changed = [], [], False
    budget = dict(budget)
    for name, size in sorted(measured.items()):
        ceiling = budget.get(name)
        if ceiling is None:
            budget[name] = size
            changed = True
            notes.append("seeding %s at %.2f MiB" % (name, size / 1048576))
        elif size > ceiling * (1 + TOLERANCE):
            failures.append("DEP-binary-size: %s is %.2f MiB, ceiling %.2f MiB (+%.1f%%)"
                            % (name, size / 1048576, ceiling / 1048576,
                               100.0 * (size - ceiling) / ceiling))
        elif size < ceiling:
            budget[name] = size
            changed = True
            notes.append("%s improved %.2f -> %.2f MiB; ceiling lowered"
                         % (name, ceiling / 1048576, size / 1048576))
    return failures, notes, budget, changed


def main():
    if subprocess.run(["which", "cargo"], capture_output=True).returncode != 0:
        print("cargo not installed — run `make bootstrap`")
        return SKIP

    # A private target dir: the shared one is written by six worktrees at once, and a
    # measurement taken from a directory someone else is linking into is not a measurement.
    env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(ROOT, "target-size"))
    build = subprocess.run(["cargo", "build", "--release", "--workspace"],
                           cwd=ROOT, env=env, capture_output=True, text=True)
    if build.returncode != 0:
        print("release build failed, so there is nothing to measure:\n%s"
              % build.stderr[-800:])
        return SKIP

    outdir = os.path.join(env["CARGO_TARGET_DIR"], "release")
    measured = {}
    for name in SHIPPED:
        path = os.path.join(outdir, name)
        if os.path.exists(path):
            measured[name] = os.path.getsize(path)
    if not measured:
        print("no shipped binaries found in %s — nothing measured, and an empty "
              "measurement is not a pass" % outdir)
        return SKIP

    budget = {}
    if os.path.exists(BUDGET):
        with open(BUDGET) as fh:
            budget = json.load(fh)

    failures, notes, budget, changed = verdict(measured, budget)

    if changed and "--check-only" not in sys.argv:
        with open(BUDGET, "w") as fh:
            json.dump(budget, fh, indent=2, sort_keys=True)
            fh.write("\n")
        notes.append("wrote %s — commit it" % os.path.relpath(BUDGET, ROOT))

    for n in notes:
        print("  note: %s" % n)
    if failures:
        print("\nbinary size: FAILED")
        for f in failures:
            print("  - %s" % f)
        return 1
    print("binary size: " + ", ".join("%s %.2f MiB" % (n, s / 1048576)
                                      for n, s in sorted(measured.items())))
    return 0


if __name__ == "__main__":
    sys.exit(main())
