#!/usr/bin/env bash
#
# The predicates of a deletion tool, one test each. It runs against production data, so "I read it
# and it looked right" is not enough: what must be provable is that the deny list wins, that a
# lookalike domain is not a probe domain, and that nothing young is ever a candidate.
set -uo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=../prune-probe-projects.sh
. ./prune-probe-projects.sh

pass=0
fail=0
KEEP="6906cadb-45cd-4f27-8151-952b9d9bfb15"
PROBE="11111111-2222-3333-4444-555555555555"
DAY=86400

ok() {
    if "$@"; then pass=$((pass + 1)); else fail=$((fail + 1)); echo "  FAIL: expected true: $*"; fi
}
no() {
    if "$@"; then fail=$((fail + 1)); echo "  FAIL: expected false: $*"; else pass=$((pass + 1)); fi
}

echo "deny list"
ok is_denied "$PROBE" "morgan@avo.so"
ok is_denied "$PROBE" "MORGAN@AVO.SO"
ok is_denied "$KEEP" "qa@wheel.test"
no is_denied "$PROBE" "qa@wheel.test"
# The whole point: denied beats every other predicate, however probe-shaped the rest looks.
no is_candidate "$KEEP" "qa@wheel.test" "$((30 * DAY))"
no is_candidate "$PROBE" "morgan@avo.so" "$((30 * DAY))"

echo "probe domains"
ok is_probe_address "api-verify@wheel.test"
ok is_probe_address "QA@WheelCheck.dev"
ok is_probe_address "someone@example.com"
no is_probe_address "someone@wheel.test.attacker.com"
no is_probe_address "someone@notwheel.test"
no is_probe_address "someone@sub.wheel.test"
no is_probe_address "morgan@avo.so"
no is_probe_address ""
no is_probe_address "wheel.test"

echo "age"
ok is_old_enough "$DAY"
ok is_old_enough "$((DAY + 1))"
no is_old_enough "$((DAY - 1))"
no is_old_enough 0
no is_old_enough ""
no is_old_enough "abc"

echo "project id"
ok is_uuid "$PROBE"
no is_uuid "not-a-uuid"
no is_uuid "'; DELETE FROM projects; --"
no is_candidate "'; DELETE FROM projects; --" "qa@wheel.test" "$((30 * DAY))"

echo "candidates"
ok is_candidate "$PROBE" "qa@wheel.test" "$((2 * DAY))"
no is_candidate "$PROBE" "qa@wheel.test" "$((DAY / 2))"
no is_candidate "$PROBE" "" "$((2 * DAY))"

echo "the deny list may not be emptied"
( DENY_OWNERS=""; require_deny_list ) 2>/dev/null && { fail=$((fail + 1)); echo "  FAIL: ran with no denied owners"; } || pass=$((pass + 1))
( DENY_PROJECTS=""; require_deny_list ) 2>/dev/null && { fail=$((fail + 1)); echo "  FAIL: ran with no denied projects"; } || pass=$((pass + 1))

echo "usage"
( main --nonsense >/dev/null 2>&1 ) && { fail=$((fail + 1)); echo "  FAIL: an unknown flag was accepted"; } || pass=$((pass + 1))

echo "the run itself, with the database and the host stubbed"
export DATABASE_URL="stub"
export WHEEL_HOST_URL="stub"
export WHEEL_HOST_SECRET="stub"
DESTROYED="$(mktemp)"
DELETED="$(mktemp)"
fetch_projects() {
    printf '%s|%s|%s\n' "$PROBE" "qa@wheel.test" "$((2 * DAY))"
    printf '%s|%s|%s\n' "$KEEP" "qa@wheel.test" "$((30 * DAY))"
    printf '%s|%s|%s\n' "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee" "morgan@avo.so" "$((30 * DAY))"
    printf '%s|%s|%s\n' "bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee" "qa@wheel.test" "60"
    printf '%s|%s|%s\n' "cccccccc-bbbb-cccc-dddd-eeeeeeeeeeee" "" "$((30 * DAY))"
}
destroy_sandbox() { echo "$1" >> "$DESTROYED"; printf '204'; }
delete_row() { echo "$1" >> "$DELETED"; }

out=$(main)
case "$out" in
    *"5 projects, 1 candidates, 0 deleted"*) pass=$((pass + 1)) ;;
    *) fail=$((fail + 1)); echo "  FAIL: dry run counted wrong: $out" ;;
esac
[ ! -s "$DESTROYED" ] && pass=$((pass + 1)) || { fail=$((fail + 1)); echo "  FAIL: the dry run destroyed something"; }

out=$(main --apply)
case "$out" in
    *"5 projects, 1 candidates, 1 deleted, 0 failed"*) pass=$((pass + 1)) ;;
    *) fail=$((fail + 1)); echo "  FAIL: apply counted wrong: $out" ;;
esac
[ "$(cat "$DESTROYED")" = "$PROBE" ] && pass=$((pass + 1)) || { fail=$((fail + 1)); echo "  FAIL: destroyed $(cat "$DESTROYED")"; }
[ "$(cat "$DELETED")" = "$PROBE" ] && pass=$((pass + 1)) || { fail=$((fail + 1)); echo "  FAIL: deleted $(cat "$DELETED")"; }
rm -f "$DESTROYED" "$DELETED"

echo "a database it cannot read is not an empty database"
fetch_projects() { echo "psql: could not connect" >&2; return 2; }
# `|| rc=$?` rather than `rc=$?` on the next line: the sourced script sets -e, so a failing command
# substitution in a bare assignment would end this test run instead of being measured by it.
rc=0
out=$(main 2>&1) || rc=$?
[ "$rc" -ne 0 ] && pass=$((pass + 1)) || { fail=$((fail + 1)); echo "  FAIL: a failed query exited 0"; }
case "$out" in
    *"refusing to continue"*) pass=$((pass + 1)) ;;
    *) fail=$((fail + 1)); echo "  FAIL: a failed query did not say so: $out" ;;
esac
case "$out" in
    *"0 projects, 0 candidates"*) fail=$((fail + 1)); echo "  FAIL: a failed query reported a clean run" ;;
    *) pass=$((pass + 1)) ;;
esac

echo
echo "prune-probe-projects: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
