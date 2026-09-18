#!/usr/bin/env python3

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""Runs `infra/dev/e2e.py` (API's documented manual sanity script) as part of the suite, so the
one positive path it covers -- an owner starts a sandbox and reads a real board back through the
authenticated proxy -- stops being invisible to CI. `test_api_projects.py`'s start test only
asserts `status < 500`; nothing else in this suite actually reads a board back.

Script-style (has a `__main__` guard), per `run.sh`'s own dispatch convention -- and it must be,
not pytest-style: `run.sh`'s result-line guard requires literal "passed"/"failed"/"error"/"no
tests ran" in the output, which `e2e.py`'s own "PASS"/"FAIL"/"RESULT: ALL PASS" lines do not
contain. Printing an explicit line here, keyed off the real exit code, is what makes this wrapper
visible to that guard rather than silently flagged as a suite that "produced no result line" --
exactly the failure mode `run.sh`'s own history warns about.
"""
import os
import subprocess
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))


def main():
    r = subprocess.run([sys.executable, "infra/dev/e2e.py"], cwd=ROOT)
    print("1 passed" if r.returncode == 0 else "1 failed")
    return r.returncode


if __name__ == "__main__":
    sys.exit(main())
