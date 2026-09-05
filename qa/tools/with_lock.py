#!/usr/bin/env python3
"""Run a command holding an exclusive file lock: with_lock.py <lockfile> <cmd...>

Five worktrees were each cold-compiling the same tree and OOM-killing one another's
cargo runs (PM's §1 ruling; SDK lost three builds to it, which is how a red gate reached
main). Serialising the cargo steps costs wall-clock only when builds actually overlap,
and turns "killed at random" into "waits its turn".

flock(1) is util-linux and absent on macOS, so this uses fcntl directly — the same
advisory lock, portable across both, with no dependency on which BSD/GNU userland the
developer happens to have.
"""
import fcntl, os, subprocess, sys, time


def main():
    if len(sys.argv) < 3:
        print("usage: with_lock.py <lockfile> <cmd...>", file=sys.stderr)
        return 2
    lockfile, cmd = sys.argv[1], sys.argv[2:]
    timeout = float(os.environ.get("WHEEL_LOCK_TIMEOUT", "1800"))
    quiet = os.environ.get("WHEEL_LOCK_QUIET") == "1"

    fd = os.open(lockfile, os.O_CREAT | os.O_RDWR, 0o666)
    start = time.time()
    announced = False
    while True:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            break
        except BlockingIOError:
            waited = time.time() - start
            if waited > timeout:
                print("with_lock: gave up after %.0fs waiting for %s" % (waited, lockfile),
                      file=sys.stderr)
                os.close(fd)
                # 75, not 77. Both mean "did not run", but they need different words:
                # 77 is "this gate cannot run here" (no cargo-llvm-cov, no docker) and is
                # answered by installing something. 75 is "another worktree held the lock
                # longer than I waited" — transient, and answered by running it again.
                # Collapsing them let a fully contended run report every Rust gate as an
                # ordinary skip, which locally reads as a passing check.
                return 75
            if not announced and not quiet:
                # Say so once: a silent 4-minute wait looks identical to a hang.
                print("  … waiting for the cargo lock (another worktree is building)",
                      file=sys.stderr, flush=True)
                announced = True
            time.sleep(1.0)

    try:
        return subprocess.call(cmd)
    finally:
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)


if __name__ == "__main__":
    sys.exit(main())
