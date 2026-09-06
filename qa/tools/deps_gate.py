#!/usr/bin/env python3
"""DEP-* — dependency weight is a number someone has to argue for, not a drift.

PM's A10 (efficiency is P1): "a gate on total crate count ... and cargo tree -d returning
nothing; a new duplicate should be a red build, not a slow one."

WHAT IS MEASURED, AND WHY IT IS NOT `cargo tree -d`:
`cargo tree -d` and a raw `cargo metadata` count both include crates that never compile on
any machine we own. The unfiltered resolve is 346 packages with 25 duplicated names; of
those, 62 packages and 11 duplicates are windows-only. Gating on 346 would mean gating on a
number that nobody can act on and that moves when an unrelated crate adds a windows target
-- and a gate people cannot act on is a gate people learn to ignore. So every number here
is per-platform (`--filter-platform`), for the two platforms that exist: this laptop and
what Railway ships.

Budgets are CEILINGS THAT RATCHET DOWN. Below the ceiling, the ceiling drops to what was
measured and the gate asks you to commit it; above it, the build is red. Efficiency work
therefore locks itself in, and a regression has to be argued for in a diff rather than
noticed six months later on a bill.

  python3 qa/tools/deps_gate.py            # check
  python3 qa/tools/deps_gate.py --update   # ratchet the budget file down to what is measured
"""
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BUDGET = os.path.join(ROOT, "qa", "deps-budget.json")
PLATFORMS = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
SKIP = 77


def measure(platform):
    p = subprocess.run(["cargo", "metadata", "--format-version", "1",
                        "--filter-platform", platform],
                       capture_output=True, text=True, cwd=ROOT)
    if p.returncode != 0:
        return None, (p.stderr or "").strip()[:200]
    m = json.loads(p.stdout)
    byid = {pkg["id"]: pkg for pkg in m["packages"]}
    graph = {n["id"]: [d["pkg"] for d in n.get("deps", [])] for n in m["resolve"]["nodes"]}

    def closure(root):
        seen, stack = set(), [root]
        while stack:
            i = stack.pop()
            if i in seen:
                continue
            seen.add(i)
            stack.extend(graph.get(i, []))
        return seen

    versions = {}
    for pkg in m["packages"]:
        versions.setdefault(pkg["name"], set()).add(pkg["version"])
    members = [pkg for pkg in m["packages"] if pkg.get("source") is None]
    return {
        "total": len(m["packages"]),
        "duplicates": sorted(k for k, v in versions.items() if len(v) > 1),
        "crates": {pkg["name"]: len(closure(pkg["id"])) - 1 for pkg in members},
        "pulls": {pkg["name"]: sorted({byid[i]["name"] for i in closure(pkg["id"])})
                  for pkg in members},
    }, None


def load():
    if not os.path.exists(BUDGET):
        return {"platforms": {}, "forbidden": {}, "known_debt": {}}
    with open(BUDGET) as fh:
        return json.load(fh)


def main():
    if subprocess.run(["which", "cargo"], capture_output=True).returncode != 0:
        print("cargo not installed — run `make bootstrap`")
        return SKIP

    budget = load()
    update = "--update" in sys.argv
    failures, notes, measured = [], [], {}

    for platform in PLATFORMS:
        got, err = measure(platform)
        if got is None:
            print("could not read the dependency graph for %s: %s" % (platform, err))
            return SKIP
        measured[platform] = got
        want = budget.get("platforms", {}).get(platform, {})

        ceiling = want.get("total")
        if ceiling is None:
            notes.append("%s: seeding total at %d" % (platform, got["total"]))
        elif got["total"] > ceiling:
            failures.append("DEP-crate-budget: %s resolves %d crates, ceiling is %d. "
                            "Adding %d crates to every build needs an argument in the diff."
                            % (platform, got["total"], ceiling, got["total"] - ceiling))
        elif got["total"] < ceiling:
            notes.append("%s: total improved %d -> %d; ratchet with --update"
                         % (platform, ceiling, got["total"]))

        for name, count in sorted(got["crates"].items()):
            per = want.get("crates", {}).get(name)
            if per is None:
                notes.append("%s: seeding %s at %d" % (platform, name, count))
            elif count > per:
                failures.append("DEP-crate-budget: %s/%s pulls %d crates, ceiling is %d"
                                % (platform, name, count, per))
            elif count < per:
                notes.append("%s: %s improved %d -> %d; ratchet with --update"
                             % (platform, name, per, count))

        allowed = set(want.get("duplicates", []))
        new = [d for d in got["duplicates"] if d not in allowed]
        gone = [d for d in allowed if d not in got["duplicates"]]
        if new:
            failures.append("DEP-no-new-duplicates: %s compiles these crates TWICE and they "
                            "are not in the budget: %s. Two versions of one crate is two "
                            "compiles and two copies in the binary."
                            % (platform, ", ".join(new)))
        if gone:
            # Expires by breaking: a duplicate that has been removed must leave the file,
            # or the allowlist silently re-permits it the next time it comes back.
            failures.append("DEP-no-new-duplicates: %s no longer duplicates %s — good, now "
                            "remove it from qa/deps-budget.json (or --update) so it cannot "
                            "come back unnoticed." % (platform, ", ".join(gone)))

    # Dependencies that must not appear in a given binary at all. PM: "a laptop or wheeld
    # build never compiles sqlx".
    linux = measured[PLATFORMS[-1]]
    for member, banned in sorted(budget.get("forbidden", {}).items()):
        pulls = set(linux["pulls"].get(member, []))
        for crate in banned:
            debt = budget.get("known_debt", {}).get("%s:%s" % (member, crate))
            if crate in pulls and not debt:
                failures.append("DEP-forbidden: %s pulls %s" % (member, crate))
            elif crate in pulls:
                notes.append("KNOWN DEBT %s pulls %s — %s" % (member, crate, debt))
            elif debt:
                failures.append("DEP-forbidden: %s no longer pulls %s. Delete its "
                                "known_debt entry so the ban is enforced from now on."
                                % (member, crate))

    if update:
        budget.setdefault("platforms", {})
        for platform, got in measured.items():
            budget["platforms"][platform] = {"total": got["total"],
                                             "duplicates": got["duplicates"],
                                             "crates": got["crates"]}
        with open(BUDGET, "w") as fh:
            json.dump(budget, fh, indent=2, sort_keys=True)
            fh.write("\n")
        print("budget written to %s" % os.path.relpath(BUDGET, ROOT))
        return 0

    for n in notes:
        print("  note: %s" % n)
    if failures:
        print("\ndependency budget: FAILED")
        for f in failures:
            print("  - %s" % f)
        return 1
    print("dependency budget: %d crates on linux, %d duplicates, every member within its "
          "ceiling" % (linux["total"], len(linux["duplicates"])))
    return 0


if __name__ == "__main__":
    sys.exit(main())
