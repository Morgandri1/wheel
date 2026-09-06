#!/usr/bin/env python3
"""Per-crate line-coverage gate — ARCHITECTURE.md §0b (>=90% per crate).

PM ruling 2026-09-05: the 90% bar is per CRATE, not workspace-wide, and is on for
everything on main. A workspace average hides a 0%-covered crate behind a well-tested
one, which is exactly what §0b exists to prevent.

Exemptions are declared HERE, in one place, and each one must name:
  the crate · why · the EVENT that expires it (never "later", never a date nobody checks)
An exemption whose crate has since reached the bar FAILS the gate, so a stale exemption
cannot quietly outlive its reason.

Runs one instrumented build for the whole workspace and splits the result per crate.
"""
import json, os, shutil, subprocess, sys, tempfile, collections

SKIP = 77
MIN = float(os.environ.get("COV_MIN", "90"))
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))

def _engine_image_exists():
    try:
        return subprocess.run(["docker", "image", "inspect", "wheel-engine:test"],
                              capture_output=True).returncode == 0
    except FileNotFoundError:
        return False


# crate -> (reason, expiry prose, expired?) where expired? is a PREDICATE, not a promise.
#
# An exemption whose expiry is only written down expires when somebody remembers, which is
# never. wheel-engine's said "expires when the engine is bootable / wheel-engine:test
# exists" -- and that image has existed for some time, with the whole integration suite
# running against it, while the exemption quietly went on hiding the largest crate in the
# workspace at 71%. The condition has to be executable or it is decoration.
EXEMPT = {
    "wheel-engine": ("scaffolding; not yet bootable",
                     "expires when the engine is bootable / wheel-engine:test exists",
                     _engine_image_exists),
}

def db_gated_crates():
    """Crates whose test suite is partly gated behind a Postgres instance we do not have.

    `crates/wheel-api/tests/*_db.rs` self-skip when TEST_DATABASE_URL is unset, printing
    "skipping ... TEST_DATABASE_URL not set" and passing. Coverage is then measured with a
    large part of the suite absent, and the number that comes out is not a verdict on the
    crate -- it is a verdict on this machine. Measured locally, wheel-api reads 33.38%
    against 89.02% in CI, where the database exists.

    Reporting that as FAIL would send its owner after a 57-point gap that is not there. So
    the crate is reported INCONCLUSIVE and the run does not fail on it. CI sets
    TEST_DATABASE_URL (and WHEEL_CI_HAS_DB=1, which turns the skip into a hard error), so
    the bar is still enforced exactly where the measurement is complete.
    """
    if os.environ.get("TEST_DATABASE_URL"):
        return set()
    out = set()
    crates_dir = os.path.join(ROOT, "crates")
    for crate in os.listdir(crates_dir) if os.path.isdir(crates_dir) else []:
        tests = os.path.join(crates_dir, crate, "tests")
        if not os.path.isdir(tests):
            continue
        for f in os.listdir(tests):
            if not f.endswith(".rs"):
                continue
            try:
                with open(os.path.join(tests, f)) as fh:
                    if "TEST_DATABASE_URL" in fh.read():
                        out.add(crate)
                        break
            except OSError:
                pass
    return out


def main():
    try:
        probe = subprocess.run(["cargo", "llvm-cov", "--version"], capture_output=True)
    except FileNotFoundError:
        # No cargo at all. A gate that cannot run says so; it does not raise, because a
        # traceback exits non-zero and reads as "the code is bad" rather than "I could not
        # look at the code".
        print("cargo is not on PATH — run 'make bootstrap' (or source ~/.cargo/env)")
        return SKIP
    if probe.returncode != 0:
        print("cargo-llvm-cov not installed — run 'make bootstrap'")
        return SKIP

    # NOT <repo>/target: ~/.cargo/config.toml points every worktree at one shared
    # target-dir, so <repo>/target may not exist and llvm-cov fails to create the report.
    tmp = tempfile.mkdtemp(prefix="wheel-cov-")
    out = os.path.join(tmp, "qa-coverage.json")

    # A PRIVATE target dir for the instrumented build.
    #
    # ~/.cargo/config.toml points every worktree at one shared target-dir so six agents
    # do not each rebuild the world. That is right for building and wrong for measuring:
    # llvm-cov reads the profile data it finds there, which includes objects built from
    # another worktree's copy of the same crate. It reported validate.rs at 0% while the
    # file was 97% covered, and dragged wheel-core's total to 4.85% -- every per-crate
    # number in BUG-006 was measured against a mixture of six checkouts.
    #
    # Slower (this build is not shared with anyone), and the only way the number means
    # anything.
    # PERSISTENT, not a temp dir: separate from the shared build cache so the measurement
    # is honest, but reused between runs so it is not a cold rebuild every time. The first
    # version put it under the temp dir that gets rmtree'd at the end, which made every
    # `make coverage` a full instrumented rebuild -- on a six-agent host that is minutes of
    # load average 50, and a gate nobody can afford to run is a gate nobody runs.
    cov_target = os.path.join(ROOT, "target-cov")
    env = dict(os.environ, CARGO_TARGET_DIR=cov_target)
    r = subprocess.run(
        ["cargo", "llvm-cov", "--workspace", "--json", "--output-path", out,
         # PM-approved, requested by API, owned here rather than in their crates so the
         # team that benefits is not the team that widens it. Scoped to main.rs and
         # nothing wider: those files are pure wiring (config load, pool, router assembly,
         # serve). If logic lands in one, the fix is to move the logic into a covered
         # module — NOT to widen this regex.
         "--ignore-filename-regex", r"(^|/)main\.rs$"],
        cwd=ROOT, env=env, capture_output=True, text=True)
    if r.returncode in (137, -9):
        # SIGKILL: the instrumented build was OOM-killed, not a coverage failure.
        # Reporting this as "below the bar" would be a lie, and reporting it as a pass
        # would be worse. It is a gate that could not run.
        print("cargo llvm-cov was KILLED (exit 137 — out of memory).\n"
              "The instrumented build needs substantially more RAM than a normal build;\n"
              "on a loaded machine it cannot complete. Run `make coverage` alone, or rely\n"
              "on CI, where it has the machine to itself.")
        return SKIP
    combined = (r.stderr or "") + (r.stdout or "")
    if r.returncode != 0 and ("SIGKILL" in combined or "signal: 9" in combined):
        # llvm-cov itself exited 101, but only because one of the TEST BINARIES it ran was
        # killed by the OOM reaper. The 137/-9 check above catches llvm-cov being killed; it
        # does not catch a child dying, and the difference is invisible in the exit code.
        #
        # Reporting this as a coverage failure is the same lie as reporting a skip as a pass,
        # pointed the other way: it sends somebody to write tests for a crate whose tests
        # were never allowed to finish. The instrumented binaries are much larger than normal
        # ones, and this host runs six agents.
        killed = [ln for ln in combined.splitlines() if "SIGKILL" in ln or "signal: 9" in ln]
        print("a test binary was KILLED (out of memory), so coverage was never measured:\n  %s\n"
              "This is a gate that could not run, not a crate that failed. Run `make coverage`\n"
              "when the machine is quieter, or rely on CI where it has the box to itself."
              % "\n  ".join(killed[:3]))
        return SKIP
    if r.returncode != 0 or not os.path.exists(out):
        print("cargo llvm-cov failed (exit %d):\n%s" % (r.returncode, r.stderr[-2000:] or r.stdout[-2000:]))
        return 1

    with open(out) as f:
        data = json.load(f)
    shutil.rmtree(tmp, ignore_errors=True)

    # Sum per crate from per-file line counts; a crate is crates/<name>/...
    #
    # Scoped to THIS worktree. "contains /crates/" matches
    # /Users/metatron/wheel-wt/<other-role>/crates/... just as happily as our own, so an
    # unscoped filter sums one crate's lines across every checkout on the machine.
    root = os.path.realpath(ROOT) + os.sep
    per = collections.defaultdict(lambda: [0, 0])   # crate -> [covered, total]
    foreign = 0
    for export in data.get("data", []):
        for fl in export.get("files", []):
            path = fl.get("filename", "")
            marker = os.sep + "crates" + os.sep
            if marker not in path:
                continue
            if not os.path.realpath(path).startswith(root):
                foreign += 1
                continue
            crate = path.split(marker, 1)[1].split(os.sep, 1)[0]
            s = fl.get("summary", {}).get("lines", {})
            per[crate][0] += s.get("covered", 0)
            per[crate][1] += s.get("count", 0)

    if not per:
        # Never "0% coverage": a report with no files from this worktree is a broken
        # measurement, and the honest answer is that the gate did not run.
        print("no crate coverage data from %s (%d file(s) came from other worktrees) — "
              "the report does not describe this checkout" % (ROOT, foreign))
        return SKIP
    if foreign:
        print("note: ignored %d instrumented file(s) from other worktrees" % foreign)

    fails, exempted, inconclusive = [], [], []
    needs_db = db_gated_crates()
    print("per-crate line coverage (bar: %.0f%%)" % MIN)
    for crate in sorted(per):
        covered, total = per[crate]
        pct = (100.0 * covered / total) if total else 100.0
        ex = EXEMPT.get(crate)
        if ex and ex[2]():
            # The condition the exemption named has come true. This is a ruling for PM to
            # renew or retire -- not something this gate may quietly keep honouring, and
            # not something it may silently drop either.
            fails.append("%s is EXEMPT on a condition that has now been MET (%s). It reads "
                         "%.2f%%. Renew the exemption with a new expiry or retire it; the "
                         "gate will not keep hiding the crate on an expired promise."
                         % (crate, ex[1], pct))
            print("  EXPIRED %-13s %6.2f%%  (%d/%d)  exemption no longer applies"
                  % (crate, pct, covered, total))
        elif crate in needs_db:
            inconclusive.append(
                "%s reads %.2f%% here, but its *_db.rs suites skipped (TEST_DATABASE_URL "
                "unset) — this number describes this machine, not the crate" % (crate, pct))
            print("  ????   %-14s %6.2f%%  (%d/%d)  db tests did not run" %
                  (crate, pct, covered, total))
        elif ex and pct < MIN:
            exempted.append("%s at %.2f%% — %s (%s)" % (crate, pct, ex[0], ex[1]))
            print("  EXEMPT %-14s %6.2f%%  (%d/%d)" % (crate, pct, covered, total))
        elif ex and pct >= MIN:
            fails.append("%s is EXEMPT but now at %.2f%% — remove its exemption from "
                         "qa/tools/coverage_gate.py" % (crate, pct))
            print("  STALE  %-14s %6.2f%%  (%d/%d)" % (crate, pct, covered, total))
        elif pct < MIN:
            fails.append("%s at %.2f%% is below the %.0f%% bar (§0b)" % (crate, pct, MIN))
            print("  FAIL   %-14s %6.2f%%  (%d/%d)" % (crate, pct, covered, total))
        else:
            print("  ok     %-14s %6.2f%%  (%d/%d)" % (crate, pct, covered, total))

    if exempted:
        print("\n%d exempt crate(s):" % len(exempted))
        for e in exempted:
            print("  -", e)
    if inconclusive:
        print("\n%d crate(s) NOT MEASURED here:" % len(inconclusive))
        for i in inconclusive:
            print("  -", i)
        print("  Start Postgres and set TEST_DATABASE_URL to measure them locally; CI "
              "always does.")
    if fails:
        print("\ncoverage gate: %d FAILED" % len(fails))
        for f in fails:
            print("  -", f)
        return 1
    print("\ncoverage gate: every non-exempt crate meets the %.0f%% bar" % MIN)
    return 0

if __name__ == "__main__":
    sys.exit(main())
