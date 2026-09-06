#!/usr/bin/env python3
"""Every TESTPLAN ID a test asserts must exist in docs/TESTPLAN.md.

The plan is the contract; the suites are the evidence. When they drift, both keep looking
healthy: the suite reports `ok SEC-vault-env-scope` and the plan lists a criterion nobody
tests, and no single file is wrong. I found three such IDs by hand today, which is exactly
the kind of check that decays if it depends on someone remembering.

Deliberately one-directional. An ID in the plan with no test is normal and expected — the
plan is written ahead of the code and half of it is M2/M3. An ID asserted by a test that the
plan has never heard of is the failure: it means a criterion is being checked under a name
nothing else in the project recognises, so it cannot be traced, reported on, or reviewed.

Sub-IDs (`SEC-vault-env-scope/wired`) trace to their parent. They are facets of one criterion
and enumerating every facet in the plan would make it a copy of the code.
"""
import os, re, sys

SKIP = 77
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
PLAN = os.path.join(ROOT, "docs", "TESTPLAN.md")
SUITES = [os.path.join(ROOT, "qa", "integration"), os.path.join(ROOT, "qa", "contract")]
# The Playwright specs assert criteria too, and until now this gate did not read them: it
# printed "every asserted ID is in the plan" while scanning only the Python suites. Three
# E2E-oauth-* IDs were added and the gate stayed green with none of them in TESTPLAN. A
# gate whose scope is narrower than its claim is the same failure it exists to prevent.
E2E_SUITES = [os.path.join(ROOT, "qa", "e2e", "tests")]

# `R.check("ID", ...)` / `R.skip("ID", ...)` — the two ways a suite claims an ID.
CALL = re.compile(r'R\.(?:check|skip)\(\s*"([^"]+)"')
# Playwright: test("ID: prose", ...). The ID is the leading token before the colon.
E2E_CALL = re.compile(r'\btest\(\s*["`]([A-Za-z][A-Za-z0-9-]*)\s*:')
# An ID assembled at runtime cannot be traced: `SEC-child-env-no-%s` matches nothing in the
# plan and reports nothing missing, so two S1 criteria sat in a suite and NOT in TESTPLAN
# with both this gate and a reader of the plan reporting all clear. Interpolating a
# SUB-case (`WM-setup/%s`) is fine — the parent carries the criterion — but interpolating
# into the ID body itself invents an untraceable criterion.
SYNTHETIC = re.compile(r'R\.(?:check|skip)\(\s*"([A-Za-z][A-Za-z0-9-]*)%')
# Setup and plumbing steps use `NAME/lowercase`; they are not criteria and are not traced.
PLUMBING = re.compile(r"^[A-Z][A-Za-z-]*/[a-z]")
# An ID may carry a human description after a colon; the ID is the part before it.
ID_SHAPE = re.compile(r"^[A-Z][A-Za-z0-9]*(?:-[A-Za-z0-9]+)+(?:/[A-Za-z0-9-]+)*$")


def normalise(raw):
    """The ID out of an assertion label. `CLI-whoami: exit 0` -> `CLI-whoami`.

    Labels carry prose so a failure reads as a sentence. That is worth keeping, so the
    tracer strips it rather than forcing every label to be a bare ID. Anything that does
    not then look like an ID — a format placeholder, a free-text label — is not traced,
    because inventing IDs for those would fill the plan with noise and train people to
    ignore this gate.
    """
    head = raw.split(":", 1)[0].strip()
    if "%" in head or not ID_SHAPE.match(head):
        return None
    return head


def synthetic_ids(path, text):
    """IDs whose body is built by interpolation — untraceable by construction."""
    return [(os.path.basename(path), m.group(1)) for m in SYNTHETIC.finditer(text)]


def plan_ids(text):
    return set(re.findall(r"`([A-Za-z][A-Za-z0-9-]*(?:/[A-Za-z0-9-]+)*)`", text))


def main():
    if not os.path.exists(PLAN):
        print("no docs/TESTPLAN.md")
        return SKIP
    known = plan_ids(open(PLAN).read())

    missing, checked, files, synthetic = {}, 0, 0, []
    for d in SUITES:
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            if not name.endswith(".py") or name == os.path.basename(__file__):
                continue
            files += 1
            src = open(os.path.join(d, name)).read()
            synthetic += synthetic_ids(name, src)
            for raw in CALL.findall(src):
                tid = normalise(raw)
                if tid is None or PLUMBING.match(tid):
                    continue
                checked += 1
                if tid.split("/")[0] not in known and tid not in known:
                    missing.setdefault(tid, []).append(name)

    for d in E2E_SUITES:
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            if not name.endswith(".ts"):
                continue
            files += 1
            src = open(os.path.join(d, name)).read()
            for raw in E2E_CALL.findall(src):
                tid = normalise(raw)
                if tid is None or PLUMBING.match(tid):
                    continue
                checked += 1
                if tid.split("/")[0] not in known and tid not in known:
                    missing.setdefault(tid, []).append(name)

    print("%d ID(s) asserted across %d suite file(s); %d named in TESTPLAN"
          % (checked, files, len(known)))
    if synthetic:
        print("\nid traceability: %d assertion(s) build the ID body at runtime" % len(synthetic))
        for where, prefix in sorted(set(synthetic)):
            print("  - %-38s in %s" % (prefix + "%...", where))
        print("\nSpell each criterion's ID out literally. An interpolated ID matches nothing "
              "in the plan, so this gate reports all clear while the criteria it names are "
              "untraced. Interpolating a sub-case after a '/' is fine.")
        return 1

    if missing:
        print("\nid traceability: %d asserted ID(s) are not in docs/TESTPLAN.md" % len(missing))
        for tid, where in sorted(missing.items()):
            print("  - %-38s asserted by %s" % (tid, ", ".join(sorted(set(where)))))
        print("\nAdd the criterion to the plan, or rename the assertion to the ID the plan "
              "already uses. A test under an unknown ID cannot be traced or reviewed.")
        return 1
    print("id traceability: every asserted ID is in the plan")
    return 0


if __name__ == "__main__":
    sys.exit(main())
