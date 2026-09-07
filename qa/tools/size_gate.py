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
# What is shipped is DISCOVERED from cargo metadata, never hand-listed. The hand-written
# list this replaces named "wheel-cli" and "wheel-api" -- neither of which is a binary
# (wheel-cli builds `wheel`; wheel-api is a library) -- and omitted `wheeld`, the daemon
# Railway actually runs. It therefore measured two of the four things that ship and said
# nothing about the omission, because a name that produces no file just fell out of the
# loop. Discovery plus MISSING-IS-A-FAILURE is what stops that being silent.
#
# Only `export-schema` is excluded: it is a codegen tool run at build time, not a
# deliverable, so its size is nobody's running cost.
NOT_SHIPPED = {"export-schema"}
SKIP = 77

# MEASURE EACH BINARY IN THE FEATURE SET ITS DOCKERFILE BUILDS. Raised by API, who caught
# it against their own change and held the merge for it. `postgres` is becoming non-default
# on wheel-api, and this gate builds `--workspace` with DEFAULT features -- so it would have
# measured a wheel-api with no Postgres driver and no second TLS stack, gone green by a
# comfortable margin, and stopped measuring the binary Railway actually runs.
# docker/Dockerfile.api builds `-p wheel-api --features postgres`; this must agree with it.
#
# Same shape as the boot_db hole: a change removes something from the default build and
# takes a check with it, silently. wheel-api is currently the only member where the
# deployed feature set differs from the default.
# Shape: {member: {"features": [...], "no_default": bool}}. A bare list is shorthand for
# additive features with defaults left on. `no_default` exists because Railway's wheel-api
# is built --no-default-features --features postgres: production sets a postgres:// URL and
# never opens a sqlite store, and dropping the sqlite feature drops libsqlite3-sys -- an
# actual compiled C library, which is why API measured 2.14 MiB (28.4%) for a 5-crate delta.
#
# The declaration and docker/Dockerfile.api must land in the same window, and THE ORDER IS
# NOT ARBITRARY -- the two windows fail in opposite directions:
#
#   gate flipped FIRST (this commit):  gate measures no-default (5.39 MiB), Dockerfile still
#     ships default+postgres (7.53). The gate is measuring something STRICTER than what
#     ships. Wrong, but it cannot hide growth in the real artifact -- worst case it fails on
#     a binary smaller than the deployed one.
#   Dockerfile flipped first:  gate measures 7.53 while Railway ships 5.39. The gate is
#     LOOSE -- 2.14 MiB of headroom in which the shipped binary can grow unmeasured, and it
#     stays green throughout. That is the silent direction.
#
# So the gate goes first and the Dockerfile follows. Same reasoning as landing the coverage
# feature flag before API's default change rather than after: when two halves must agree,
# sequence them so the intermediate state is over-strict rather than over-permissive.
DEPLOY_FEATURES = {"wheel-api": {"features": ["postgres"], "no_default": True}}


def deploy_build(member, spec):
    """cargo args for the artifact this member's Dockerfile actually produces."""
    if isinstance(spec, list):
        spec = {"features": spec, "no_default": False}
    cmd = ["cargo", "build", "--release", "-p", member]
    if spec.get("no_default"):
        cmd.append("--no-default-features")
    if spec.get("features"):
        cmd += ["--features", ",".join(spec["features"])]
    return cmd
# Percent a binary may grow before the gate objects. Release size moves a little with
# toolchain patches, and a gate that fires on 200 bytes gets ignored.
TOLERANCE = 0.02


def host_triple():
    """The platform these bytes are for.

    A binary's size is a property of its TARGET, not of the project. API measured
    wheel-api at 7,897,376 bytes on macOS; the same commit is 8.78 MiB on Linux, and CI
    compared the second against the first and reported a 16.6% regression that never
    happened. Nothing had grown -- the ceiling and the measurement were describing
    different binaries. deps-budget.json was already keyed by platform for exactly this
    reason; this file simply had not caught up.
    """
    try:
        p = subprocess.run(["rustc", "-vV"], capture_output=True, text=True)
    except OSError:
        # No rustc on PATH is "cannot measure", not a crash. An unhandled exception here
        # reads as a broken gate rather than an absent toolchain.
        return None
    if p.returncode != 0:
        return None
    for line in p.stdout.splitlines():
        if line.startswith("host:"):
            return line.split(":", 1)[1].strip()
    return None


def target_dir(env):
    """Where cargo will actually put the binaries: env wins, then the shared config."""
    if env.get("CARGO_TARGET_DIR"):
        return env["CARGO_TARGET_DIR"]
    p = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"],
                       capture_output=True, text=True, cwd=ROOT, env=env)
    if p.returncode == 0:
        return json.loads(p.stdout)["target_directory"]
    return os.path.join(ROOT, "target")


def shipped_binaries(root=ROOT):
    """Bin target names for every workspace member, minus the build-time-only ones."""
    p = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"],
                       capture_output=True, text=True, cwd=root)
    if p.returncode != 0:
        return None
    names = set()
    for pkg in json.loads(p.stdout)["packages"]:
        for tgt in pkg.get("targets", []):
            if "bin" in tgt.get("kind", []) and tgt["name"] not in NOT_SHIPPED:
                names.add(tgt["name"])
    return sorted(names)


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

    # The shared target dir, held under the cargo lock for the build AND the measurement.
    # A private dir would guarantee nobody else is linking, but it also means a second
    # multi-gigabyte copy of every dependency on a laptop that PM has just cleaned 79 GB
    # off. The lock buys the same guarantee for the cost of waiting our turn -- and this
    # gate exists because disk and compute are the P1, so it should not be the thing that
    # eats them. WHEEL_SIZE_TARGET_DIR overrides it in CI, where the disk is disposable.
    env = dict(os.environ)
    if os.environ.get("WHEEL_SIZE_TARGET_DIR"):
        env["CARGO_TARGET_DIR"] = os.environ["WHEEL_SIZE_TARGET_DIR"]
    lock = [sys.executable, os.path.join(ROOT, "qa", "tools", "with_lock.py"),
            "/tmp/wheel-cargo.lock"]
    builds = [["cargo", "build", "--release", "--workspace"]]
    for member, spec in sorted(DEPLOY_FEATURES.items()):
        # Built AFTER the workspace so it overwrites that member's binary: what remains in
        # target/release is the artifact its Dockerfile produces, which is the only one
        # whose size is a running cost.
        builds.append(deploy_build(member, spec))
    for cmd in builds:
        build = subprocess.run(lock + cmd, cwd=ROOT, env=env,
                               capture_output=True, text=True)
        if build.returncode == 75:
            print("another worktree held the cargo lock longer than we waited — not measured")
            return 75
        if build.returncode != 0:
            print("release build failed (%s), so there is nothing to measure:\n%s"
                  % (" ".join(cmd[-3:]), build.stderr[-800:]))
            return SKIP

    expected = shipped_binaries()
    if expected is None:
        print("could not read cargo metadata, so the list of shipped binaries is unknown")
        return SKIP
    if not expected:
        print("cargo metadata reports no bin targets — that is not a workspace we ship, "
              "and an empty measurement is not a pass")
        return SKIP

    outdir = os.path.join(target_dir(env), "release")
    measured, missing = {}, []
    for name in expected:
        path = os.path.join(outdir, name)
        if os.path.exists(path):
            measured[name] = os.path.getsize(path)
        else:
            missing.append(name)
    if missing:
        # The build returned 0, so metadata promised a binary the build did not produce.
        # Skipping it would measure a subset and call it the total.
        print("binary size: FAILED")
        print("  - DEP-binary-size: cargo metadata declares %s but the release build "
              "produced no such file in %s. Measuring what is left would report a subset "
              "as the total." % (", ".join(missing), outdir))
        return 1

    plat = host_triple()
    if plat is None:
        print("could not determine the host target triple, so there is no ceiling to "
              "compare against — and comparing against someone else's platform is the "
              "bug this replaced")
        return SKIP

    doc = {}
    if os.path.exists(BUDGET):
        with open(BUDGET) as fh:
            doc = json.load(fh)
    # Migrate the old flat shape rather than silently reading it as this platform's.
    if doc and not doc.get("platforms"):
        doc = {"platforms": {}}
    budget = doc.get("platforms", {}).get(plat, {})

    failures, notes, budget, changed = verdict(measured, budget)
    doc.setdefault("platforms", {})[plat] = budget

    if changed and "--check-only" not in sys.argv:
        with open(BUDGET, "w") as fh:
            json.dump(doc, fh, indent=2, sort_keys=True)
            fh.write("\n")
        notes.append("wrote %s [%s] — commit it" % (os.path.relpath(BUDGET, ROOT), plat))

    for n in notes:
        print("  note: %s" % n)
    if failures:
        print("\nbinary size: FAILED")
        for f in failures:
            print("  - %s" % f)
        return 1
    print("binary size [%s]: " % plat + ", ".join("%s %.2f MiB" % (n, s / 1048576)
                                      for n, s in sorted(measured.items())))
    return 0


if __name__ == "__main__":
    sys.exit(main())
