#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Does a runaway agent kill the daemon, or does the daemon outlive the agent?
#
# This is the property every other resource limit depends on, and it is one line in
# infra/vps/systemd/wheeld.service.d/10-resources.conf that nobody would think to write.
#
# systemd's default is DefaultOOMPolicy=stop: when ANY process in a unit's cgroup is OOM-killed,
# systemd stops THE UNIT. So the moment you give wheeld.service a MemoryMax -- which you must, or a
# runaway agent takes the whole box and sshd with it -- you have also armed a mechanism where one
# agent's memory bug stops wheeld and every other project's agents. The limit that was supposed to
# contain one agent instead escalates it.
#
# This measures both policies against the same workload, so the difference is observed rather than
# argued: a small long-lived parent (wheeld's shape) spawns a memory hog (an agent's shape) inside a
# cgroup too small for it.
#
# Exit 0 only if BOTH are true: the default policy stops the unit, and `continue` does not. The
# first half matters as much as the second — if the default did not stop the unit, this file would
# be asserting a fix for a problem that does not exist, which is its own kind of lie.
set -uo pipefail

vps="$(cd "$(dirname "$0")/../.." && pwd)"
dropin="$vps/systemd/wheeld.service.d/10-resources.conf"
work="$(mktemp -d)"
trap 'rm -rf "$work"; systemctl reset-failed oom-probe >/dev/null 2>&1; rm -f /etc/systemd/system/oom-probe.service; systemctl daemon-reload' EXIT

# No language runtime, on purpose: a test bed should not need python installed to prove a cgroup
# limit. bash allocates 1 MiB per iteration into a distinct variable, so the pages are touched and
# cannot be reclaimed.
cat > "$work/hog" <<'HOG'
#!/bin/bash
i=0
while :; do
    printf -v "b$i" "%*s" 1048576 ""
    i=$((i + 1))
done
HOG
cat > "$work/parent" <<'PARENT'
#!/bin/bash
# wheeld's shape: small, long-lived, and the thing that must survive.
"$(dirname "$0")/hog" &
while :; do sleep 1; done
PARENT
chmod +x "$work/hog" "$work/parent"

run_case() { # run_case <policy-line> -> prints "ActiveState Result oom_kill"
    {
        echo "[Service]"
        echo "Type=simple"
        echo "ExecStart=$work/parent"
        echo "MemoryMax=128M"
        echo "MemorySwapMax=0"
        # Restart would mask the very thing being measured: a unit that systemd stopped and then
        # restarted looks active again a second later.
        echo "Restart=no"
        [ -z "$1" ] || echo "$1"
    } > /etc/systemd/system/oom-probe.service
    systemctl daemon-reload
    systemctl reset-failed oom-probe >/dev/null 2>&1
    systemctl start oom-probe >/dev/null 2>&1
    local waited=0 killed=0
    while [ "$waited" -lt 30 ]; do
        if [ "$(awk '/^oom_kill /{print $2}' "/sys/fs/cgroup/system.slice/oom-probe.service/memory.events" 2>/dev/null || echo 0)" -gt 0 ]; then
            killed=1
            break
        fi
        # A cgroup that has disappeared means systemd already tore the unit down, which is itself
        # the `stop` outcome — stop waiting for a counter that no longer exists.
        [ -d /sys/fs/cgroup/system.slice/oom-probe.service ] || break
        waited=$((waited + 1))
        sleep 1
    done
    sleep 2
    printf '%s %s %s' "$(systemctl show -P ActiveState oom-probe)" "$(systemctl show -P Result oom-probe)" "$killed"
    systemctl stop oom-probe >/dev/null 2>&1
    systemctl reset-failed oom-probe >/dev/null 2>&1
}

echo "oom-containment: a runaway child inside a 128M cgroup, with and without OOMPolicy=continue"
echo "  systemd default: $(systemctl show --property=DefaultOOMPolicy | cut -d= -f2)"
echo

fail=0

read -r active result killed <<<"$(run_case "")"
printf '  %-28s ActiveState=%-8s Result=%-10s child-oom-killed=%s\n' "systemd default" "$active" "$result" "$killed"
if [ "$active" = failed ] || [ "$result" = oom-kill ]; then
    echo "     as documented: the CHILD hit the limit and systemd stopped THE UNIT. Natively that is"
    echo "     wheeld and every other project's agents, taken down by one agent's memory bug."
else
    echo "     UNEXPECTED: the default policy did not stop the unit here, so this systemd does not"
    echo "     behave the way 10-resources.conf says it does. Do not ship OOMPolicy=continue as a"
    echo "     fix for a problem that was not reproduced — investigate first." >&2
    fail=1
fi

read -r active result killed <<<"$(run_case "OOMPolicy=continue")"
printf '  %-28s ActiveState=%-8s Result=%-10s child-oom-killed=%s\n' "OOMPolicy=continue" "$active" "$result" "$killed"
if [ "$active" = active ] && [ "$killed" = 1 ]; then
    echo "     the child was OOM-killed and the parent kept running: the engine outlives the agent."
else
    echo "     FAILED: with OOMPolicy=continue the parent did not survive its child being killed" >&2
    fail=1
fi

echo
if grep -qE '^\s*OOMPolicy=continue\s*$' "$dropin"; then
    echo "  and the shipped drop-in sets it: $(grep -n 'OOMPolicy' "$dropin" | tail -1)"
else
    echo "  FAILED: $dropin does not set OOMPolicy=continue, so none of the above protects wheeld" >&2
    fail=1
fi

[ "$fail" = 0 ]
