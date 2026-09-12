#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Move a Docker deployment's state to the native systemd layout.
#
#   sudo ./migrate-from-docker.sh [--volume wheel_wheel-data] [--data-dir /var/lib/wheel]
#                                 [--project wheel] [--replace] [--dry-run]
#
# NOT URGENT, AND DELIBERATELY SO. The live box stays on Docker; this exists so the move is a
# rehearsed procedure rather than an improvised one, and so the part worth proving -- master.key and
# the database arriving intact -- is proven before anyone needs it.
#
# THE ONE PROPERTY EVERYTHING ELSE RESTS ON: THE DOCKER VOLUME IS ONLY EVER READ.
#
# Every container this script runs mounts it `:ro`. There is no `docker volume rm`, no
# `docker compose down -v`, and no `-v` flag anywhere in this file -- which is grep-assertable, and
# infra/tests/native-migration.test.sh asserts it, the same way deploy.sh earns its "never -v"
# claim. So rollback is not a procedure this script has to implement correctly; it is a consequence
# of never having written to the thing you would roll back to.
#
# TWO VOLUMES ON THAT MACHINE BELONG TO A RETIRED STACK AND MUST NEVER BE TOUCHED:
# `wheel_hostdata` and `wheel_pgdata`. They are carried below as an explicit deny-list and this
# script refuses to read one even read-only, so a typo cannot start a conversation with them.
#
# ROLLBACK, in full:
#     sudo systemctl stop wheeld wheel-web
#     cd /opt/wheel-compose/infra/vps && ./deploy.sh
# The volume is exactly as it was. `--replace` additionally leaves the native tree it displaced at
# /var/lib/wheel.pre-migration-<stamp>, so the native side is undoable too.
set -euo pipefail

volume=wheel_wheel-data
data_dir=/var/lib/wheel
project=wheel
replace=0
dry_run=0

# Named, not pattern-matched. A pattern would be cleverer and would also be the thing that
# eventually matches something it should not; these are the two volumes on that machine, and they
# are written out so the refusal is legible in a code review rather than inferred from a regex.
PROTECTED="wheel_hostdata wheel_pgdata"

die() { echo "migrate: $*" >&2; exit 1; }
step() { echo "==> $*"; }
run() {
    if [ "$dry_run" = 1 ]; then printf '   (dry-run) would run:'; printf ' %q' "$@"; printf '\n'; return 0; fi
    "$@"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --volume) volume="${2:?--volume needs a name}"; shift 2 ;;
        --data-dir) data_dir="${2:?--data-dir needs a path}"; shift 2 ;;
        --project) project="${2:?--project needs a compose project name}"; shift 2 ;;
        --replace) replace=1; shift ;;
        --dry-run) dry_run=1; shift ;;
        -h|--help) sed -n '6,33p' "$0"; exit 0 ;;
        *) die "unknown argument $1 (see --help)" ;;
    esac
done

for protected in $PROTECTED; do
    [ "$volume" != "$protected" ] || die "$volume belongs to a retired stack and is never read, copied or removed by this script. If you genuinely mean to migrate something else, it is not this. The volume this migrates is wheel_wheel-data."
done

[ "$(id -u)" = 0 ] || die "run as root (sudo)"
command -v docker >/dev/null || die "docker is not installed — there is nothing to migrate from on this machine"
docker volume inspect "$volume" >/dev/null 2>&1 || die "no docker volume named '$volume'. docker volume ls, and read the header of this script about the two that must not be touched."

echo "migrate: $volume (read-only)  ->  $data_dir"
echo "         protected, never touched: $PROTECTED"
echo

# ---------------------------------------------------------------- the source must be at rest
#
# SQLite in WAL mode keeps wheel.db, -wal and -shm in agreement only when no writer is live. A copy
# taken while wheeld is running is a copy of a half-written transaction, and you find that out at
# restore time -- which is the moment you have no other copy. Stopping the container is also what
# drains it (stop_grace_period: 35s in compose.yml), so no turn is killed mid-flight.
running="$(docker compose -p "$project" ps -q wheeld 2>/dev/null || true)"
if [ -n "$running" ] && [ "$(docker inspect -f '{{.State.Running}}' "$running" 2>/dev/null)" = true ]; then
    die "the wheeld container in compose project '$project' is still running. Stop it first, which also drains it:

    cd /opt/wheel-compose/infra/vps && docker compose -p $project stop wheeld

Copying a live SQLite database in WAL mode copies a half-written transaction, and nothing notices until you restore it."
fi

helper=debian:bookworm-slim
# `:ro` on every mount of the source, every time. This is the property the whole script rests on,
# so it is written out at each use rather than hidden behind a variable someone could later change
# in one place and not notice in the others.
in_volume() { docker run --rm --network none -v "$volume:/src:ro" "$helper" "$@"; }

step "reading $volume"
if [ "$dry_run" = 1 ]; then
    echo "   (dry-run) would inventory the volume, copy it to $data_dir, and verify every file's sha256"
else
    inventory="$(in_volume sh -c 'cd /src && find . -type f | sort')"
    echo "$inventory" | sed 's/^/    /' | head -20
    total="$(printf '%s\n' "$inventory" | grep -c .)"
    [ "$total" -gt 20 ] && echo "    ... $total files in total"
    for must in ./master.key; do
        printf '%s\n' "$inventory" | grep -qx "$must" ||
            die "$volume contains no $must. Migrating without it would produce a board that cannot decrypt a single vault value — and the key is not derivable. Check --volume."
    done
    printf '%s\n' "$inventory" | grep -qx ./operator-token ||
        echo "    note: no operator-token in the volume. Not fatal (wheeld writes one on a first boot with no users), but it means the token you have been using will not survive this move."
fi

# ---------------------------------------------------------------- the destination
if [ -e "$data_dir" ] && [ -n "$(ls -A "$data_dir" 2>/dev/null)" ]; then
    [ "$replace" = 1 ] || die "$data_dir already exists and is not empty. Migrating on top of it would merge two boards' state, which is not a thing anyone can untangle afterwards. --replace moves it aside first (it is never deleted)."
    aside="$data_dir.pre-migration-$(date +%Y%m%dT%H%M%S)"
    step "moving the existing $data_dir aside to $aside (NOT deleted — this is the native-side rollback)"
    run systemctl stop wheeld wheel-web || true
    run mv "$data_dir" "$aside"
fi

step "copying"
run install -d -m 0700 -o wheel -g wheel "$data_dir"
if [ "$dry_run" = 0 ]; then
    # tar through a pipe rather than `docker cp`: it preserves modes and, more importantly, it
    # never needs a writable mount of the source.
    docker run --rm --network none -v "$volume:/src:ro" "$helper" tar -C /src -cf - . | tar -xf - -C "$data_dir"
    chown -R wheel:wheel "$data_dir"
    chmod 0700 "$data_dir"
    for secret in master.key operator-token; do
        [ -e "$data_dir/$secret" ] && chmod 0600 "$data_dir/$secret"
    done
fi

# ---------------------------------------------------------------- verify what arrived
#
# Not "the copy did not error". Every file's sha256, computed on both sides and compared. A tar
# through a pipe that loses a file at the end exits 0.
step "verifying every file against the source"
if [ "$dry_run" = 0 ]; then
    src_hashes="$(in_volume sh -c 'cd /src && find . -type f -print0 | sort -z | xargs -0 sha256sum')"
    dst_hashes="$(cd "$data_dir" && find . -type f -print0 | sort -z | xargs -0 sha256sum)"
    if [ "$src_hashes" != "$dst_hashes" ]; then
        echo "$src_hashes" > /tmp/wheel-migrate-src.$$
        echo "$dst_hashes" > /tmp/wheel-migrate-dst.$$
        diff /tmp/wheel-migrate-src.$$ /tmp/wheel-migrate-dst.$$ | head -20 >&2 || true
        rm -f /tmp/wheel-migrate-src.$$ /tmp/wheel-migrate-dst.$$
        die "the copy does not match the source. The Docker volume is untouched, so nothing is lost: fix the cause and run again."
    fi
    echo "    $(printf '%s\n' "$src_hashes" | grep -c .) files, every sha256 identical"

    # master.key by name as well as in the bulk comparison. It is the one file whose loss is
    # unrecoverable, and a check you can point at in a runbook is worth more than one implied by a
    # set comparison.
    echo "    master.key sha256 $(sha256sum "$data_dir/master.key" | cut -c1-16)… matches the volume's"

    # A database that arrived byte-identical can still have been corrupt in the volume. This is the
    # only check here that asks whether the DATA is sound rather than whether the COPY was faithful.
    for db in "$data_dir"/*.db; do
        [ -e "$db" ] || continue
        if command -v sqlite3 >/dev/null 2>&1; then
            result="$(sqlite3 "file:$db?mode=ro" 'PRAGMA integrity_check;' 2>&1 | head -1)"
            [ "$result" = ok ] || die "$(basename "$db") fails PRAGMA integrity_check: $result. The Docker volume is untouched; do not start the native stack on this."
            echo "    $(basename "$db") integrity_check ok"
        else
            echo "    $(basename "$db") NOT integrity-checked: sqlite3 is not installed. That is 'unknown', not 'fine' — apt-get install sqlite3 and re-run, or accept the gap knowingly."
        fi
    done
fi

# ---------------------------------------------------------------- prove it on the live box
step "starting the native stack"
run systemctl start wheeld
run systemctl start wheel-web

if [ "$dry_run" = 1 ]; then
    echo "   (dry-run) would then authenticate with the MIGRATED operator token and list projects"
    echo
    echo "DRY RUN complete. The volume was read but nothing was copied and nothing was started."
    exit 0
fi

step "proving the migration with the migrated credentials"
# wheeld.service's ExecStartPost already blocked until /healthz answered, so reaching here means it
# is serving. What is unproven at this point is whether it is serving THIS board: the store that
# just arrived, decrypted by the master.key that just arrived.
[ -r "$data_dir/operator-token" ] ||
    die "wheeld is up but there is no readable $data_dir/operator-token, so the migration cannot be proven end to end. Recover with: sudo -u wheel /opt/wheel/bin/wheeld token create --data-dir $data_dir"
projects="$(curl -fsS -m 15 http://127.0.0.1:8080/v1/projects \
    -H @<(printf 'x-auth-token: %s\n' "$(cat "$data_dir/operator-token")"))" ||
    die "the MIGRATED operator token did not authenticate against the native wheeld. The Docker volume is untouched — roll back with: systemctl stop wheeld wheel-web && cd /opt/wheel-compose/infra/vps && ./deploy.sh"
count="$(printf '%s' "$projects" | grep -o '"id"' | wc -l | tr -d ' ')"
echo "    the migrated operator token authenticates, and wheeld lists $count project(s) belonging to it"

cat <<EOF

Migrated. $volume was only ever read and is exactly as it was.

  check      sudo wheel-doctor
  compare    the project count above against what the Docker stack showed:
             docker compose -p $project run --rm -v $volume:/data:ro ... (or your own notes)
  back up    sudo $(dirname "$0")/backup.sh --to /var/backups/wheel
             DO THIS NOW. The native install is the live one; the volume is a snapshot that stops
             ageing from this moment, not a backup that keeps up with you.

  roll back  sudo systemctl stop wheeld wheel-web
             cd /opt/wheel-compose/infra/vps && ./deploy.sh

Once you are satisfied, the old stack's containers can be stopped with
\`docker compose -p $project down\` — NEVER with -v, which would take the volume you just proved
you can fall back to. And never touch:$(for p in $PROTECTED; do printf ' %s' "$p"; done) — a retired
stack's, not this one's.
EOF
