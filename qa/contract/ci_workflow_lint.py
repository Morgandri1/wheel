#!/usr/bin/env python3
"""Lint .github/workflows/ci.yml — it cannot be tested by running it.

A broken workflow file does not fail CI; it means CI never runs, which looks like
"no red" rather than "no verdict". These checks are the ones whose absence has already
cost us a verdict on main.
"""
import os, sys

SKIP = 77
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
CI = os.path.join(ROOT, ".github", "workflows", "ci.yml")

# Jobs allowed to be switched off, each with the reason and the EVENT that re-enables it —
# the same discipline as the coverage exemptions, and for the same reason. `integration` sat
# here as `if: false` from before the engine image existed; the image landed, the condition
# did not, and 127 assertions ran only on one laptop while CI reported green.
DISABLED_OK = {}

def check_relocated_gates(doc):
    """Gates that `make check` cannot run must still run SOMEWHERE, provably.

    qa:image-contents needs an engine image; `make check` builds none, so it is marked not
    applicable there and runs in the CI job that does build one. That is a reasonable trade
    and a dangerous one: "moved to another job" and "silently stopped running" look exactly
    alike from a green summary. This asserts the destination still exists -- the same job
    must both build the image and invoke the gate.
    """
    fails = []
    for name, job in (doc.get("jobs") or {}).items():
        steps = job.get("steps") or []
        runs = " \n".join(str(st.get("run") or "") for st in steps if isinstance(st, dict))
        if "engine-image" in runs and "image_contents.py" in runs:
            return []          # found a job that builds the image and checks it
    fails.append(
        "no CI job both runs `make engine-image` AND invokes qa/contract/image_contents.py. "
        "That gate is marked NOT APPLICABLE in `make check` because nothing builds an image "
        "there, on the promise that CI runs it — this is the check that the promise is kept. "
        "Either restore the step, or make the gate strict in `make check` again.")
    return fails



def main():
    try:
        import yaml
    except ImportError:
        print("PyYAML not installed — run 'make bootstrap'")
        return SKIP
    if not os.path.exists(CI):
        print("no .github/workflows/ci.yml")
        return SKIP

    try:
        d = yaml.safe_load(open(CI))
    except yaml.YAMLError as e:
        print("ci.yml is not valid YAML:\n%s" % e)
        return 1

    fails = []
    jobs = d.get("jobs") or {}
    if not jobs:
        fails.append("ci.yml defines no jobs")

    # `on:` parses as the boolean True in YAML 1.1 — check both spellings.
    triggers = d.get("on", d.get(True)) or {}
    if "push" not in triggers:
        fails.append("ci.yml does not trigger on push — main would never be verified")

    cip = str((d.get("concurrency") or {}).get("cancel-in-progress", ""))
    if cip.strip().lower() == "true":
        fails.append("concurrency.cancel-in-progress is unconditionally true: every merge to "
                     "main kills the previous run's verdict, so main reads as unverified. "
                     "Guard it with github.ref != 'refs/heads/main'.")

    for name, job in jobs.items():
        if not job.get("steps"):
            fails.append("job '%s' has no steps" % name)
        if job.get("timeout-minutes") is None:
            fails.append("job '%s' has no timeout-minutes — a hung job blocks the queue" % name)

    disabled = [n for n, j in jobs.items() if str(j.get("if", "")).strip().lower() == "false"]
    for n in disabled:
        if n not in DISABLED_OK:
            fails.append(
                "job '%s' is disabled with `if: false` and has no entry in DISABLED_OK — a "
                "disabled job reports the same green as a passing one. If it must stay off, "
                "add it below with the reason and the event that turns it back on." % n)

    fails.extend(check_relocated_gates(d))

    print("ci.yml: %d job(s) — %s" % (len(jobs), ", ".join(sorted(jobs))))
    print("cancel-in-progress: %s" % (cip or "(unset)"))
    if disabled:
        print("disabled: %s" % ", ".join(sorted(disabled)))
    if fails:
        print("\nci workflow lint: %d FAILED" % len(fails))
        for f in fails:
            print("  -", f)
        return 1
    print("\nci workflow lint: ok")
    return 0

if __name__ == "__main__":
    sys.exit(main())
