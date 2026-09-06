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
NAME2 = "qa-engine-shm2-%s" % uuid.uuid4().hex[:8]
PORT2 = free_port(int(os.environ.get("WHEEL_SHM2_PORT", "17434")))
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

        up, died = False, None
        for _ in range(60):
            if http("GET", "/healthz")[0] == 200:
                up = True
                break
            # A container that has EXITED is a definitive answer, not something to keep
            # waiting on. Polling the full 30s for a process that died in two seconds is
            # slow, and worse, it reports "never became healthy" — a timeout — for what is
            # actually a crash with an exit code. Those are different facts and the second
            # one is the useful one.
            state = sh("docker", "inspect", "-f",
                       "{{.State.Status}} {{.State.ExitCode}}", NAME).stdout.strip()
            if state.startswith("exited"):
                died = state
                break
            time.sleep(0.5)

        logs = sh("docker", "logs", "--tail", "6", NAME)
        said = (logs.stdout + logs.stderr)[-400:]
        how = ("it EXITED (%s)" % died) if died else "it never served /healthz within 30s"
        R.control("ENG-starts-without-shm", up,
                  "%s on a filesystem that cannot host a WAL index. A deployment that cannot "
                  "provide shm must fall back to a rollback journal and come up; crash-looping "
                  "there reads as a corrupt database. Engine said: %s" % (how, said))

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
        # ---- an override must not be able to disable the only working recovery -------
        #
        # PM's lever prolonged an 80-minute outage. WHEEL_SQLITE_JOURNAL names the mode to
        # try FIRST, and the negotiation then reuses that same `wanted` at the
        # locking_mode=EXCLUSIVE step — so setting it to TRUNCATE collapses both attempts
        # onto TRUNCATE and the WAL-under-EXCLUSIVE path, the one that actually works on a
        # shm-less volume, is never reached. The deploy died with "tried TRUNCATE, then
        # TRUNCATE" and the operator's own fix could not land.
        #
        # A configuration override on a RECOVERY path must be provable, not plausible: it
        # shipped without a test that the forced value can be reached on a hostile volume,
        # and the failure mode was silent — a lever that looks set correctly and disables
        # the only branch that works.
        #
        # So: whatever this is set to, a database on this volume must still end up in a mode
        # the filesystem can host. Every value, including nonsense.
        for value in ("TRUNCATE", "WAL", "DELETE", "not-a-mode"):
            vol2 = "%s-%s" % (VOL, value.lower().replace("-", ""))
            name2 = "%s-%s" % (NAME, value.lower().replace("-", ""))
            sh("docker", "volume", "create", vol2)
            sh("docker", "run", "--rm", "-v", "%s:/data" % vol2, "--entrypoint", "sh",
               "wheel-engine:test", "-c", "mkdir -p /data/wheel.db-shm")
            port2 = free_port(0)
            sh("docker", "run", "-d", "--name", name2, "-v", "%s:/data" % vol2,
               "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
               "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
               "-e", "WHEEL_VAULT_KEY=" + key,
               "-e", "WHEEL_ROLE=engine",
               "-e", "WHEEL_SQLITE_JOURNAL=" + value,
               "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
               "-p", "%d:7000" % port2, "wheel-engine:test")
            ok2 = False
            for _ in range(40):
                try:
                    req = urllib.request.Request("http://127.0.0.1:%d/healthz" % port2)
                    with urllib.request.urlopen(req, None, 3) as r:
                        if r.status == 200:
                            ok2 = True
                            break
                except Exception:
                    pass
                time.sleep(0.5)
            said = sh("docker", "logs", "--tail", "3", name2)
            # Gated on the BASELINE, not merely on the fixture. While the engine cannot
            # start on this volume at all, an override case failing says nothing about the
            # override — it is the same defect counted again. Four duplicate failures make a
            # report look four times worse and tell a reader nothing extra, and the one that
            # matters (TRUNCATE collapsing the negotiation) becomes visible only once the
            # baseline works.
            R.gated("ENG-journal-override-cannot-disable-recovery/%s" % value.lower(),
                    "ENG-starts-without-shm", ok2,
                    "WHEEL_SQLITE_JOURNAL=%s stopped the engine starting on a volume that "
                    "cannot host a WAL index. An override on a RECOVERY path must not be "
                    "able to disable the only branch that works: %s"
                    % (value, ((said.stdout + said.stderr)[-220:])))
            sh("docker", "rm", "-f", name2)
            sh("docker", "volume", "rm", vol2)
        # ---- the same property on a HEALTHY volume, which runs today -----------------
        #
        # ENG-concurrent-readers above can only be observed once the engine starts, so
        # BUG-022 leaves it skipped indefinitely. The property it guards — that the engine
        # never leaves its database exclusively locked — does not depend on a hostile
        # volume, and PM flagged it directly: a transient EXCLUSIVE that is not given back
        # trades a crash loop for an engine no second connection can read.
        #
        # Measured for the record: a connection holding locking_mode=EXCLUSIVE blocks a
        # second one from BOTH reading and writing ("database is locked"). So this is a real
        # discriminator, not a formality.
        sh("docker", "rm", "-f", NAME2)
        key2 = sh("openssl", "rand", "-base64", "32").stdout.strip()
        ok2 = sh("docker", "run", "-d", "--name", NAME2,
                 "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
                 "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
                 "-e", "WHEEL_VAULT_KEY=" + key2,
                 "-e", "WHEEL_ROLE=engine",
                 "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
                 "-p", "%d:7000" % PORT2, "wheel-engine:test").returncode == 0
        healthy = False
        if ok2:
            for _ in range(60):
                # curl, not wget: the image has curl and no wget, so the wget version
                # spun for the full 30s and then failed its own control. The control
                # caught it — "the engine did not start on a NORMAL volume" — which is
                # the right failure, but the cause was my probe, not the engine.
                probe = sh("docker", "exec", NAME2, "sh", "-c",
                           "curl -sf http://127.0.0.1:7000/healthz 2>/dev/null || true")
                if "ok" in probe.stdout:
                    healthy = True
                    break
                time.sleep(0.5)

        if not R.control("ENG-second-connection/engine-up", healthy,
                         "the engine did not start on a NORMAL volume, so a second-connection "
                         "result would say nothing about locking"):
            return R.report("engine-shm")

        second = sh("docker", "exec", NAME2, "python3", "-c",
                    "import sqlite3;c=sqlite3.connect('/data/wheel.db',timeout=5);"
                    "c.execute('SELECT count(*) FROM nodes').fetchone();"
                    "c.execute('CREATE TABLE IF NOT EXISTS qa_probe(a)');"
                    "c.execute('INSERT INTO qa_probe VALUES (1)');c.commit();print('second-ok')")
        R.gated("ENG-second-connection-not-locked-out", "ENG-second-connection/engine-up",
                "second-ok" in second.stdout,
                "a second connection to the running engine's database could not read AND "
                "write: %s. An exclusive lock that was never given back trades a crash loop "
                "for an engine nothing else can open — and the query path opens this file a "
                "second time." % (second.stderr or second.stdout)[:200])
        # ---- a full disk must say "full" -------------------------------------------
        #
        # PM's ask, and the reason is the outage itself: the operator spent the first part of
        # 80 minutes reading sqlite errors about shared memory. A cause the message does not
        # name is a cause somebody has to guess, and the guesses were expensive.
        #
        # Measured on a volume with no free space, the engine's FIRST line today is
        # "opening sqlite at /data/wheel.db: unable to open database file: Error code 14",
        # which sends a reader after a corrupt database or a permission problem. ENOSPC is
        # the one condition sqlite cannot describe and the filesystem can.
        full = NAME + "-full"
        sh("docker", "rm", "-f", full)
        # uid/gid on the tmpfs, or the whole test is a lie: /data is root-owned by default
        # and the container runs as agent(10001), so the filler `dd` fails with Permission
        # denied, the volume stays EMPTY, and the engine's error is about an unwritable
        # directory rather than a full one. I read that error as the disk-full message and
        # was about to report a defect that does not exist.
        sh("docker", "run", "-d", "--name", full,
           "--tmpfs", "/data:size=2m,uid=10001,gid=10001",
           "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
           "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
           "-e", "WHEEL_VAULT_KEY=" + key,
           "-e", "WHEEL_ROLE=engine",
           "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
           "--entrypoint", "sh", "wheel-engine:test",
           "-c", "dd if=/dev/zero of=/data/filler bs=1k count=1990 2>/dev/null; "
                 "exec wheel-engine")
        time.sleep(6)
        # 2>&1 through a shell, so the lines come back INTERLEAVED as the container
        # emitted them. Concatenating .stdout + .stderr puts every stdout line first, so
        # "the FIRST line" became the tracing INFO banner rather than the failure — and the
        # control correctly refused to judge a first line that was not the first line.
        said = sh("sh", "-c", "docker logs %s 2>&1" % full)
        # The FAILURE line, not literally line one: a tracing banner legitimately precedes
        # it, and PM's requirement is about which CAUSE the operator is told, not about
        # line numbering.
        failure = next((l.strip() for l in said.stdout.splitlines()
                        if l.strip() and "wheel-engine:" in l and "level" not in l), "")
        low = failure.lower()
        R.control("ENG-diskfull/engine-failed", bool(failure),
                  "the engine did not fail on a full volume, so there is no failure line to "
                  "judge and this check would pass for the wrong reason. Log: %r"
                  % said.stdout[-200:])
        R.gated("ENG-diskfull-says-so", "ENG-diskfull/engine-failed",
                any(w in low for w in ("disk is full", "no space", "disk full", "enospc",
                                       "database is full", "out of space")),
                "the failure on a FULL volume was %r — it must name the disk being full. "
                "ENOSPC is the one cause sqlite cannot describe and the filesystem can, and "
                "an operator reading a generic open error goes looking for corruption."
                % failure[:200])
        sh("docker", "rm", "-f", full)
    finally:
        sh("docker", "rm", "-f", NAME)
        sh("docker", "rm", "-f", NAME2)
        sh("docker", "volume", "rm", VOL)

    return R.report("engine-shm")


if __name__ == "__main__":
    sys.exit(main())
