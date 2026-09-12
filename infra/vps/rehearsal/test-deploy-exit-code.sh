#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# deploy.sh once exited 0 when `docker compose up` failed: `if compose up; then ... fi` with no
# `else` has exit status 0 by itself when the condition is false, so a bare `up_rc=$?` placed AFTER
# the whole if/fi reads the IF STATEMENT's status, not the command it tested — a well-known shell
# trap, and a real regression here once. This needs no real docker daemon and no built images: a
# `docker` shim in PATH answers `compose ... up` with a chosen exit code, instantly, so the test
# runs in well under a second and mutation-checks the exact shape that broke.
#
#   infra/vps/rehearsal/test-deploy-exit-code.sh              both cases; exits 0 only if both pass
#   infra/vps/rehearsal/test-deploy-exit-code.sh --mutate      reverts deploy.sh to the buggy
#                                                               if/fi-with-no-else shape in a COPY,
#                                                               and requires the failing-up case to
#                                                               go from PASS to FAIL against it
set -uo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d /tmp/wheel-deploy-exit-test.XXXXXX)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/bin"
cat >"$work/bin/docker" <<'SHIM'
#!/bin/sh
# Fake docker: answers `compose ... up ...` with $SHIM_UP_EXIT_CODE, `ps` with nothing (so the
# legacy-project warning never fires), and everything else with success.
for a in "$@"; do
    case "$a" in
        up) exit "${SHIM_UP_EXIT_CODE:-0}" ;;
    esac
done
for a in "$@"; do
    case "$a" in
        ps) exit 0 ;;
    esac
done
exit 0
SHIM
chmod +x "$work/bin/docker"

mkdir -p "$work/env"
{
    echo "WHEEL_SIGNUP=closed"
} >"$work/env/.env"
chmod 600 "$work/env/.env"

run_case() {
    local exit_code=$1 deploy_script=$2
    PATH="$work/bin:$PATH" SHIM_UP_EXIT_CODE="$exit_code" \
        bash "$deploy_script" --env-file "$work/env/.env" >"$work/out.log" 2>&1
}

pass=0
fail=0
check() {
    local label=$1 want=$2 got=$3
    if [ "$got" = "$want" ]; then
        echo "PASS  $label — exit $got (wanted $want)"
        pass=$((pass + 1))
    else
        echo "FAIL  $label — exit $got, wanted $want"
        sed 's/^/      /' "$work/out.log" >&2
        fail=$((fail + 1))
    fi
}

run_case 0 "$here/deploy.sh"
check "successful compose up exits 0" 0 $?

run_case 7 "$here/deploy.sh"
check "failed compose up (rc=7) propagates a non-zero exit" 7 $?

if [ "${1:-}" = --mutate ]; then
    # The exact shape that broke: no else, exit status read after the whole construct. Run from
    # its own directory, a copy of $here, not just the one script file: deploy.sh sources
    # lib/derive-env.sh relative to its own path, and a lone copy elsewhere would fail on that
    # instead of exercising the regression this is testing for.
    mutated_dir="$work/mutated-vps"
    cp -a "$here" "$mutated_dir"
    mutated="$mutated_dir/deploy.sh"
    python3 - "$here/deploy.sh" "$mutated" <<'PY' || exit 2
import sys
src, dst = sys.argv[1], sys.argv[2]
text = open(src).read()
old = '''if compose up -d --build; then
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
fi'''
new = '''if compose up -d --build; then
    compose ps
    exit 0
fi
up_rc=$?'''
count = text.count(old)
if count != 1:
    sys.exit(f"mutate: the fixed shape occurs {count} times, expected 1 — deploy.sh has changed shape since this test was written")
open(dst, "w").write(text.replace(old, new))
PY
    run_case 7 "$mutated"
    mutated_rc=$?
    if [ "$mutated_rc" = 0 ]; then
        echo "PASS  mutation: reverting to the buggy if/fi shape reproduces exit 0 on a failed up (mutation-checked)"
        pass=$((pass + 1))
    else
        echo "FAIL  mutation: the buggy shape was expected to exit 0 (reproducing the bug) but exited $mutated_rc — this test may no longer be exercising the regression it claims to"
        fail=$((fail + 1))
    fi
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
