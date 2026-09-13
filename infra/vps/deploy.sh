#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Brings up infra/vps/compose.yml (the wheeld single-binary path) from infra/vps/.env. This is the
# entry point for a DOCKER-based deploy — never run `docker compose` against compose.yml directly,
# it needs settings this script derives (see lib/derive-env.sh).
#
# THE DEFAULT IS NOT THIS SCRIPT. `infra/vps/install.sh` is: systemd services built from source, no
# Docker daemon on the box (README.md §2). This path is fully supported and is the right choice
# when the container boundary between agents and the host matters more to you than what you give up
# for it — resource limits (compose sets none), `ufw` being authoritative over published ports,
# `systemd-cgls` as a way to see what agents are doing, and wheeld's ability to update itself.
# README.md §9 states the trade; §8 states what the native path loses in exchange.
#
# The two paths are independent and use independent directories (/opt/wheel-compose here,
# /opt/wheel there), so trying one does not commit you to it. infra/vps/migrate-from-docker.sh
# moves this path's state to the other one without writing to the volume it reads.
#
#   infra/vps/deploy.sh [--env-file <path>] [--dry-run]
#   infra/vps/deploy.sh --stop-legacy [--legacy-project <name>] [--dry-run]
#
#   --env-file <path>       Default: infra/vps/.env. Its content is copied to a private working
#                            file before anything reads it, but the file ITSELF may be chmod'd to
#                            600 in place if it is looser than that (see below) — not "never
#                            written to". Refused if it is a symlink, rather than following it.
#   --stop-legacy            Stop an older compose deployment (default project name "wheel") if
#                            one is running, with `docker compose -p <name> down` — NEVER `-v`, so
#                            its volumes are never touched. Without this flag, an old deployment
#                            publishing this stack's ports simply makes `docker compose up` fail to
#                            bind them, which is the safe default: nothing here stops another
#                            deployment by surprise.
#   --legacy-project <name>  Default: wheel.
#   --dry-run                Resolve settings and print every command this would run. Runs no
#                            docker command that creates, starts, stops or removes anything.
#
# Non-interactive throughout: every input is a flag, an environment variable, or infra/vps/.env.

set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
env_file="$here/.env"
stop_legacy=0
legacy_project=wheel
dry_run=0

die() {
    echo "deploy: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --env-file) env_file="${2:?--env-file needs a path}"; shift 2 ;;
        --stop-legacy) stop_legacy=1; shift ;;
        --legacy-project) legacy_project="${2:?--legacy-project needs a name}"; shift 2 ;;
        --dry-run) dry_run=1; shift ;;
        -h | --help) sed -n '6,38p' "$0"; exit 0 ;;
        *) die "unknown argument $1 (see --help)" ;;
    esac
done

[ -f "$env_file" ] || die "$env_file does not exist — copy .env.example to .env first"
[ -L "$env_file" ] && die "$env_file is a symlink — refusing to follow it. This script reads it as root and may chmod it; point --env-file at the real file directly."

# .env holds no secret by default (wheeld generates its own inside its volume), but it is exactly
# the kind of file that quietly stops being true, so this checks rather than trusting whoever ran
# `chmod 600` once remembered to. `ls -ld`'s permission string is portable across GNU and BSD stat,
# unlike `stat`'s own format flags.
env_perms="$(ls -ld "$env_file")"
env_perms="${env_perms%% *}"
# The 10 permission characters always come first; some platforms append a decoration after them
# (macOS: `@` for extended attributes, some Linux configurations: `+` for an ACL), so this takes
# a fixed-width prefix rather than trusting the whole field's length to still be 10.
env_perms="${env_perms:0:10}"
if [ "${env_perms#????}" != "------" ]; then
    echo "deploy: $env_file is not 600 (group/other can read or write it: $env_perms) — fixing it" >&2
    [ "$dry_run" = 1 ] || chmod 600 "$env_file"
fi

# shellcheck source=/dev/null
. "$here/lib/derive-env.sh"

work="$(mktemp -d /tmp/wheel-deploy.XXXXXX)"
trap 'rm -rf "$work"' EXIT
resolved="$work/.env"
cp "$env_file" "$resolved"
(
    set -a
    # shellcheck source=/dev/null
    . "$env_file"
    set +a
    wheel_derive_env "$resolved"
)

mode="tunnel mode (nothing published)"
grep -q '^WHEEL_DOMAIN=.' "$resolved" 2>/dev/null && mode="TLS mode ($(grep '^WHEEL_DOMAIN=' "$resolved" | tail -1 | cut -d= -f2-))"
echo "==> $mode"
if [ "$dry_run" = 1 ]; then
    # Full values, because seeing exactly what would be applied is the point of a dry run — and a
    # dry run is not the thing that ends up in a shell's history as "the command that just worked".
    echo "    resolved settings (from $env_file):"
    sed 's/^/      /' "$resolved"
else
    # Names only on a real run: this reaches SSH scrollback and a root shell's history on every
    # invocation, not just when asked to inspect it, and .env is not a place secrets belong (see
    # .env.example) but this should not be where that stops being true.
    echo "    resolved settings (from $env_file): $(grep -o '^[A-Z_]*' "$resolved" | tr '\n' ' ')"
fi

if [ "$stop_legacy" = 1 ]; then
    step_header="==> checking for an existing compose project '$legacy_project'"
    if [ "$dry_run" = 1 ]; then
        echo "$step_header (dry-run: would run 'docker compose -p $legacy_project ps -a --format json')"
    else
        echo "$step_header"
    fi
    containers="$(docker compose -p "$legacy_project" ps -a --format '{{.Name}}\t{{.Image}}\t{{.State}}' 2>/dev/null || true)"
    if [ -z "$containers" ]; then
        echo "    none found; nothing to stop"
    else
        echo "    found:"
        while IFS= read -r line; do echo "      $line"; done <<<"$containers"
        volumes="$(docker volume ls --filter "label=com.docker.compose.project=$legacy_project" --format '{{.Name}}' 2>/dev/null || true)"
        if [ -n "$volumes" ]; then
            echo "    its volumes (left untouched, no -v, ever):"
            while IFS= read -r line; do echo "      $line"; done <<<"$volumes"
        fi
        if [ "$dry_run" = 1 ]; then
            echo "    (dry-run) would run: docker compose -p $legacy_project down"
        else
            echo "==> stopping '$legacy_project' (docker compose down, no -v)"
            docker compose -p "$legacy_project" down
        fi
    fi
elif docker compose -p "$legacy_project" ps -q 2>/dev/null | grep -q .; then
    echo "==> WARNING: a compose project named '$legacy_project' is already running." >&2
    echo "    If it publishes the ports this stack needs, 'docker compose up' below will fail to bind them." >&2
    echo "    Re-run with --stop-legacy to stop it first (its volumes are never touched either way)." >&2
fi

compose() {
    docker compose -p wheel --project-directory "$here" --env-file "$resolved" -f "$here/compose.yml" "$@"
}

if [ "$dry_run" = 1 ]; then
    echo "==> (dry-run) would run: docker compose -p wheel --env-file <resolved> -f compose.yml up -d --build"
    echo
    echo "DRY RUN complete. Nothing was changed."
    exit 0
fi

echo "==> docker compose up -d --build"
if compose up -d --build; then
    compose ps
    exit 0
else
    # $? here is compose's own exit status, because it's read as the FIRST thing in the else
    # branch. A bare `up_rc=$?` placed after the whole if/fi, with no else, is a well-known trap:
    # an if statement that took neither branch (the "then" skipped, no "else" to run) has exit
    # status 0 by itself, so `$?` there reflects the IF STATEMENT, not the command it tested —
    # this script shipped exactly that bug once, silently exiting 0 on a real deploy failure.
    # rehearsal/test-deploy-exit-code.sh mutation-checks this shape specifically.
    up_rc=$?
fi

# depends_on (verify-signup-gate included) gates STARTING a service and never stops one already
# running, so on a fresh deploy a failed dependency already means wheeld/web never started —
# nothing more to do there. On an UPGRADE, though, wheeld and an already-running web stay Up,
# still answering on their loopback ports (tunnel mode's whole access path), so `up` failing is
# only a loud alarm unless something here actually stops them too. This does, whenever `up` fails
# for any reason, not only a verify-signup-gate failure specifically — a conservative default is
# better than trying to attribute the exact cause and getting it wrong. It is still only wheeld
# and web going down, not proof that nothing was reachable in the time before this ran.
echo "==> docker compose up failed (rc=$up_rc). Stopping wheeld and web as a precaution — check 'docker compose -p wheel logs' for why, especially verify-signup-gate." >&2
compose stop wheeld web 2>&1 | sed 's/^/    /' >&2
compose ps
exit "$up_rc"
