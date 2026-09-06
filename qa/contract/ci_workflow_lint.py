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

def check_pnpm_pinned(doc):
    """Every pnpm/action-setup must pin a version.

    The action reads `packageManager` from the package.json at the REPO ROOT; ours lives in
    web/, so an unpinned setup dies with "No pnpm version is specified" before a single
    assertion runs. My packaged-web job did exactly that -- red for a whole CI run, and a job
    that never STARTED is indistinguishable in the summary from one that found a bug.
    """
    fails = []
    for name, job in (doc.get("jobs") or {}).items():
        for st in job.get("steps") or []:
            if not isinstance(st, dict):
                continue
            if str(st.get("uses") or "").startswith("pnpm/action-setup"):
                with_ = st.get("with") or {}
                if not with_.get("version"):
                    fails.append(
                        "job '%s' uses pnpm/action-setup without `with: {version: N}` — the "
                        "action looks for packageManager in the ROOT package.json and ours is "
                        "in web/, so this job will die at setup before running anything"
                        % name)
    return fails


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



def check_size_gate_runs(doc):
    """DEP-binary-size is not in `make check`, so CI is the only place it runs.

    Same trap as qa:image-contents one function up: a gate that needs an expensive build
    gets moved out of the fast job for good reasons, and then nothing runs it at all. The
    crate-count half rides in `make check` and needs no assertion here; the size half has
    exactly one home, so this asserts that home still exists.
    """
    for name, job in (doc.get("jobs") or {}).items():
        steps = job.get("steps") or []
        runs = " \n".join(str(st.get("run") or "") for st in steps if isinstance(st, dict))
        if "make size" in runs or "size_gate.py" in runs:
            return []
    return ["no CI job runs `make size`. DEP-binary-size needs a release build so it is "
            "deliberately not in `make check`; that is only honest while something else "
            "runs it. Either restore the job or put the gate back in check."]


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
    fails.extend(check_size_gate_runs(d))
    fails.extend(check_pnpm_pinned(d))

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
