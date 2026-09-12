#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Can an agent still do its job under the sandbox we shipped?
#
# This is the check the hardening work exists to survive. Every agent runs inside
# wheeld.service's cgroup and mount namespace, so every directive in that unit is a constraint on
# arbitrary code that clones repositories, installs dependencies and compiles. A sandbox that scores
# well in `systemd-analyze security` and quietly breaks `npm install` has made the product worse
# while looking like it made it safer, and the symptom surfaces days later as "the model seems
# dumber" rather than as an error anyone attributes to a unit file.
#
# IT READS THE SHIPPED UNIT. The [Service] directives under test are lifted out of
# infra/vps/systemd/wheeld.service and its 10-resources.conf drop-in verbatim; only the things that
# would start the actual daemon (ExecStart, ExecStartPre/Post, Type, Restart, KillMode) are dropped.
# So this cannot pass against a copy that has drifted from what install.sh installs -- there is no
# copy.
#
# Run it inside the systemd test bed:
#   docker run -d --name h --privileged --cgroupns=private --tmpfs /run --tmpfs /run/lock \
#       -v "$PWD:/repo:ro" wheel-native-rehearsal:base
#   docker exec h /repo/infra/vps/rehearsal/native/harden-probe.sh
#
# Exit 0 only if every MUST-WORK check passed and every MUST-FAIL check failed. A check that could
# not run (no network, no toolchain) is reported as SKIP and makes the run non-zero, because
# "I could not check" must never read as "the check passed".
set -uo pipefail

# By default a SKIP makes this exit non-zero, because a check that could not run must never read as
# a check that passed. --allow-skips is for the one caller that legitimately causes its own skips:
# rehearse-native.sh --only harden, the fast standalone path, where no Rust toolchain has been
# installed. It never applies to a full rehearsal, where install.sh has put a real one on the box.
allow_skips=0
[ "${1:-}" != "--allow-skips" ] || allow_skips=1

here="$(cd "$(dirname "$0")" && pwd)"
vps="$(cd "$here/../.." && pwd)"
unit="$vps/systemd/wheeld.service"
dropin="$vps/systemd/wheeld.service.d/10-resources.conf"
[ -r "$unit" ] || { echo "harden-probe: no $unit" >&2; exit 2; }

pass=0; fail=0; skip=0
say() { printf '  %-6s %-34s %s\n' "$1" "$2" "${3:-}"; }
good() { pass=$((pass + 1)); say PASS "$1" "${2:-}"; }
bad()  { fail=$((fail + 1)); say FAIL "$1" "${2:-}"; }
meh()  { skip=$((skip + 1)); say SKIP "$1" "${2:-}"; }

# ---------------------------------------------------------------- build the probe unit
#
# Directives that would start the real daemon, or that describe its lifecycle rather than its
# sandbox, are the only ones removed. Everything that constrains what code inside the unit may do
# is kept exactly as shipped -- that is the whole point.
extract() {
    awk '/^\[Service\]/{s=1;next} /^\[/{s=0} s' "$1" |
        grep -vE '^\s*(#|$)' |
        grep -vE '^(ExecStart|ExecStartPre|ExecStartPost|ExecStop|Type|Restart|RestartSec|KillMode|TimeoutStopSec|SyslogIdentifier|EnvironmentFile)='
}
directives="$(extract "$unit")"
[ -r "$dropin" ] && directives="$directives
$(extract "$dropin")"

# `wheel` has to exist for User=wheel, and $HOME must be the data directory: ProtectHome=yes makes
# /home and /root empty, so an agent whose HOME is anywhere else has no writable home at all, and
# npm/cargo/claude all want one. install.sh creates the user this way for exactly this reason.
id wheel >/dev/null 2>&1 || useradd --system --home-dir /var/lib/wheel --no-create-home --shell /usr/sbin/nologin wheel
install -d -m 0700 -o wheel -g wheel /var/lib/wheel

work=/var/lib/wheel/harden-probe
rm -rf "$work"; install -d -m 0700 -o wheel -g wheel "$work"

probe() { # probe <name> <script body>  -> runs it under the shipped directives, returns its rc
    local name=$1 body=$2
    printf '#!/bin/bash\nset -uo pipefail\n%s\n' "$body" > "$work/body.sh"
    chmod 0755 "$work/body.sh"; chown wheel:wheel "$work/body.sh"
    : > "$work/out"; chown wheel:wheel "$work/out"
    {
        echo "[Service]"
        echo "Type=oneshot"
        printf '%s\n' "$directives"
        # Mirrors what install.sh writes into /etc/wheel/wheeld.env. EnvironmentFile= is stripped
        # from the probe unit, so anything the real unit gets from the environment has to be
        # restated here or the probe would test a different process than the one that ships.
        echo "Environment=GIT_TERMINAL_PROMPT=0"
        echo "ExecStart=$work/body.sh"
        echo "StandardOutput=append:$work/out"
        echo "StandardError=append:$work/out"
        # The real unit's start is gated by wheeld actually serving; a oneshot probe just needs
        # enough time for npm to reach a registry on a slow link.
        echo "TimeoutStartSec=600"
    } > /etc/systemd/system/harden-probe.service
    systemctl daemon-reload
    systemctl reset-failed harden-probe >/dev/null 2>&1
    systemctl start harden-probe >/dev/null 2>&1
    # ExecMainStatus alone is 0 for a unit whose START JOB failed -- a directive systemd refused to
    # parse, a unit that would not load at all -- because no main process ever ran to have a status.
    # Every MUST-SUCCEED check below would then read green against a unit file that does not even
    # load, which is the opposite of what this probe is for. So a load/start failure is detected
    # separately and reported as a failure of the check rather than as a passing one.
    local rc result
    result="$(systemctl show -P Result harden-probe 2>/dev/null)"
    rc="$(systemctl show -P ExecMainStatus harden-probe 2>/dev/null)"
    case "$result" in
        success | "") ;;
        exit-code) ;;
        *)
            PROBE_OUT="the unit did not run: Result=$result (a directive systemd refused, or a unit that would not load). $(cat "$work/out" 2>/dev/null)"
            systemctl reset-failed harden-probe >/dev/null 2>&1
            return 1
            ;;
    esac
    systemctl reset-failed harden-probe >/dev/null 2>&1
    PROBE_OUT="$(cat "$work/out" 2>/dev/null)"
    return "${rc:-1}"
}
last() { printf '%s' "$PROBE_OUT" | tr '\n' ' ' | tail -c 130; }

echo "harden-probe: the shipped wheeld.service [Service] directives, with a real workload under them"
echo "  $(printf '%s' "$directives" | grep -c .) directives under test, from $(basename "$unit") + $(basename "$dropin")"
echo

# ---------------------------------------------------------------- the sandbox is actually ON
#
# Every MUST-WORK result below is meaningless if the directives silently failed to apply, so the
# restrictions are proven FIRST. This is the same reasoning as verify-signup-gate.sh's control
# request: establish that the thing under test is really in effect before trusting what it says.
echo "the sandbox is in effect (these MUST fail):"

# THESE DECOYS ARE LOAD-BEARING, and they exist because the first version of this probe was wrong.
#
# It tried to write /usr/local and read /root, and both "passed" -- but they passed on ORDINARY
# UNIX PERMISSIONS, not on the directives. /usr/local is root-owned 0755 and /root is root-owned
# 0700, so the `wheel` user cannot touch either one whether or not ProtectSystem and ProtectHome are
# set at all. mutate-native.sh caught it: removing both directives turned nothing red.
#
# So each decoy is a path the service user WOULD be able to use if the directive were absent: a
# world-writable directory outside ReadWritePaths, and a world-readable file under /home. Now the
# only thing that can stop them is the sandbox, and the ProtectSystem check additionally requires
# the failure reason to be EROFS -- "Read-only file system" -- rather than any refusal at all,
# because a permission error and a read-only mount are different claims.
install -d -m 0777 /opt/wheel-probe-decoy 2>/dev/null || true
install -d -m 0755 /home/wheel-probe-decoy 2>/dev/null || true
echo "operator-secret-stand-in" > /home/wheel-probe-decoy/readable 2>/dev/null || true
chmod 0644 /home/wheel-probe-decoy/readable 2>/dev/null || true
cleanup_decoys() { rm -rf /opt/wheel-probe-decoy /home/wheel-probe-decoy; }
trap cleanup_decoys EXIT

if probe rw-outside 'echo x > /opt/wheel-probe-decoy/written 2>&1'; then
    bad "ProtectSystem=strict" "wrote a world-writable path outside ReadWritePaths — the filesystem is NOT read-only, so nothing below is a test of a sandbox"
elif printf '%s' "$PROBE_OUT" | grep -q 'Read-only file system'; then
    good "ProtectSystem=strict" "a 0777 dir outside ReadWritePaths is EROFS ($(last))"
else
    bad "ProtectSystem=strict" "the write failed, but not with EROFS — so something other than ProtectSystem refused it and this check proves nothing ($(last))"
fi

if probe read-home 'cat /home/wheel-probe-decoy/readable 2>&1'; then
    bad "ProtectHome=yes" "read a world-readable file under /home — an agent can read the operator's keys and shell history"
else
    good "ProtectHome=yes" "a 0644 file under /home is unreachable ($(last))"
fi

# Root's processes. ProtectProc=invisible is what makes sshd, caddy and the board invisible to an
# agent. It does NOT touch redteam 037 (same-uid siblings stay visible) and this probe does not
# claim it does.
if probe read-proc 'cat /proc/1/cmdline >/dev/null 2>&1'; then
    bad "ProtectProc=invisible" "read /proc/1/cmdline as the service user — other users' processes are visible"
else
    good "ProtectProc=invisible" "root's /proc is not readable ($(last))"
fi

# CapabilityBoundingSet= is APPLIED here, and its cost is deliberately NOT measured here.
#
# An earlier version of this probe asserted that `ping` stops working. It does not, and finding out
# why is the reason this comment is long. Two things conspire:
#
#   1. ProtectSystem=strict puts the unit in a mount namespace where /usr is nosuid, so the kernel
#      IGNORES ping's file capability instead of refusing the execve. In a unit with an empty
#      bounding set and NO mount namespace, `ping` really does die with EPERM -- which is what the
#      isolated experiment showed, and why the wrong conclusion was easy to reach.
#   2. Ubuntu 24.04 defaults net.ipv4.ping_group_range to `0 2147483647`, so ping falls back to an
#      unprivileged ICMP datagram socket and needs no capability at all.
#
# And this container cannot measure the real costs either: Docker sets
# net.ipv4.ip_unprivileged_port_start=0, so binding port 80 succeeds here with no capability, which
# it would not on a VM. Measuring a capability restriction inside a --privileged container with
# rewritten sysctls produces confident, wrong answers -- see the Dockerfile header.
#
# So what is checked is the honest thing: that systemd really applied an empty bounding set. What
# it costs is stated in the README from the directive's semantics, not from a measurement this test
# bed is able to make.
# The probe unit has to exist before its resolved properties can be read, so run a no-op through it
# first and then ask systemd what it computed.
probe capcheck 'true' || true
if [ -z "$(systemctl show -P CapabilityBoundingSet harden-probe 2>/dev/null)" ]; then
    good "CapabilityBoundingSet= is applied" "systemd computed an empty bounding set for the unit"
else
    bad "CapabilityBoundingSet= is applied" "systemd computed a NON-empty bounding set ($(systemctl show -P CapabilityBoundingSet harden-probe)) — the directive is missing or overridden"
fi

echo
echo "an agent can still work (these MUST succeed):"

# ---------------------------------------------------------------- fork, exec, process groups
if probe spawn '
    out=$(bash -c "bash -c \"bash -c \\\"echo deep\\\"\"")
    [ "$out" = deep ] || { echo "nested exec produced: $out"; exit 1; }
    setsid sleep 1 & wait $! || true
    echo "nested exec ok; setsid ok; tasks now $(ls /proc/self/task | wc -l)"
'; then good "spawn child processes" "$(last)"; else bad "spawn child processes" "$(last)"; fi

# PrivateDevices=yes gives a minimal private /dev. The plausible breakage is a pty, because plenty
# of CLIs allocate one, so it is measured rather than assumed.
if probe pty '
    for d in /dev/null /dev/zero /dev/urandom /dev/ptmx /dev/tty; do
        [ -e "$d" ] || { echo "missing $d"; exit 1; }
    done
    script -qec "echo pty-works" /dev/null | head -1
'; then good "PrivateDevices: pty + core devices" "$(last)"; else bad "PrivateDevices: pty + core devices" "$(last)"; fi

# ProcSubset=all was a DELIBERATE non-hardening choice. This is the check that would go red if
# someone "tightened" it to `pid`, which hides these files and breaks every build tool that sizes
# its parallelism from them.
if probe procinfo '
    grep -q MemTotal /proc/meminfo || { echo "no /proc/meminfo"; exit 1; }
    grep -qE "^(processor|CPU)" /proc/cpuinfo || { echo "no /proc/cpuinfo"; exit 1; }
    echo "meminfo $(awk "/MemTotal/{print \$2}" /proc/meminfo)kB, $(nproc) cpus, loadavg $(cut -d" " -f1 /proc/loadavg)"
'; then good "ProcSubset=all: meminfo/cpuinfo" "$(last)"; else bad "ProcSubset=all: meminfo/cpuinfo" "$(last)"; fi

if probe curlout '
    curl -fsS -o /dev/null -m 20 https://registry.npmjs.org/ || { echo "curl to the npm registry failed"; exit 1; }
    echo "https egress works without any capability"
'; then good "https egress (curl, no capabilities)" "$(last)"; else bad "https egress (curl, no capabilities)" "$(last)"; fi

if probe dns '
    getent hosts github.com | head -1 || { echo "getent failed"; exit 1; }
    getent ahostsv4 registry.npmjs.org | head -1 || true
'; then good "DNS via getaddrinfo" "$(last)"; else bad "DNS via getaddrinfo" "$(last)"; fi

# WHY AF_NETLINK IS KEPT -- and this is not the reason the proposal originally gave.
#
# The folklore is that dropping AF_NETLINK breaks DNS, because glibc's __check_pf() uses a netlink
# socket inside getaddrinfo. MEASURED, THAT IS FALSE: with AF_NETLINK removed, `getent hosts`,
# node's dns.lookup and `npm install` all still worked -- glibc falls back. The check above would
# not have caught the mutation, and believing the folklore would have left the real cost undetected.
#
# What actually breaks is every netlink CONSUMER: `ss`, `ip`, getifaddrs() and node's
# os.networkInterfaces() ("Cannot open netlink socket: Address family not supported by protocol").
# And `ss` is in this kit's own critical path -- libexec/wheeld-ready uses it to prove the loopback
# promise -- so dropping AF_NETLINK would silently turn the guarantee in proposal §3 into an
# unverifiable claim. That is the reason, and this is the check that holds it.
if probe netlink '
    ss -ltnH >/dev/null 2>&1 || { echo "ss failed -- wheeld-ready cannot prove the loopback bind"; exit 1; }
    ip -br addr >/dev/null 2>&1 || { echo "ip failed"; exit 1; }
    echo "ss and ip both work ($(ss -ltnH 2>/dev/null | wc -l) listening sockets visible)"
'; then good "AF_NETLINK: ss/ip (wheeld-ready needs ss)" "$(last)"; else bad "AF_NETLINK: ss/ip (wheeld-ready needs ss)" "$(last)"; fi

# WHY SystemCallFilter= AND RestrictNamespaces= ARE REFUSED on this unit, as a measurement rather
# than an argument. `unshare -Urm` plus a tmpfs mount is precisely what bwrap, Chromium's sandbox
# and rootless container tooling do, and an agent doing browser QA is a first-class Wheel workload.
#
# Measured: RestrictNamespaces=yes fails this at `unshare` ("Operation not permitted");
# SystemCallFilter=@system-service fails it one step later, at "cannot change root filesystem
# propagation". Note that plain `unshare -Ur` WITHOUT the mount survives the syscall filter, so a
# probe that only ran `unshare` would have missed it -- the mount is the part that matters.
if ! command -v unshare >/dev/null 2>&1; then
    meh "namespaces: unshare + mount (bwrap/Chromium)" "util-linux's unshare is not in this test bed"
elif probe userns '
    unshare -Urm sh -c "mkdir -p /tmp/nsprobe && mount -t tmpfs none /tmp/nsprobe && echo mounted" 2>&1
'; then good "namespaces: unshare + mount (bwrap/Chromium)" "$(last)"; else bad "namespaces: unshare + mount (bwrap/Chromium)" "$(last)"; fi

# ---------------------------------------------------------------- THE ONES THE BRIEF NAMES
if ! command -v git >/dev/null 2>&1; then
    meh "git clone" "git is not installed in this test bed"
elif probe clone '
    cd "$HOME/harden-probe" || exit 1
    # GITHUB'"'"'S OWN CANONICAL TEST REPOSITORY, so a red result here means the sandbox, not a repo
    # that moved. An earlier version of this probe pointed at a package repo that no longer exists,
    # and git answered "could not read Username for https://github.com: No such device or address"
    # -- which is what a MISSING REPO looks like to a service with no tty, and reads exactly like a
    # sandbox failure. That confusion is the reason GIT_TERMINAL_PROMPT=0 is now in wheeld.env.
    git clone --quiet --depth 1 https://github.com/octocat/Hello-World.git repo 2>&1 | tail -2
    [ -d repo/.git ] || { echo "no clone"; exit 1; }
    cd repo && git log --oneline -1
'; then good "git clone (repo-backed workspace)" "$(last)"; else bad "git clone (repo-backed workspace)" "$(last)"; fi

# The failure mode the fixture above uncovered, pinned as its own check: an agent cloning a private
# or nonexistent repo must get a CLEAR refusal, not a message about a missing device. There is no
# tty under systemd, so git's credential helper has nothing to prompt on.
if ! command -v git >/dev/null 2>&1; then
    meh "git fails clearly on a private repo" "git is not installed in this test bed"
elif probe clonepriv '
    cd "$HOME/harden-probe" || exit 1
    out=$(git clone --quiet --depth 1 https://github.com/octocat/this-repo-does-not-exist-wheel.git nope 2>&1)
    case "$out" in
        *"terminal prompts disabled"*) echo "clear: $out"; exit 0 ;;
        *) echo "UNCLEAR: $out"; exit 1 ;;
    esac
'; then good "git fails clearly on a private repo" "$(last)"; else bad "git fails clearly on a private repo" "$(last)"; fi

if ! command -v npm >/dev/null 2>&1; then
    meh "npm install" "node/npm is not installed in this test bed"
elif probe npm '
    cd "$HOME/harden-probe" || exit 1
    mkdir -p pkg && cd pkg
    npm init -y >/dev/null 2>&1
    # A dependency with a native build step, so this exercises node-gyp-shaped work -- compiling,
    # spawning, writing -- and not just a tarball download.
    npm install --no-audit --no-fund --loglevel=error esbuild 2>&1 | tail -3
    [ -d node_modules/esbuild ] || { echo "esbuild not installed"; exit 1; }
    echo "installed $(node -e "console.log(require(\"./node_modules/esbuild/package.json\").version)")"
'; then good "npm install (with a postinstall)" "$(last)"; else bad "npm install (with a postinstall)" "$(last)"; fi

if ! command -v npm >/dev/null 2>&1; then
    meh "run a build" "node/npm is not installed in this test bed"
elif probe build '
    cd "$HOME/harden-probe/pkg" || exit 1
    printf "export const n: number = 41 + 1;\nconsole.log(n);\n" > in.ts
    ./node_modules/.bin/esbuild in.ts --bundle --outfile=out.js --log-level=error || exit 1
    node out.js
'; then good "run a build (esbuild)" "$(last)"; else bad "run a build (esbuild)" "$(last)"; fi

# Optional tier: the heaviest thing an agent does on this box is compile Rust (a wheel-on-wheel
# board does exactly that). Skipped loudly rather than silently when no toolchain is present.
if ! command -v cargo >/dev/null 2>&1; then
    meh "cargo build" "no Rust toolchain in this test bed (rehearse-native.sh installs one)"
elif probe cargo '
    cd "$HOME/harden-probe" || exit 1
    export CARGO_HOME="$HOME/harden-probe/.cargo"
    cargo new --quiet --bin crate 2>&1 | tail -2
    cd crate && cargo build --quiet --offline 2>&1 | tail -3
    ./target/debug/crate
'; then good "cargo build" "$(last)"; else bad "cargo build" "$(last)"; fi

echo
echo "resource limits are applied (not just written):"
for prop in MemoryMax MemoryHigh TasksMax OOMPolicy; do
    probe noop 'true' || true
    printf '  %-6s %-34s %s\n' INFO "$prop" "$(systemctl show -P "$prop" harden-probe 2>/dev/null || echo unknown)"
done
# CPUQuota needs the `cpu` controller, which a container does not always get delegated. Reported
# honestly rather than asserted: see the Dockerfile header for where this test bed differs from a VM.
if grep -qw cpu /sys/fs/cgroup/system.slice/cgroup.controllers 2>/dev/null; then
    printf '  %-6s %-34s %s\n' INFO CPUQuota "cpu controller delegated — CPUQuota=150% is enforceable here"
else
    printf '  %-6s %-34s %s\n' NOTE CPUQuota "the cpu controller is NOT delegated to this container, so CPUQuota cannot be measured here. It applies on a real VM; this test bed cannot prove it."
fi

echo
echo "harden-probe: $pass passed, $fail failed, $skip skipped"
[ "$fail" = 0 ] || exit 1
[ "$skip" = 0 ] || [ "$allow_skips" = 1 ] || {
    echo "harden-probe: $skip check(s) could not run, and a check that could not run is not a check that passed. Install what they need, or pass --allow-skips if you are deliberately running the reduced set." >&2
    exit 1
}
exit 0
