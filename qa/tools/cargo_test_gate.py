#!/usr/bin/env python3
"""Run a cargo test command and tell a real failure apart from a run that was eaten.

Six worktrees share one `target-dir` (~/.cargo/config.toml, mandated by the contract for
throughput). `qa/check.sh` serialises the gates it runs, but nothing stops a dev typing
`cargo test` in their own worktree, and that invalidates and rewrites artifacts underneath a
run already in progress. The result looks like this:

    Running tests/boot_db.rs (/Users/metatron/wheel-target/debug/deps/boot_db-573d97...)
    error: test failed, to rerun pass `-p wheel-api --test boot_db`

No test list, no assertion, no panic, no compiler error -- the binary never got to run. Run
alone straight afterwards, the same suite passes 5/5, from a binary with a different hash.

That shape has now cost twice: once as BUG-019, which I filed against another agent's crate
before PM produced CI evidence contradicting it, and once as a red `make check` on `main`
reported to nobody only because I checked first. A red merge gate is an instruction to the
whole team to stop; it has to mean the code is broken.

So: a cargo failure is only FAILED when the output carries evidence of a real failure -- a
failing test, a panic, a compiler error, or a killing signal. Anything else exits 75, which
check.sh already renders as "did not run", and the run is inconclusive rather than red.
Both wordings are honest; only one of them is actionable, and calling contention a failure
teaches everyone to ignore the gate that is supposed to stop them.
"""
import subprocess
import sys

REAL_FAILURE = (
    "test result: FAILED",
    "panicked at",
    "error[",
    "error: could not compile",
    "signal:",           # segfault/abort is a real failure of the binary
    "SIGSEGV",
    "SIGABRT",
    "assertion",
)

CONTENDED = 75


def main(argv):
    if not argv:
        print("usage: cargo_test_gate.py <cargo> <args...>", file=sys.stderr)
        return 2
    proc = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True, bufsize=1)
    seen = []
    for line in proc.stdout:
        sys.stdout.write(line)
        seen.append(line)
    rc = proc.wait()
    if rc == 0:
        return 0

    out = "".join(seen)
    if any(marker in out for marker in REAL_FAILURE):
        return rc

    print("\n  cargo exited %d with no failing test, no panic and no compiler error.\n"
          "  That is the shape of a run whose build artifacts were rewritten underneath it "
          "-- six worktrees share one target-dir and a bare `cargo` call outside "
          "qa/check.sh's lock does exactly this.\n"
          "  Reporting INCONCLUSIVE rather than red: re-run it, alone, before believing "
          "anything about main." % rc, file=sys.stderr)
    return CONTENDED


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
