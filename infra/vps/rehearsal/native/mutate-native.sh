#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Watch the native rehearsal's checks fail. Breaks one layer on purpose, runs the probe against it,
# and exits 0 only if every check that guards that layer came back red. Anything else that went red
# is listed as collateral, not hidden. Same contract and same shape as rehearsal/mutate.sh, which
# does this for the Docker path's edge.
#
#   mutate-native.sh harden    tighten the sandbox the way a well-meaning person would
#   mutate-native.sh sandbox   loosen it: remove the directives that protect the host
#   mutate-native.sh oom       drop OOMPolicy=continue
#   mutate-native.sh all       all three
#
# `harden` is the one that matters most and the reason this file exists. Every other mutation
# breaks something that would be caught the first time anyone looked. A sandbox tightened past what
# agents need breaks NOTHING VISIBLE -- wheeld starts, the board serves, `systemd-analyze security`
# scores better -- and the damage shows up days later as agents that cannot build, reported as
# "the model seems worse". The mutations below are literally the four directives a security review
# would suggest adding, and each one must turn a specific agent capability red.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
vps="$(cd "$here/../.." && pwd)"
unit="$vps/systemd/wheeld.service"
dropin="$vps/systemd/wheeld.service.d/10-resources.conf"

layer="${1:-}"
[ -n "$layer" ] || { sed -n '6,24p' "$0"; exit 2; }

backup="$(mktemp -d)"
cp "$unit" "$backup/wheeld.service"
cp "$dropin" "$backup/10-resources.conf"
restore() {
    cp "$backup/wheeld.service" "$unit"
    cp "$backup/10-resources.conf" "$dropin"
    rm -rf "$backup"
}
trap restore EXIT

# Every edit must apply exactly once, so a mutation cannot silently miss the line it meant to break
# and then report a green run as proof of anything.
# Matches a WHOLE LINE, never a substring: these unit files name their own directives in their
# commentary (ProtectSystem=strict is discussed a few lines above where it is set), so a substring
# match would be ambiguous exactly where the mutation matters most.
edit() { # edit <file> <old-line> <new-lines>
    python3 "$here/mutate-edit.py" "$1" "$2" "$3"
}

overall=0

run_probe() { # run_probe <expected-red...>  -- runs harden-probe and checks the named checks failed
    local out rc
    out="$("$here/harden-probe.sh" 2>&1)"
    rc=$?
    local missed=0
    for want in "$@"; do
        if printf '%s' "$out" | grep -qE "^  (FAIL|SKIP) +$want"; then
            echo "  red as it must be    $want"
        else
            echo "  STILL NOT RED        $want"
            missed=1
        fi
    done
    printf '%s\n' "$out" | grep -E '^  FAIL' | while read -r line; do
        for want in "$@"; do
            case "$line" in *"$want"*) continue 2 ;; esac
        done
        echo "  collateral           ${line#  FAIL }"
    done
    echo "  (probe rc=$rc)"
    return $missed
}

do_harden() {
    echo "=== mutation 'harden': the four directives a security review would suggest adding ==="
    # Each of these is a real, defensible-sounding suggestion, and each one breaks a specific thing
    # an agent does. docs/proposals/wheeld-native-production.md §4 records why each was rejected.
    edit "$unit" "ProcSubset=all" "ProcSubset=pid" ||
        { echo "  mutation did not apply" >&2; return 1; }
    edit "$unit" "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK" \
                 "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6" || return 1
    edit "$unit" "SystemCallArchitectures=native" \
                 "SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallErrorNumber=EPERM
RestrictNamespaces=yes" || return 1
    # Each tightening must turn its OWN check red, and the mapping is the measured one, not the
    # folklore one: ProcSubset=pid hides /proc/meminfo and /proc/cpuinfo; dropping AF_NETLINK breaks
    # `ss` and `ip` (NOT DNS -- that was measured and is false); SystemCallFilter and
    # RestrictNamespaces break the namespaced mount that bwrap and Chromium need.
    run_probe "ProcSubset=all: meminfo/cpuinfo" \
              "AF_NETLINK: ss/ip" \
              "namespaces: unshare + mount"
}

do_sandbox() {
    echo "=== mutation 'sandbox': remove the directives that protect the host from agents ==="
    edit "$unit" "ProtectHome=yes" "ProtectHome=no" || return 1
    edit "$unit" "ProtectSystem=strict" "ProtectSystem=no" || return 1
    edit "$unit" "ProtectProc=invisible" "ProtectProc=default" || return 1
    run_probe "ProtectSystem=strict" "ProtectHome=yes" "ProtectProc=invisible"
}

do_oom() {
    echo "=== mutation 'oom': drop OOMPolicy=continue, so one agent takes down the daemon ==="
    edit "$dropin" "OOMPolicy=continue" "# OOMPolicy removed by mutate-native.sh" || return 1
    if "$here/oom-containment.sh" >/dev/null 2>&1; then
        echo "  STILL NOT RED        oom-containment passed with OOMPolicy removed from the drop-in"
        return 1
    fi
    echo "  red as it must be    oom-containment"
    return 0
}

case "$layer" in
    harden)  do_harden  || overall=1 ;;
    sandbox) do_sandbox || overall=1 ;;
    oom)     do_oom     || overall=1 ;;
    all)
        do_harden  || overall=1; restore; cp "$unit" "$backup/wheeld.service"; cp "$dropin" "$backup/10-resources.conf"; echo
        do_sandbox || overall=1; restore; cp "$unit" "$backup/wheeld.service"; cp "$dropin" "$backup/10-resources.conf"; echo
        do_oom     || overall=1
        ;;
    *) echo "mutate-native: say which layer to break: harden, sandbox, oom, all" >&2; exit 2 ;;
esac

echo
if [ "$overall" = 0 ]; then
    echo "mutate-native '$layer': every check that guards this layer came back red, as it must"
else
    echo "mutate-native '$layer': a check that should have caught this mutation did not" >&2
fi
exit "$overall"
