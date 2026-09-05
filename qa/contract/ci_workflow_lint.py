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

    print("ci.yml: %d job(s) — %s" % (len(jobs), ", ".join(sorted(jobs))))
    print("cancel-in-progress: %s" % (cip or "(unset)"))
    if disabled:
        print("deliberately disabled: %s" % ", ".join(sorted(disabled)))
    if fails:
        print("\nci workflow lint: %d FAILED" % len(fails))
        for f in fails:
            print("  -", f)
        return 1
    print("\nci workflow lint: ok")
    return 0

if __name__ == "__main__":
    sys.exit(main())
