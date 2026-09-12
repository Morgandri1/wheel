#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Back up a native Wheel install's data directory, and restore one.
#
#   sudo ./backup.sh [--to <dir>] [--data-dir <dir>] [--keep <n>] [--no-stop] [--dry-run]
#   sudo ./backup.sh --restore <archive> [--data-dir <dir>] [--dry-run]
#   sudo ./backup.sh --verify <archive>
#
# LOSING master.key LOSES EVERY VAULT SECRET ON THE BOARD, PERMANENTLY. It is not derived from
# anything and it is not recoverable: it decrypts every project's engine secret and vault key
# (crates/wheeld/src/supervise.rs). Every Anthropic key, every OAuth token, every credential an
# agent was given goes with it. The database is replaceable by comparison -- you would lose work,
# not the ability to ever read your own secrets again.
#
# THREE THINGS THIS DOES THAT A `tar -czf` DOES NOT, each of which is a way people lose data while
# believing they have a backup:
#
#   1. It STOPS wheeld first. SQLite in WAL mode keeps `wheel.db`, `wheel.db-wal` and `wheel.db-shm`
#      in agreement only at rest. A tar taken while a writer is live copies a half-written
#      transaction, and the failure is not visible until you restore it -- at the exact moment you
#      have no working copy. --no-stop exists for a copy you know is only a snapshot of the secrets;
#      it labels the archive as such and refuses to be restored by this script without --force.
#   2. It VERIFIES what it wrote. The archive is listed back and every file's sha256 compared to the
#      source. An archive nobody has ever read is a hope, not a backup.
#   3. It REFUSES TO BE QUIET about encryption. The archive contains master.key in the clear. This
#      script does not encrypt it for you -- a key management scheme chosen by a backup script is a
#      key management scheme nobody knows they have -- but it will not finish without telling you,
#      by name, what you are now holding and what `gpg --symmetric` would cost you.
set -euo pipefail

data_dir=/var/lib/wheel
to=/var/backups/wheel
keep=7
stop=1
dry_run=0
restore=""
verify=""
force=0

die() { echo "backup: $*" >&2; exit 1; }
step() { echo "==> $*"; }
run() {
    if [ "$dry_run" = 1 ]; then printf '   (dry-run) would run:'; printf ' %q' "$@"; printf '\n'; return 0; fi
    "$@"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --to) to="${2:?--to needs a directory}"; shift 2 ;;
        --data-dir) data_dir="${2:?--data-dir needs a path}"; shift 2 ;;
        --keep) keep="${2:?--keep needs a number}"; shift 2 ;;
        --no-stop) stop=0; shift ;;
        --restore) restore="${2:?--restore needs an archive}"; shift 2 ;;
        --verify) verify="${2:?--verify needs an archive}"; shift 2 ;;
        --force) force=1; shift ;;
        --dry-run) dry_run=1; shift ;;
        -h|--help) sed -n '6,40p' "$0"; exit 0 ;;
        *) die "unknown argument $1 (see --help)" ;;
    esac
done

# Every file in the archive, and its sha256. Written into the archive itself as MANIFEST so a
# restore can check what it is unpacking without needing this script's source of truth to still
# exist -- an archive has to be self-describing or it stops being verifiable the day the repo moves.
manifest_of() { # manifest_of <root>
    ( cd "$1" && find . -type f ! -name MANIFEST -print0 | sort -z | xargs -0 sha256sum )
}

# ---------------------------------------------------------------- verify
if [ -n "$verify" ]; then
    [ -f "$verify" ] || die "$verify does not exist"
    tmp="$(mktemp -d)"; chmod 0700 "$tmp"; trap 'rm -rf "$tmp"' EXIT
    step "verifying $verify"
    tar -xzf "$verify" -C "$tmp"
    [ -f "$tmp/MANIFEST" ] || die "$verify has no MANIFEST — it was not written by this script, so its contents cannot be checked against what was backed up"
    ( cd "$tmp" && sha256sum --quiet -c MANIFEST ) || die "CONTENTS DO NOT MATCH THE MANIFEST. This archive is damaged; do not restore it over a working install."
    for must in master.key; do
        [ -f "$tmp/$must" ] || die "$verify contains no $must. Whatever else is in it, it cannot restore this board's secrets."
    done
    n="$(grep -c . "$tmp/MANIFEST")"
    echo "  ok: $n files, all hashes match, master.key present"
    grep -q '^# taken-live' "$tmp/MANIFEST" && echo "  WARNING: taken with --no-stop, so the database may be mid-transaction. Good for the secrets, not trustworthy for the store."
    exit 0
fi

[ "$(id -u)" = 0 ] || die "run as root (sudo): the data directory is 0700 and owned by the service user"

# ---------------------------------------------------------------- restore
if [ -n "$restore" ]; then
    [ -f "$restore" ] || die "$restore does not exist"
    tmp="$(mktemp -d)"; chmod 0700 "$tmp"; trap 'rm -rf "$tmp"' EXIT
    step "checking the archive before touching anything"
    tar -xzf "$restore" -C "$tmp"
    [ -f "$tmp/MANIFEST" ] || die "$restore has no MANIFEST"
    ( cd "$tmp" && sha256sum --quiet -c MANIFEST ) || die "the archive does not match its own manifest — refusing to restore damaged data over a working install"
    if grep -q '^# taken-live' "$tmp/MANIFEST" && [ "$force" = 0 ]; then
        die "this archive was taken with --no-stop, so its database may be a half-written transaction. --force if you accept that (the secrets in it are fine; the store may not be)."
    fi
    rm -f "$tmp/MANIFEST"

    # The old tree is MOVED, never deleted. A restore is exactly when you find out the archive was
    # not what you thought, and the thing you just overwrote is the only other copy.
    aside="$data_dir.pre-restore-$(date +%Y%m%dT%H%M%S)"
    step "stopping wheeld"
    run systemctl stop wheeld wheel-web || true
    if [ -e "$data_dir" ]; then
        step "moving the current data directory aside to $aside (NOT deleted)"
        run mv "$data_dir" "$aside"
    fi
    step "restoring into $data_dir"
    run install -d -m 0700 -o wheel -g wheel "$data_dir"
    if [ "$dry_run" = 0 ]; then
        tar -c -C "$tmp" . | tar -x -C "$data_dir"
        chown -R wheel:wheel "$data_dir"
        chmod 0700 "$data_dir"
        for secret in master.key operator-token; do
            [ -e "$data_dir/$secret" ] && chmod 0600 "$data_dir/$secret"
        done
    fi
    step "starting wheeld"
    run systemctl start wheeld
    run systemctl start wheel-web
    echo
    echo "Restored from $restore."
    echo "  the previous data directory is $aside — check the board before you remove it"
    echo "  verify:  sudo wheel-doctor health"
    exit 0
fi

# ---------------------------------------------------------------- back up
[ -d "$data_dir" ] || die "$data_dir does not exist — there is nothing to back up"
[ -f "$data_dir/master.key" ] || die "$data_dir has no master.key. Either this is not a Wheel data directory, or the thing most worth backing up is already gone."

stamp="$(date +%Y%m%dT%H%M%S)"
archive="$to/wheel-data-$stamp.tar.gz"
run install -d -m 0700 -o root -g root "$to"

was_active=0
if [ "$stop" = 1 ]; then
    systemctl is-active --quiet wheeld && was_active=1
    if [ "$was_active" = 1 ]; then
        # The drain, not a kill: KillMode=mixed plus TimeoutStopSec=35 lets turns in flight finish
        # (~28s) before agents are stopped. A backup that interrupts a turn costs an agent's work.
        step "stopping wheeld (drains in-flight turns, up to 35s)"
        run systemctl stop wheeld
    fi
else
    step "NOT stopping wheeld (--no-stop): the secrets in this archive are fine, the database may be mid-transaction"
fi

restart_if_needed() {
    if [ "$was_active" = 1 ]; then
        step "starting wheeld again"
        run systemctl start wheeld || echo "backup: WARNING: wheeld did not restart — journalctl -u wheeld -n 50" >&2
    fi
}
# The daemon comes back even if the tar fails. A backup script that leaves the product down because
# it ran out of disk is a worse outage than the missing backup.
trap restart_if_needed EXIT

step "archiving $data_dir -> $archive"
if [ "$dry_run" = 1 ]; then
    echo "   (dry-run) would write $archive (0600) with a sha256 MANIFEST, verify it, and keep the newest $keep"
else
    tmp="$(mktemp -d)"; chmod 0700 "$tmp"
    manifest_of "$data_dir" > "$tmp/MANIFEST"
    [ "$stop" = 1 ] || echo "# taken-live (--no-stop): the database in this archive may be mid-transaction" >> "$tmp/MANIFEST"
    ( umask 077 && tar -czf "$archive.part" -C "$data_dir" . -C "$tmp" MANIFEST )
    mv -f "$archive.part" "$archive"
    chmod 0600 "$archive"
    rm -rf "$tmp"
fi

# READ IT BACK. Every other step so far describes what was intended; this is the only one that
# reports what is actually on disk.
step "verifying the archive that was just written"
if [ "$dry_run" = 0 ]; then
    "$0" --verify "$archive" || die "the archive just written does not verify. It is at $archive; do not rely on it."
fi

if [ "$dry_run" = 0 ] && [ "$keep" -gt 0 ]; then
    step "keeping the newest $keep archives in $to"
    # shellcheck disable=SC2012
    ls -1t "$to"/wheel-data-*.tar.gz 2>/dev/null | tail -n "+$((keep + 1))" | while read -r old; do
        echo "    removing $old"
        rm -f "$old"
    done
fi

restart_if_needed
trap - EXIT

[ "$dry_run" = 0 ] || exit 0
cat <<EOF

$archive  ($(du -h "$archive" | cut -f1))

THIS FILE CONTAINS master.key IN THE CLEAR. Whoever holds it holds every secret on this board:
every vault value, every agent credential, the operator token. It is mode 0600 and root-owned here,
which protects it on this machine and nowhere else.

Before it leaves this server, encrypt it:
  gpg --symmetric --cipher-algo AES256 $archive     # then copy the .gpg, and only the .gpg

And copy it OFF this machine. A backup on the disk you are backing up is a copy, not a backup:
  scp $archive.gpg you@elsewhere:

Check it, do not assume it:
  sudo $0 --verify $archive
EOF
