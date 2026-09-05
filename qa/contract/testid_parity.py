#!/usr/bin/env python3
"""Contract test: every data-testid the E2E suite selects must exist in web/src.

TESTPLAN: E2E-testids.

Playwright can only tell you a testid is missing by launching a browser, starting Next
and the mock, and failing a test 30 seconds in — and on a loaded machine it may not get
that far at all. The same drift is detectable statically in under a second, so it is,
here. This is the gate that catches "Web renamed a testid" and "QA guessed a name Web
never adopted", which is how an E2E suite quietly stops testing anything.

Template ids (`node-${name}`) are checked by their literal prefix, since the suffix is
data. A prefix no component emits is still a broken selector.

Selectors between DEFERRED-BEGIN and DEFERRED-END name UI a milestone has not built
yet. They are exempt from the missing check — and inverted: once web/src starts
rendering one, this gate FAILS, because a deferred selector that works is a test
somebody owes. That is the same discipline as a tracked known-bug fixture: the marker
expires by breaking, not by being forgotten.
"""
import os, re, sys

SKIP = 77
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
TESTIDS = os.path.join(ROOT, "qa", "e2e", "testids.ts")
WEB_SRC = os.path.join(ROOT, "web", "src")

LITERAL = re.compile(r':\s*"([a-z0-9][a-z0-9-]*)"', re.I)
TEMPLATE = re.compile(r'`([a-z0-9-]*?)\$\{', re.I)


def web_testids():
    found = set()
    for dirpath, _, files in os.walk(WEB_SRC):
        for fn in files:
            if not fn.endswith((".tsx", ".ts")):
                continue
            with open(os.path.join(dirpath, fn), errors="replace") as f:
                txt = f.read()
            found |= set(re.findall(r'data-testid=[\"\{]`?([a-z0-9-]+)', txt, re.I))
            # data-testid={`node-${x}`} -> record the literal prefix
            found |= set(re.findall(r'data-testid=\{`([a-z0-9-]*?)\$\{', txt, re.I))
    return found


def main():
    if not os.path.isdir(WEB_SRC):
        print("web/src not on main yet")
        return SKIP
    if not os.path.exists(TESTIDS):
        print("qa/e2e/testids.ts absent")
        return SKIP

    src = open(TESTIDS).read()
    deferred = set()
    for block in re.findall(r"DEFERRED-BEGIN(.*?)DEFERRED-END", src, re.S):
        deferred |= set(LITERAL.findall(block))
    wanted = {m for m in LITERAL.findall(src)} - deferred
    prefixes = {m for m in TEMPLATE.findall(src) if m}
    have = web_testids()

    missing = sorted(t for t in wanted if t not in have)
    landed = sorted(t for t in deferred if t in have)
    missing_prefix = sorted(p for p in prefixes
                            if not any(h.startswith(p) for h in have))

    print("testids referenced by the E2E suite: %d literal, %d templated"
          % (len(wanted), len(prefixes)))
    print("testids present in web/src:          %d" % len(have))
    if deferred:
        print("deferred to a later milestone:       %d (%s)"
              % (len(deferred), ", ".join(sorted(deferred))))

    if landed:
        print("\n%d DEFERRED selector(s) web/src now renders:" % len(landed))
        for t in landed:
            print("  - %s" % t)
        print("\nThe UI these name has shipped, so the E2E coverage for it is now owed.\n"
              "Move them out of the DEFERRED block and write the test.")
        return 1

    if missing or missing_prefix:
        print("\n%d selector(s) the E2E suite uses that WEB DOES NOT RENDER:"
              % (len(missing) + len(missing_prefix)))
        for t in missing:
            near = [h for h in sorted(have) if t.split("-")[0] in h or h.split("-")[-1] in t][:3]
            print("  - %-28s %s" % (t, ("did you mean: " + ", ".join(near)) if near else ""))
        for p in missing_prefix:
            print("  - %-28s (templated prefix)" % (p + "${...}"))
        print("\nThese are broken selectors: the suite would fail on all of them at runtime,\n"
              "30s into a browser launch. Either Web renamed them or QA proposed names Web\n"
              "never adopted — reconcile qa/e2e/testids.ts against web/src.")
        return 1

    print("\ntestid parity: every selector the E2E suite uses is rendered by web/src")
    return 0


if __name__ == "__main__":
    sys.exit(main())
