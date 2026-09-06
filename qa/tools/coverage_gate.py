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

EXEMPT = {
    "wheel-engine": ("scaffolding; not yet bootable",
                     "expires when the engine is bootable / wheel-engine:test exists"),
}

def main():
    if subprocess.run(["cargo", "llvm-cov", "--version"], capture_output=True).returncode != 0:
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

    fails, exempted = [], []
    print("per-crate line coverage (bar: %.0f%%)" % MIN)
    for crate in sorted(per):
        covered, total = per[crate]
        pct = (100.0 * covered / total) if total else 100.0
        ex = EXEMPT.get(crate)
        if ex and pct < MIN:
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
    if fails:
        print("\ncoverage gate: %d FAILED" % len(fails))
        for f in fails:
            print("  -", f)
        return 1
    print("\ncoverage gate: every non-exempt crate meets the %.0f%% bar" % MIN)
    return 0

if __name__ == "__main__":
    sys.exit(main())
