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
import json, os, subprocess, sys, collections

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

    out = os.path.join(ROOT, "target", "qa-coverage.json")
    r = subprocess.run(
        ["cargo", "llvm-cov", "--workspace", "--json", "--output-path", out,
         # PM-approved, requested by API, owned here rather than in their crates so the
         # team that benefits is not the team that widens it. Scoped to main.rs and
         # nothing wider: those files are pure wiring (config load, pool, router assembly,
         # serve). If logic lands in one, the fix is to move the logic into a covered
         # module — NOT to widen this regex.
         "--ignore-filename-regex", r"(^|/)main\.rs$"],
        cwd=ROOT, capture_output=True, text=True)
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

    # Sum per crate from per-file line counts; a crate is crates/<name>/...
    per = collections.defaultdict(lambda: [0, 0])   # crate -> [covered, total]
    for export in data.get("data", []):
        for fl in export.get("files", []):
            path = fl.get("filename", "")
            marker = os.sep + "crates" + os.sep
            if marker not in path:
                continue
            crate = path.split(marker, 1)[1].split(os.sep, 1)[0]
            s = fl.get("summary", {}).get("lines", {})
            per[crate][0] += s.get("covered", 0)
            per[crate][1] += s.get("count", 0)

    if not per:
        print("no crate coverage data found — is the workspace empty?")
        return SKIP

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
