#!/usr/bin/env bash
#
# Delete the projects that automated probes leave behind on the deployed API.
#
# Every engine a stale project keeps resident costs memory on the single host and slows the next
# person's project creation. This removes them; it is a deletion tool aimed at production, so it
# does nothing until asked twice: dry-run is the default, and a project is a candidate only if it
# survives a deny list checked BEFORE any other predicate.
#
#   ./infra/prune-probe-projects.sh            # list what would be deleted
#   ./infra/prune-probe-projects.sh --apply    # delete it
#
# Requires DATABASE_URL (the API's Postgres), WHEEL_HOST_URL and WHEEL_HOST_SECRET: sandboxes are
# destroyed through the host before their rows are dropped, so nothing is orphaned on the host.
set -euo pipefail

# Never touched, whatever else is true of them. The operator's own account and the board the team
# runs on: the two things whose loss cannot be undone by re-running a test.
DENY_OWNERS="morgan@avo.so"
DENY_PROJECTS="6906cadb-45cd-4f27-8151-952b9d9bfb15"

# An address is a probe account only if its domain is one of these exactly. Not a substring: a real
# user at wheel.test.example.org is not a probe, and neither is one at notwheel.test.
PROBE_DOMAINS="wheel.test wheelcheck.dev example.com"

MIN_AGE_SECONDS=86400

lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

# Denied wins over every other predicate, so it is asked first and separately.
is_denied() {
    local id owner
    id=$(lower "$1")
    owner=$(lower "$2")
    local d
    for d in $DENY_PROJECTS; do [ "$id" = "$(lower "$d")" ] && return 0; done
    for d in $DENY_OWNERS; do [ "$owner" = "$(lower "$d")" ] && return 0; done
    return 1
}

is_probe_address() {
    local email domain
    email=$(lower "$1")
    case "$email" in
        *@*) domain=${email##*@} ;;
        *) return 1 ;;
    esac
    local d
    for d in $PROBE_DOMAINS; do [ "$domain" = "$d" ] && return 0; done
    return 1
}

is_old_enough() {
    case "$1" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "$1" -ge "$MIN_AGE_SECONDS" ]
}

# The only value this script interpolates into SQL, so it is checked rather than trusted: anything
# that is not a plain uuid is not a project we know about, and never a candidate.
is_uuid() {
    case "$(lower "$1")" in
        [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f]-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) return 0 ;;
    esac
    return 1
}

# id, owner, age -> 0 if this project may be deleted.
is_candidate() {
    is_denied "$1" "$2" && return 1
    is_uuid "$1" || return 1
    is_probe_address "$2" || return 1
    is_old_enough "$3" || return 1
    return 0
}

human_age() {
    local s=$1
    printf '%dd%dh' $((s / 86400)) $(((s % 86400) / 3600))
}

require_deny_list() {
    if [ -z "${DENY_OWNERS// /}" ] || [ -z "${DENY_PROJECTS// /}" ]; then
        echo "refusing to run: the deny list is empty" >&2
        return 1
    fi
}

# A project with no users row has an owner we cannot identify, so it can never be a probe account.
# LEFT JOIN and an empty email, rather than an inner join that would hide it from the listing too.
QUERY="SELECT p.id, coalesce(u.email::text, ''), EXTRACT(EPOCH FROM (now() - p.created_at))::bigint
       FROM projects p LEFT JOIN users u ON u.id::text = p.owner_id
       ORDER BY p.created_at"

fetch_projects() {
    psql "$DATABASE_URL" -At -F '|' -c "$QUERY"
}

destroy_sandbox() {
    curl -sS -o /dev/null -w '%{http_code}' -X DELETE \
        -H "Authorization: Bearer $WHEEL_HOST_SECRET" \
        "$WHEEL_HOST_URL/host/v1/projects/$1"
}

delete_row() {
    psql "$DATABASE_URL" -At -c "DELETE FROM projects WHERE id = '$1'"
}

main() {
    local apply=0
    case "${1:-}" in
        --apply) apply=1 ;;
        ''|--dry-run) ;;
        *) echo "usage: $0 [--apply]" >&2; return 2 ;;
    esac

    require_deny_list
    : "${DATABASE_URL:?set DATABASE_URL to the Postgres the API uses}"
    if [ "$apply" -eq 1 ]; then
        : "${WHEEL_HOST_URL:?set WHEEL_HOST_URL}"
        : "${WHEEL_HOST_SECRET:?set WHEEL_HOST_SECRET}"
    fi

    local total=0 candidates=0 deleted=0 failed=0
    local id owner age
    while IFS='|' read -r id owner age; do
        [ -n "$id" ] || continue
        total=$((total + 1))
        if ! is_candidate "$id" "$owner" "$age"; then
            continue
        fi
        candidates=$((candidates + 1))
        printf '%s  %-40s %8s' "$id" "${owner:-<unknown owner>}" "$(human_age "$age")"
        if [ "$apply" -eq 0 ]; then
            printf '  (dry run)\n'
            continue
        fi
        local code
        code=$(destroy_sandbox "$id" || echo 000)
        case "$code" in
            2*) delete_row "$id" >/dev/null; deleted=$((deleted + 1)); printf '  destroyed, row deleted\n' ;;
            *) failed=$((failed + 1)); printf '  host said %s — row kept\n' "$code" ;;
        esac
    done <<EOF
$(fetch_projects)
EOF

    printf '\n%d projects, %d candidates, %d deleted, %d failed%s\n' \
        "$total" "$candidates" "$deleted" "$failed" \
        "$([ "$apply" -eq 0 ] && echo ' (dry run: nothing was deleted)')"
}

# Sourced by infra/tests/prune-probe-projects.test.sh, which drives the predicates directly.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    main "$@"
fi
