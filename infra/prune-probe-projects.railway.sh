#!/usr/bin/env bash
#
# Run the reviewed prune against the Railway deployment.
#
# Postgres and wheel-host are both private: `postgres.railway.internal` and
# `wheel-host.railway.internal:7100` resolve only inside the project's network, and no container
# there has both a database client and an HTTP client. Adding either to the internet-facing API
# image to make an occasional ops script convenient is a bad trade — it hands anyone who gets
# execution in that container exactly the two tools they would want.
#
# So the reviewed script runs HERE, unchanged, with its deny list and its dry-run default, and only
# its two I/O seams are executed inside the containers that can reach the thing they talk to. The
# secrets stay in those containers: the commands below name $DATABASE_URL and $WHEEL_HOST_SECRET,
# they never carry their values.
#
#   ./infra/prune-probe-projects.railway.sh            # list candidates
#   ./infra/prune-probe-projects.railway.sh --apply    # delete them
set -euo pipefail

cd "$(dirname "$0")"
# shellcheck source=prune-probe-projects.sh
. ./prune-probe-projects.sh

# Not used by the seams below — the credentials live in the containers — but the reviewed script
# requires it to be set, and that check is worth keeping rather than working around.
export DATABASE_URL="${DATABASE_URL:-railway}"
export WHEEL_HOST_URL="${WHEEL_HOST_URL:-railway}"
export WHEEL_HOST_SECRET="${WHEEL_HOST_SECRET:-railway}"

rw() {
    local service=$1
    shift
    railway ssh --service "$service" "$@" 2>/dev/null | tr -d '\r'
}

fetch_projects() {
    rw Postgres "psql \"\$DATABASE_URL\" -At -F '|' -c \"$QUERY\""
}

destroy_sandbox() {
    rw wheel-host "curl -sS -o /dev/null -w '%{http_code}' -X DELETE \
        -H \"Authorization: Bearer \$WHEEL_HOST_SECRET\" \
        \"\$WHEEL_HOST_URL/host/v1/projects/$1\""
}

delete_row() {
    rw Postgres "psql \"\$DATABASE_URL\" -At -c \"DELETE FROM projects WHERE id = '$1'\""
}

main "$@"
