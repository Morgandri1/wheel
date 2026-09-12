#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# The safety properties of migrate-from-docker.sh and backup.sh, asserted against their source.
#
# These are DESTRUCTION-ADJACENT scripts that run as root against the one machine holding every
# secret on the board, and their central claims -- "the Docker volume is only ever read", "two
# named volumes are never touched", "the old tree is moved aside, not deleted" -- are properties of
# the text. A property of the text can be checked in milliseconds on every commit, and that is
# worth far more than a rehearsal nobody runs before merging.
#
# Same placement argument as infra/tests/prune-probe-projects.test.sh: plain bash, no deps, no
# network, no docker, sub-second, so exit 0/1 is honest and there is no "could not run" state.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
mig="$root/infra/vps/migrate-from-docker.sh"
bak="$root/infra/vps/backup.sh"

pass=0; fail=0
ok() { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf '  FAIL  %s\n      → %s\n' "$1" "$2" >&2; }

# The scripts' own prose discusses the flags they must never use ("NEVER with -v"), so every check
# below reads CODE ONLY -- comment lines stripped -- or it would be asserting against the
# documentation of the rule rather than the rule.
code() { grep -vE '^\s*#' "$1"; }

for f in "$mig" "$bak"; do
    [ -f "$f" ] || { bad "$(basename "$f") exists" "the kit references it"; }
    [ -x "$f" ] || { bad "$(basename "$f") is executable" "install docs tell the operator to run it directly"; }
done
[ -f "$mig" ] && [ -f "$bak" ] || { echo "native-migration: a script is missing" >&2; exit 1; }

echo "== migrate-from-docker.sh only ever READS the Docker volume =="

# The whole rollback story is "the volume is exactly as it was". That is not a procedure this
# script implements; it is a consequence of never writing. So the absence of every write verb is
# the actual test, and it is what makes rollback true by construction.
if code "$mig" | grep -qE 'docker\s+volume\s+rm'; then
    bad "no 'docker volume rm'" "it would delete the only copy of master.key that is not the one being migrated"
else ok; fi

# `down` IMMEDIATELY followed by the flag. A looser pattern matched the closing instructions'
# own sentence -- "docker compose -p $project down -- NEVER with -v" -- which is prose telling the
# operator the rule, inside a heredoc that code() cannot strip. A test that fails on the
# documentation of the rule it enforces teaches people to weaken the test.
if code "$mig" | grep -qE '\bdown\s+(-v\b|--volumes\b)'; then
    bad "no 'compose down -v'" "-v removes the project's volumes, which is exactly the fallback this migration depends on"
else ok; fi

if code "$mig" | grep -qE 'docker\s+run[^|]*-v\s+[^ ]*:/src(?!:ro)' 2>/dev/null ||
   code "$mig" | grep -E 'docker\s+run' | grep -E '\-v\s+[^ ]+:/src' | grep -qv ':ro'; then
    bad "every mount of the source volume is :ro" "a writable mount of the source makes 'the volume is untouched' a hope rather than a fact"
else ok; fi

# Every -v that names the volume variable must carry :ro. Checked positively as well as negatively,
# because a future edit is more likely to ADD a mount than to change an existing one.
mounts="$(code "$mig" | grep -oE '\-v "\$volume:[^"]*"' | sort -u)"
if [ -z "$mounts" ]; then
    bad "the source volume is mounted at all" "if nothing mounts \$volume, this test is checking a script that no longer reads it"
elif printf '%s\n' "$mounts" | grep -qv ':ro"$'; then
    bad "every \$volume mount ends in :ro" "found: $(printf '%s' "$mounts" | tr '\n' ' ')"
else ok; fi

echo "== the two retired volumes are refused by name =="
for protected in wheel_hostdata wheel_pgdata; do
    if code "$mig" | grep -q "$protected"; then ok
    else bad "$protected is named in the deny-list" "the operator said these belong to a retired stack and must never be touched; a script that does not know their names cannot refuse them"; fi
done
# Named, not pattern-matched: a regex would eventually match something it should not.
if code "$mig" | grep -q 'PROTECTED='; then ok
else bad "there is a PROTECTED list" "the refusal must be a legible list, not an inferred pattern"; fi

echo "== nothing is deleted; things are moved aside =="
# A migration and a restore are exactly when you discover the source was not what you thought, and
# the thing just overwritten is the only other copy.
for f in "$mig" "$bak"; do
    n="$(basename "$f")"
    # Two lines, not one: the timestamped path is built into `aside` and `mv` uses it a few lines
    # later, so requiring both on one line tested a coding style rather than the property.
    if code "$f" | grep -qE '^\s*aside=.*(pre-migration|pre-restore)' && code "$f" | grep -qE '\bmv\b[^|]*"\$aside"'; then ok
    else bad "$n moves the displaced tree aside" "overwriting it makes the operation one-way"; fi
    if code "$f" | grep -qE 'rm\s+-rf?\s+"?\$(data_dir|to)"?\s*$'; then
        bad "$n never rm -rf's the data directory" "that is the one directory whose loss is unrecoverable"
    else ok; fi
done

echo "== the migration verifies rather than assumes =="
if code "$mig" | grep -q 'sha256sum'; then ok
else bad "migration compares sha256 on both sides" "a tar through a pipe that truncates still exits 0"; fi
# sqlite3 AND the pragma ON THE SAME LINE. Grepping for the pragma name alone passed against a
# mutation that replaced the `sqlite3` invocation with `true` and left the argument behind -- the
# string survived, the check did not run, and this test said nothing.
if code "$mig" | grep -qE 'sqlite3[^|]*integrity_check'; then ok
else bad "migration actually invokes sqlite3 ... integrity_check" "a byte-identical copy of a corrupt database is still corrupt"; fi
if code "$mig" | grep -q 'master.key'; then ok
else bad "migration checks master.key by name" "losing it loses every vault secret, permanently and unrecoverably"; fi
if code "$mig" | grep -q 'operator-token'; then ok
else bad "migration proves the MIGRATED operator token still authenticates" "a wheeld that serves /healthz proves the listener, not that this board's store decrypted"; fi

echo "== backup.sh =="
if code "$bak" | grep -qE 'systemctl stop wheeld'; then ok
else bad "backup stops wheeld first" "SQLite in WAL mode copied live is a half-written transaction, and you find out at restore time"; fi
if code "$bak" | grep -q 'MANIFEST'; then ok
else bad "backup writes a checksum manifest into the archive" "an archive nobody has read back is a hope, not a backup"; fi
if code "$bak" | grep -qE '\-\-verify'; then ok
else bad "backup can verify an archive" "there must be a way to check a backup without restoring it over a working install"; fi
if grep -q 'gpg --symmetric' "$bak"; then ok
else bad "backup tells the operator the archive holds master.key in the clear" "it is mode 0600 on this machine and nothing anywhere else"; fi
# The daemon must come back even when the tar fails; a backup that leaves the product down because
# the disk filled is a worse outage than the missing backup.
if code "$bak" | grep -qE 'trap restart_if_needed EXIT'; then ok
else bad "backup restarts wheeld even if archiving fails" "otherwise a full disk turns a backup into an outage"; fi

echo
echo "native-migration: $pass passed, $fail failed"
[ "$fail" = 0 ]
