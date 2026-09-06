#!/usr/bin/env python3
"""A volume that kills a WAL database the way the deployed one does.

WHY THIS EXISTS. The engine's journal conversion has a call site that is unreachable on a
healthy disk: SDK verified it by deleting the drain body and watching their suite stay
green. The deployed Railway volume cannot resize a `-shm` segment, so a database ALREADY in
WAL cannot be converted away — leaving WAL needs a checkpoint, the checkpoint needs the
wal-index, and the wal-index is the thing that just failed. No test on a working filesystem
reaches that path.

WHAT IT MAKES. A directory containing `wheel.db` that is already in WAL, with rows, whose
`-shm` path is a DIRECTORY so sqlite can never map it. Verified to reproduce the deployed
symptom: a naive `PRAGMA journal_mode=TRUNCATE` on it fails with "database is locked".

HOW TO USE IT from a Rust test (point it at `wheel_sqlite::configure_journal`):

    python3 qa/fixtures/wal_blocked_shm.py prepare /tmp/fx   # -> prints the db path
    <open that db with configure_journal, converting it>
    python3 qa/fixtures/wal_blocked_shm.py verify /tmp/fx/wheel.db truncate

`verify` asserts all three properties SDK asked for and exits non-zero naming the one that
failed:
  * the journal mode ENDS as the target;
  * every row is still present (a conversion that loses data is not a conversion);
  * a SECOND connection can open AND WRITE — which is the only proof the transient
    exclusive lock was actually given back. Dropping the pragma does not release it; a
    transaction has to complete.

IT CARRIES ITS OWN CONTROL. `prepare` refuses to report success unless it has confirmed the
state is genuinely hostile: already WAL, and a naive conversion genuinely fails. Without
that, a green from any test using this fixture would only mean the disk was healthy — which
is the exact hole the fixture exists to close.
"""
import os
import shutil
import sqlite3
import sys

ROWS = 5


def prepare(directory):
    shutil.rmtree(directory, ignore_errors=True)
    os.makedirs(directory)
    db = os.path.join(directory, "wheel.db")

    c = sqlite3.connect(db)
    c.execute("PRAGMA journal_mode=WAL")
    c.execute("CREATE TABLE fixture_rows(n INTEGER)")
    c.executemany("INSERT INTO fixture_rows VALUES (?)", [(i,) for i in range(ROWS)])
    c.commit()
    mode = c.execute("PRAGMA journal_mode").fetchone()[0]
    c.close()
    if mode != "wal":
        raise SystemExit("fixture is not hostile: the db did not enter WAL (got %r)" % mode)

    shm = db + "-shm"
    if os.path.exists(shm):
        os.remove(shm)
    os.makedirs(shm)

    # Control: prove the state actually breaks a naive conversion. If this SUCCEEDS the
    # fixture is not reproducing the deployed volume, and anything gated on it would pass
    # for the wrong reason.
    probe = sqlite3.connect(db)
    try:
        probe.execute("PRAGMA journal_mode=TRUNCATE")
        probe.execute("INSERT INTO fixture_rows VALUES (99)")
        probe.commit()
        raise SystemExit(
            "fixture is NOT hostile: a naive conversion succeeded on it, so this volume "
            "does not reproduce the deployed one and no test using it means anything")
    except sqlite3.OperationalError:
        pass
    finally:
        probe.close()
    return db


def verify(db, target):
    problems = []
    c = sqlite3.connect(db)
    mode = c.execute("PRAGMA journal_mode").fetchone()[0]
    if mode.lower() != target.lower():
        problems.append("journal mode is %r, expected %r" % (mode, target))
    try:
        n = c.execute("SELECT count(*) FROM fixture_rows").fetchone()[0]
        if n < ROWS:
            problems.append("rows lost in the conversion: %d of %d" % (n, ROWS))
    except sqlite3.Error as e:
        problems.append("rows unreadable after conversion: %s" % e)

    # The second connection is the whole point. A transient EXCLUSIVE lock that was never
    # given back leaves the file readable by its holder and unusable by anyone else, and
    # the query path opens this file a second time.
    try:
        second = sqlite3.connect(db, timeout=5)
        second.execute("INSERT INTO fixture_rows VALUES (1000)")
        second.commit()
        second.close()
    except sqlite3.Error as e:
        problems.append("a SECOND connection could not write (%s) — the transient exclusive "
                        "lock was not released; dropping the pragma does not release it, a "
                        "transaction has to complete" % e)
    c.close()

    for p in problems:
        print("FAIL: " + p)
    return 1 if problems else 0


def main(argv):
    if len(argv) >= 3 and argv[1] == "prepare":
        print(prepare(argv[2]))
        return 0
    if len(argv) >= 4 and argv[1] == "verify":
        return verify(argv[2], argv[3])
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
