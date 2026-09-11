#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Brings up infra/vps/compose.yml (the wheeld single-binary path) from infra/vps/.env. This is the
# entry point for a Docker-based deploy — never run `docker compose` against compose.yml directly,
# it needs settings this script derives (see lib/derive-env.sh).
#
#   infra/vps/deploy.sh [--env-file <path>] [--dry-run]
#   infra/vps/deploy.sh --stop-legacy [--legacy-project <name>] [--dry-run]
#
#   --env-file <path>       Default: infra/vps/.env. Copied to a private working file; never
#                            written to.
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
        -h | --help) sed -n '6,23p' "$0"; exit 0 ;;
        *) die "unknown argument $1 (see --help)" ;;
    esac
done

[ -f "$env_file" ] || die "$env_file does not exist — copy .env.example to .env first"
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
echo "    resolved settings (from $env_file):"
sed 's/^/      /' "$resolved"

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
compose up -d --build
compose ps
