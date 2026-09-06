#!/usr/bin/env python3
"""No integration suite may steer the fake harness through the ENGINE's environment.

Since ADVERSARY F015 the engine starts every child from an empty environment and
repopulates a short allowlist, so `-e WHEEL_FAKE_...` on the engine container stops at the
engine. The child spawns perfectly and records nothing.

That is a uniquely bad failure shape, because it does not look like a test bug. It looks
like the ENGINE: "the child never spawned", "timed out waiting for the message to reach the
child's stdin". Two suites reported exactly those, against an engine doing its job, and I
nearly read the second as a delivery regression.

I made the F015-driven change, converted two suites, and left three behind. This gate is so
that the fourth cannot happen: steering belongs in /data/wheel-fake.json via
wheel_client.configure_fakes().
"""
import os, re, sys

SKIP = 77
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
SUITES = os.path.join(ROOT, "qa", "integration")

# `-e WHEEL_FAKE_x=` in a docker-run argument list. Passing it to `docker exec` is fine:
# that is a process the test starts itself, with no engine in between to strip it.
ENV_STEER = re.compile(r'"-e"\s*,\s*"(WHEEL_FAKE_[A-Z0-9_]+)=')


def main():
    if not os.path.isdir(SUITES):
        print("no qa/integration")
        return SKIP
    bad, files = [], 0
    for name in sorted(os.listdir(SUITES)):
        if not name.endswith(".py"):
            continue
        files += 1
        src = open(os.path.join(SUITES, name)).read()
        for m in ENV_STEER.finditer(src):
            # docker exec is legitimate; only a `docker run` of the ENGINE is the problem.
            window = src[max(0, m.start() - 800):m.start()]
            if '"run"' in window or '"docker", "run"' in window:
                bad.append((name, m.group(1)))
    print("fake steering: %d suite file(s) checked" % files)
    if bad:
        print("\n%d suite(s) steer the fake harness through the ENGINE's environment:" % len(bad))
        for name, var in bad:
            print("  - %-34s %s" % (name, var))
        print("\nF015 strips these on the way into the child, so they steer NOTHING. The "
              "child then records nothing and the suite reports it as an ENGINE fault.\n"
              "Use wheel_client.configure_fakes(<container>, key=value) instead.")
        return 1
    print("fake steering: every suite configures the fakes by file")
    return 0


if __name__ == "__main__":
    sys.exit(main())
