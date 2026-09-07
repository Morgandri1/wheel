#!/usr/bin/env python3
"""Does that green run describe main, or a commit main has left?

A CI run is green about a SHA. `main` is a branch, and it moves. Those two facts are
obvious separately and easy to conflate under time pressure — three times on 2026-09-07 a
green we were waiting on turned out to describe a commit main had already left, once within
minutes of someone saying so out loud.

It is the same shape as the two checks this repo already has:

    pin_image        a tag is a mutable pointer; resolve it to an immutable id
    image_freshness  the image under test may predate the code under test
    THIS             the run under discussion may predate the branch under discussion

None of the three is clever. Each exists because the cheap assumption is wrong often enough
to cost a night.

    python3 qa/tools/green_describes_head.py <run-id> [--branch main] [--remote origin]

Exit 0 only if the run CONCLUDED SUCCESS and its head is the branch's current head. Any
other combination exits non-zero and says which of the two it failed, because "not green"
and "green about something else" need different responses: one is a defect, the other is a
stale measurement.
"""
import json
import subprocess
import sys

SKIP = 77


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def main(argv):
    if not argv:
        print(__doc__.strip())
        return 2
    run_id = argv[0]
    branch = argv[argv.index("--branch") + 1] if "--branch" in argv else "main"
    remote = argv[argv.index("--remote") + 1] if "--remote" in argv else "origin"

    if sh("which", "gh").returncode != 0:
        print("gh is not installed, so the run cannot be read — this gate did not run")
        return SKIP

    p = sh("gh", "run", "view", run_id, "--json", "status,conclusion,headSha")
    if p.returncode != 0:
        print("could not read run %s: %s" % (run_id, (p.stderr or "").strip()[:200]))
        return SKIP
    run = json.loads(p.stdout)

    sh("git", "fetch", remote, "--quiet")
    q = sh("git", "rev-parse", "%s/%s" % (remote, branch))
    if q.returncode != 0:
        print("could not resolve %s/%s" % (remote, branch))
        return SKIP
    head = q.stdout.strip()
    run_sha = (run.get("headSha") or "").strip()

    if run["status"] != "completed":
        print("run %s is %s — not a verdict yet" % (run_id, run["status"]))
        return 1
    if run.get("conclusion") != "success":
        print("run %s concluded %s. That is a DEFECT to fix, not a stale measurement."
              % (run_id, run.get("conclusion")))
        return 1
    if run_sha != head:
        print("run %s is GREEN, but about %s — and %s/%s is now %s.\n"
              "  The green describes a commit the branch has left. It is evidence, not a\n"
              "  verdict on what is deployed or about to be. Re-run against the head."
              % (run_id, run_sha[:8], remote, branch, head[:8]))
        return 1
    print("run %s: green AND describes %s/%s at %s" % (run_id, remote, branch, head[:8]))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
