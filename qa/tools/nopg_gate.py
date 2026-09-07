#!/usr/bin/env python3
"""rust:test-nopg — the DEFAULT build must not run a test that needs a Postgres driver.

`rust:test-pg` proves the Postgres arm builds and passes. This is the other half, and it is
the half that was missing when `postgres` left wheel-api's defaults: six suites kept
compiling, saw TEST_DATABASE_URL set in CI, tried to connect, and panicked with "this build
has no Postgres driver". Twelve failures, main red. A seventh was found the same way an hour
later.

THE GUARD IN THOSE FILES IS ON THE WRONG AXIS. They self-skip when TEST_DATABASE_URL is
UNSET; the failure is URL SET and driver ABSENT — a combination that could not exist while
postgres was a default, so the guard was correct right up until it wasn't.

WHY THIS DISCOVERS ITS TARGETS instead of listing them: the property is "no test in the
default build requires a driver the default build lacks". That is ONE assertion, and it was
being satisfied by hand, per file, seven times — so the seventh was always going to be found
the way the sixth was. Discovery means a `*_db.rs` added tomorrow is covered without anyone
remembering this file exists.

Running only the `*_db` targets rather than all of wheel-api is also what makes it cheap:
with the guards in place they compile to nothing, so the gate costs seconds instead of the
160s a full `cargo test -p wheel-api` takes. A gate nobody can afford to run is a gate
nobody runs.

The URL points at a closed port on purpose. Nothing should reach it; anything that tries is
the finding.
"""
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SKIP = 77
CONTENDED = 75
UNREACHABLE = "postgres://qa:qa@127.0.0.1:1/nonexistent"


def db_test_targets(pkg="wheel-api"):
    p = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"],
                       capture_output=True, text=True, cwd=ROOT)
    if p.returncode != 0:
        return None
    out = []
    for package in json.loads(p.stdout)["packages"]:
        if package["name"] != pkg:
            continue
        for t in package.get("targets", []):
            if "test" in t.get("kind", []) and t["name"].endswith("_db"):
                out.append(t["name"])
    return sorted(out)


def main():
    if subprocess.run(["which", "cargo"], capture_output=True).returncode != 0:
        print("cargo not installed — run `make bootstrap`")
        return SKIP
    targets = db_test_targets()
    if targets is None:
        print("could not read cargo metadata, so the suites to check are unknown")
        return SKIP
    if not targets:
        # Not a pass. Either the naming convention changed or the package moved, and either
        # way this gate is now asserting nothing about anything.
        print("no *_db test targets found in wheel-api. This gate discovers its targets by "
              "that suffix; if the convention changed, it is now checking NOTHING and needs "
              "updating rather than quietly passing.")
        return 1

    cmd = [sys.executable, os.path.join(ROOT, "qa", "tools", "with_lock.py"),
           os.environ.get("WHEEL_CARGO_LOCK", "/tmp/wheel-cargo.lock"),
           "cargo", "test", "-p", "wheel-api"]
    for t in targets:
        cmd += ["--test", t]
    env = dict(os.environ, TEST_DATABASE_URL=UNREACHABLE)
    env.pop("WHEEL_CI_HAS_DB", None)   # that flag turns the skip into a hard error
    r = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True, text=True)
    if r.returncode == CONTENDED:
        print("another worktree held the cargo lock longer than we waited")
        return CONTENDED
    if r.returncode == 0:
        print("default build needs no Postgres driver: %d suite(s) checked (%s)"
              % (len(targets), ", ".join(targets)))
        return 0

    tail = (r.stdout or "")[-1500:] + (r.stderr or "")[-800:]
    print("rust:test-nopg: FAILED\n"
          "  A test in the DEFAULT build tried to reach a database. Under default features "
          "wheel-api has no Postgres driver, so a suite that connects when TEST_DATABASE_URL "
          "is merely SET will panic here and in CI.\n"
          "  The fix is `#![cfg(feature = \"postgres\")]` on the suite, matching boot_db.rs "
          "-- guarding on the feature that provides the driver, not on the URL.\n"
          "  Suites checked: %s\n%s" % (", ".join(targets), tail))
    return 1


if __name__ == "__main__":
    sys.exit(main())
