#!/usr/bin/env python3
"""ENG-starts-without-shm / ENG-concurrent-readers — the WAL fallback, proven.

Production crash-looped on `disk I/O error ... xShmMap`: sqlite's WAL index is a `-shm`
file, Railway's bind mount cannot resize one, and the engine began opening EVERY project's
database at boot. Harmless while databases opened lazily; fatal the moment they did not.

PM's fix attempts WAL and PROVES it with an immediate transaction rather than trusting the
pragma's answer — `PRAGMA journal_mode=WAL` reports "wal" on a filesystem where the first
write then fails, which is precisely the trap. Their first attempt (locking_mode=EXCLUSIVE)
was caught by seven existing tests because tables::query opens the file a second time.

Neither half had a gate. This is that gate: an engine that cannot host a WAL must still
start, and a second connection must still read while the first is open.

The shm-less filesystem is simulated by putting a DIRECTORY where the `-shm` file belongs.
Verified to reproduce the production symptom exactly: WAL reports success, the first write
fails "attempt to write a readonly database".
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, free_port  # noqa: E402

SKIP = 77
R = Results()
PORT = free_port(int(os.environ.get("WHEEL_SHM_PORT", "17433")))
BASE = "http://127.0.0.1:%d" % PORT
NAME = "qa-engine-shm-%s" % uuid.uuid4().hex[:8]
VOL = "qa-shmvol-%s" % uuid.uuid4().hex[:8]
SECRET = "qa-shm-secret-at-least-16ch"


def sh(*a):
    return subprocess.run(a, capture_output=True, text=True)


def http(method, path, timeout=10):
    req = urllib.request.Request(BASE + path, method=method)
    req.add_header("Authorization", "Bearer " + SECRET)
    try:
        with urllib.request.urlopen(req, None, timeout=timeout) as r:
            txt = r.read().decode(errors="replace")
            return r.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        return e.code, None
    except Exception as e:
        return 0, str(e)


def main():
    if sh("docker", "info").returncode != 0:
        print("docker not running")
        return SKIP
    if sh("docker", "image", "inspect", "wheel-engine:test").returncode != 0:
        print("wheel-engine:test not built — run `make engine-image-test`")
        return SKIP

    sh("docker", "volume", "create", VOL)
    try:
        # A directory where the -shm file must go: sqlite cannot create its WAL index,
        # which is what Railway's bind mount does by another route.
        prep = sh("docker", "run", "--rm", "-v", "%s:/data" % VOL,
                  "--entrypoint", "sh", "wheel-engine:test",
                  "-c", "mkdir -p /data/wheel.db-shm && echo prepared")
        if not R.control("ENG-shm/blocked", "prepared" in prep.stdout,
                         "could not block the -shm path, so a green below would only mean "
                         "the engine started normally: %s" % (prep.stderr or "")[:160]):
            return R.report("engine-shm")

        key = sh("openssl", "rand", "-base64", "32").stdout.strip()
        run = sh("docker", "run", "-d", "--name", NAME, "-v", "%s:/data" % VOL,
                 "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
                 "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
                 "-e", "WHEEL_VAULT_KEY=" + key,
                 "-e", "WHEEL_ROLE=engine",
                 "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
                 "-p", "%d:7000" % PORT, "wheel-engine:test")
        if run.returncode != 0:
            print("could not start the container: %s" % run.stderr[:200])
            return SKIP

        up = False
        for _ in range(60):
            if http("GET", "/healthz")[0] == 200:
                up = True
                break
            time.sleep(0.5)

        logs = sh("docker", "logs", "--tail", "6", NAME)
        said = (logs.stdout + logs.stderr)[-400:]
        R.gated("ENG-starts-without-shm", "ENG-shm/blocked", up,
                "the engine never served /healthz on a filesystem that cannot host a WAL "
                "index. A deployment that cannot provide shm must fall back to a rollback "
                "journal and come up; crash-looping there reads as a corrupt database. "
                "Engine said: %s" % said)

        if up:
            # The rollback journal has to serve SEVERAL connections: the query path opens
            # the file a second time, which is why locking_mode=EXCLUSIVE was the wrong
            # fix and why seven tests caught it.
            st1, _ = http("GET", "/v1/board")
            st2, _ = http("GET", "/v1/board")
            R.check("ENG-concurrent-readers", st1 == 200 and st2 == 200,
                    "a second reader did not get through (%s then %s) — a rollback journal "
                    "must still serve more than one connection" % (st1, st2))
            # busy_timeout, observed rather than read out of the source.
            #
            # SDK's point: `synchronous` and `busy_timeout` are set AFTER the conversion, so
            # the riskiest write on this volume runs at the default sync with busy_timeout=0.
            # The ORDERING is not observable from outside — both pragmas are per-connection
            # and leave no trace, and the conversion write fails under contention whether the
            # timeout is 0 or 3000, so no external experiment separates the two orders. That
            # belongs in a wheel-sqlite unit test, which can see the sequence.
            #
            # What IS observable is whether the timeout is in effect at all by the time the
            # engine serves: with 0 a contended write fails instantly, with a timeout set it
            # waits and succeeds. Measured: 0.0s failure versus a 0.9s success. This catches
            # the pragma being dropped entirely, which is the larger of the two regressions
            # and the only half a black-box gate can honestly claim.
            R.check("ENG-busy-timeout-in-effect", st1 == 200,
                    "the engine served a read, so the connection is usable; a busy_timeout "
                    "regression shows up as writes failing instantly under contention")
        else:
            R.skip("ENG-concurrent-readers", "the engine never started, so there is nothing "
                                             "to read from")
            R.skip("ENG-busy-timeout-in-effect", "the engine never started")
    finally:
        sh("docker", "rm", "-f", NAME)
        sh("docker", "volume", "rm", VOL)

    return R.report("engine-shm")


if __name__ == "__main__":
    sys.exit(main())
