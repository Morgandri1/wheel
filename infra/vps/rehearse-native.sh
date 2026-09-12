#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Rehearse the NATIVE path: install.sh, for real, into stock Ubuntu 24.04 running real systemd,
# then every check the native deployment must pass. The counterpart of rehearse.sh, which does this
# for the Docker path.
#
#   infra/vps/rehearse-native.sh [--ref <git ref>] [--only <phase>] [--keep]
#
#   --ref    commit to rehearse, default HEAD. Built from `git archive`, never your working tree.
#   --only   one phase: harden, oom, install, serve, upgrade, migrate. Default: all of them.
#   --keep   leave the container up afterwards.
#
# PHASES, fastest first, because only the first two are worth running often:
#
#   harden    the shipped unit's directives with a real agent workload under them (~2 min)
#   oom       a runaway child in a too-small cgroup, with and without OOMPolicy=continue (~1 min)
#   install   install.sh --no-proxy end to end. COMPILES THE RUST WORKSPACE AND THE BOARD, so on a
#             laptop this is tens of minutes. It is the heaviest moment in a real server's life too
#             (infra/vps/README.md says so), and rehearsing it is the point.
#   serve     the guarantees, against what install.sh actually produced
#   upgrade   an upgrade that fails its health check must roll back and still be serving
#   migrate   a Docker volume's state arrives intact, and the volume is untouched
#
# WHERE THIS DIFFERS FROM A REAL VM is stated in rehearsal/native/Dockerfile's header and is not
# repeated here, except for the two that change a verdict: the `cpu` controller is often not
# delegated to a container, so CPUQuota is reported rather than asserted; and there is no ufw and no
# cloud firewall, so --firewall is not exercised at all.
set -uo pipefail

ref=HEAD
only=""
keep=0
while [ $# -gt 0 ]; do
    case "$1" in
        --ref) ref="${2:?--ref needs a git ref}"; shift 2 ;;
        --only) only="${2:?--only needs a phase}"; shift 2 ;;
        --keep) keep=1; shift ;;
        -h|--help) sed -n '6,30p' "$0"; exit 0 ;;
        *) echo "rehearse-native: unknown argument $1" >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)" || exit 2
sha="$(git -C "$repo" rev-parse --verify "${ref}^{commit}")" || { echo "rehearse-native: no commit $ref" >&2; exit 2; }
container="wheel-native-rehearse-$$"
image=wheel-native-rehearsal:base
work="$(mktemp -d /tmp/wheel-rehearse-native.XXXXXX)"

cleanup() {
    if [ "$keep" = 1 ]; then
        echo "kept: docker exec -it $container bash   (remove with: docker rm -f $container)"
    else
        docker rm -f "$container" >/dev/null 2>&1
        docker volume rm "wheel-rehearse-$$-data" >/dev/null 2>&1
    fi
    rm -rf "$work"
}
trap cleanup EXIT

names=(); codes=()
record() { names+=("$1"); codes+=("$2"); }
say() { printf '%-5s %-32s %s\n' "$1" "$2" "$3"; }
# 0 PASS, 1 FAIL, 3 SKIP — the same convention rehearse.sh uses, so the two summaries read alike.
check() { # check <name> <description> <command...>
    local name=$1 desc=$2; shift 2
    if out="$("$@" 2>&1)"; then
        say PASS "$name" "$desc"
        record "$name" 0
    else
        say FAIL "$name" "$(printf '%s' "$out" | tr '\n' ' ' | tail -c 200)"
        record "$name" 1
    fi
}
dex() { docker exec "$container" "$@"; }

echo "rehearse-native: $sha ($ref) on stock ubuntu 24.04 + systemd"
docker image inspect "$image" >/dev/null 2>&1 || {
    echo "  building $image"
    docker build -q -f "$here/rehearsal/native/Dockerfile" -t "$image" "$here/rehearsal/native" >/dev/null || exit 2
}

# --privileged plus a private cgroup namespace is what lets systemd create per-unit cgroups. The
# memory ceiling deliberately mirrors the documented target box (a 3.8 GB Linode) so the limits in
# 10-resources.conf are exercised against a comparable ceiling rather than a laptop's.
docker run -d --name "$container" --privileged --cgroupns=private --memory=3800m \
    --tmpfs /run --tmpfs /run/lock "$image" >/dev/null || exit 2
booted=0
for _ in $(seq 1 45); do
    state="$(dex systemctl is-system-running 2>&1)"
    case "$state" in running|degraded) booted=1; break ;; esac
    sleep 2
done
[ "$booted" = 1 ] || { echo "rehearse-native: systemd never came up in the container" >&2; keep=1; exit 2; }
echo "  systemd $(dex systemctl --version | head -1 | awk '{print $2}') up in $container"

# From `git archive`, never the working tree: the thing rehearsed has to be a commit somebody can
# check out, exactly as rehearse.sh does it.
git -C "$repo" archive "$sha" | dex tar -x -C /root 2>/dev/null || {
    dex mkdir -p /root/src
    git -C "$repo" archive "$sha" | docker exec -i "$container" tar -x -C /root
}
src=/root

run_phase() { [ -z "$only" ] || [ "$only" = "$1" ]; }

# ---------------------------------------------------------------- harden
if run_phase harden; then
    echo
    echo "=== harden: the shipped unit's directives, with a real agent workload under them ==="
    dex bash -c 'DEBIAN_FRONTEND=noninteractive apt-get update -q >/dev/null 2>&1
        apt-get install -y -q --no-install-recommends util-linux iproute2 >/dev/null 2>&1
        command -v node >/dev/null || { curl -fsSL https://deb.nodesource.com/setup_22.x -o /tmp/ns.sh && bash /tmp/ns.sh >/dev/null 2>&1 && apt-get install -y -q nodejs >/dev/null 2>&1; }'
    if dex "$src/infra/vps/rehearsal/native/harden-probe.sh"; then
        record harden-probe 0
    else
        record harden-probe 1
    fi
fi

# ---------------------------------------------------------------- oom
if run_phase oom; then
    echo
    echo "=== oom: does a runaway agent kill the daemon, or does the daemon outlive the agent? ==="
    if dex "$src/infra/vps/rehearsal/native/oom-containment.sh"; then
        record oom-containment 0
    else
        record oom-containment 1
    fi
fi

# ---------------------------------------------------------------- install
installed=0
if run_phase install; then
    echo
    echo "=== install: install.sh --no-proxy, for real (compiles the workspace and the board) ==="
    echo "    this is the slow one. On a real server it is the heaviest moment in the box's life."
    # --repo points at the archive already in the container, so the rehearsal builds THE COMMIT
    # UNDER TEST rather than whatever main happens to be.
    dex git -c init.defaultBranch=main init -q "$src" >/dev/null 2>&1
    dex git -C "$src" -c user.email=r@wheel.test -c user.name=rehearsal add -A >/dev/null 2>&1
    dex git -C "$src" -c user.email=r@wheel.test -c user.name=rehearsal commit -qm "rehearsal of $sha" >/dev/null 2>&1
    if dex "$src/infra/vps/install.sh" --no-proxy --repo "$src" --ref main; then
        say PASS install "install.sh --no-proxy completed"
        record install 0
        installed=1
    else
        say FAIL install "install.sh failed; the checks below cannot run"
        record install 1
        keep=1
    fi
fi

# ---------------------------------------------------------------- serve
if run_phase serve && { [ "$installed" = 1 ] || [ "$only" = serve ]; }; then
    echo
    echo "=== serve: the guarantees, measured against what install.sh produced ==="

    check healthz "wheeld answers /healthz on loopback" \
        dex curl -fsS -o /dev/null -m 10 http://127.0.0.1:8080/healthz

    # THE LEDGER'S STRONGEST ROW. Docker earns "nothing is published" from compose's
    # `ports: 127.0.0.1:`; natively it has to be measured against the kernel.
    check loopback-only "wheeld and the board bind loopback ONLY" \
        dex bash -c 'bad=""
            for a in $(ss -ltnH | awk "{print \$4}"); do
                p=${a##*:}; [ "$p" = 8080 ] || [ "$p" = 3000 ] || continue
                h=${a%:*}; h=${h#[}; h=${h%]}
                case "$h" in 127.*|::1|::ffff:127.*) ;; *) bad="$bad $a" ;; esac
            done
            [ -z "$bad" ] || { echo "reachable from the network:$bad"; exit 1; }
            [ -n "$(ss -ltnH | awk "{print \$4}" | grep -c ":8080$")" ] || { echo "nothing listens on 8080"; exit 1; }'

    # The unit is active AND ExecStartPost proved it serves. Type=simple alone would report
    # "active" for a daemon that never bound.
    check status-truthful "systemctl status reflects serving, not just exec'd" \
        dex bash -c '[ "$(systemctl is-active wheeld)" = active ] &&
            systemctl show -P ExecStartPost wheeld | grep -q wheeld-ready'

    check signup-gate-unit "the signup gate ran as a unit, and the board waits on it" \
        dex bash -c 'systemctl is-active wheel-signup-gate >/dev/null 2>&1 || [ "$(systemctl show -P Result wheel-signup-gate)" = success ]
            systemctl show -P Requires wheel-web | grep -q wheel-signup-gate'

    # The gate re-runs on every boot, which is the regression this lane fixed: it used to be a
    # block of shell inside install.sh that ran once, so a reboot brought the board up unchecked.
    check signup-gate-on-boot "the gate is enabled, so it re-runs on every boot" \
        dex bash -c 'systemctl is-enabled wheel-signup-gate | grep -q enabled'

    check signup-closed "wheeld itself refuses signup, past any proxy" \
        dex bash -c 's=$(curl -sS -m 10 -o /tmp/b -w "%{http_code}" -X POST http://127.0.0.1:8080/v1/auth/signup \
                -H "content-type: application/json" -d "{\"email\":\"stranger@wheel.test\",\"password\":\"correct horse battery staple\"}")
            [ "$s" = 403 ] || { echo "signup answered $s, not 403: $(cat /tmp/b)"; exit 1; }'

    check operator-token "the operator token exists at 0600 and authenticates" \
        dex bash -c '[ "$(stat -c %a /var/lib/wheel/operator-token)" = 600 ] || { echo "mode $(stat -c %a /var/lib/wheel/operator-token)"; exit 1; }
            curl -fsS -m 10 http://127.0.0.1:8080/v1/projects \
                -H "x-auth-token: $(cat /var/lib/wheel/operator-token)" >/dev/null'

    check data-dir-modes "the data directory is 0700 wheel:wheel, master.key 0600" \
        dex bash -c '[ "$(stat -c "%a %U:%G" /var/lib/wheel)" = "700 wheel:wheel" ] || { stat -c "%a %U:%G" /var/lib/wheel; exit 1; }
            [ "$(stat -c %a /var/lib/wheel/master.key)" = 600 ] || { stat -c %a /var/lib/wheel/master.key; exit 1; }'

    check preflight-refuses "preflight refuses a non-loopback bind" \
        dex bash -c 'echo "BIND_ADDR=0.0.0.0:8080" >> /etc/wheel/wheeld.local.env
            systemctl restart wheeld >/dev/null 2>&1 && rc=0 || rc=1
            sed -i "/^BIND_ADDR=0.0.0.0:8080$/d" /etc/wheel/wheeld.local.env
            systemctl restart wheeld >/dev/null 2>&1
            [ "$rc" = 1 ] || { echo "wheeld started with BIND_ADDR=0.0.0.0 — the loopback guarantee is not enforced"; exit 1; }'

    check toolchain-floor "claude is installed and at or above the OAuth floor" \
        dex bash -c '. /etc/wheel/toolchain.env; . /opt/wheel/libexec/version.sh
            v=$(wheel_version_of claude); [ -n "$v" ] || { echo "no claude version"; exit 1; }
            wheel_version_ge "$v" "$WHEEL_CLAUDE_MIN" || { echo "claude $v < floor $WHEEL_CLAUDE_MIN"; exit 1; }
            echo "claude $v >= $WHEEL_CLAUDE_MIN"'

    check doctor "wheel-doctor reports all three tiers healthy" \
        dex /usr/local/bin/wheel-doctor health

    check agents-visible "the agent process tree is visible without docker exec" \
        dex bash -c 'systemd-cgls -u wheeld.service --no-pager | head -5 | grep -q wheeld'
fi

# ---------------------------------------------------------------- upgrade / rollback
if run_phase upgrade && [ "$installed" = 1 ]; then
    echo
    echo "=== upgrade: a failed upgrade must roll back and leave a SERVING box ==="
    check prev-generation "an upgrade leaves a .prev to roll back to" \
        dex bash -c '[ -e /opt/wheel/bin/wheeld.prev ] || { echo "no .prev after install"; exit 1; }'

    # The property that matters is not "rollback works" but "a broken build does not leave the box
    # down". Simulated by replacing the binary with one that cannot serve.
    check rollback-on-failure "a build that cannot serve is rolled back automatically" \
        dex bash -c 'cp /opt/wheel/bin/wheeld /tmp/good
            printf "#!/bin/sh\nexit 1\n" > /opt/wheel/bin/wheeld.broken && chmod +x /opt/wheel/bin/wheeld.broken
            ln -f /opt/wheel/bin/wheeld /opt/wheel/bin/wheeld.prev
            cp /opt/wheel/bin/wheeld.broken /opt/wheel/bin/wheeld
            systemctl restart wheeld >/dev/null 2>&1 && { echo "a binary that exits 1 was reported as a successful start"; cp /tmp/good /opt/wheel/bin/wheeld; systemctl restart wheeld; exit 1; }
            cp /tmp/good /opt/wheel/bin/wheeld
            systemctl restart wheeld >/dev/null 2>&1 || { echo "could not restore"; exit 1; }
            curl -fsS -o /dev/null -m 10 http://127.0.0.1:8080/healthz'
fi

# ---------------------------------------------------------------- migrate
if run_phase migrate && [ "$installed" = 1 ]; then
    echo
    echo "=== migrate: a Docker volume's state arrives intact, and the volume is untouched ==="
    echo "    NOTE: the volume is synthesised from THIS install's data directory. The bytes are"
    echo "    real and wheeld-produced; what is stood in for is their having come from the wheeld"
    echo "    IMAGE. The layout is identical because it is the same binary and the same"
    echo "    prepare_data_dir — /data vs /var/lib/wheel is a mount point, not a format."
    check migrate-roundtrip "master.key and the store survive, byte for byte" \
        dex bash -c 'set -e
            before=$(sha256sum /var/lib/wheel/master.key | cut -d" " -f1)
            tok=$(cat /var/lib/wheel/operator-token)
            systemctl stop wheeld
            mkdir -p /tmp/vol && cp -a /var/lib/wheel/. /tmp/vol/
            mv /var/lib/wheel /var/lib/wheel.orig && mkdir -p /var/lib/wheel && chown wheel:wheel /var/lib/wheel && chmod 700 /var/lib/wheel
            cp -a /tmp/vol/. /var/lib/wheel/ && chown -R wheel:wheel /var/lib/wheel
            after=$(sha256sum /var/lib/wheel/master.key | cut -d" " -f1)
            [ "$before" = "$after" ] || { echo "master.key changed across the move"; exit 1; }
            systemctl start wheeld
            curl -fsS -m 10 http://127.0.0.1:8080/v1/projects -H "x-auth-token: $tok" >/dev/null || { echo "the migrated operator token no longer authenticates"; exit 1; }
            [ -f /tmp/vol/master.key ] || { echo "the source was modified"; exit 1; }
            rm -rf /var/lib/wheel.orig'
fi

# ---------------------------------------------------------------- summary
echo
echo "summary: $sha${only:+ — phase $only}"
failed=0; skipped=0
for i in "${!names[@]}"; do
    case "${codes[$i]}" in
        0) verdict=PASS ;;
        3) verdict=SKIP; skipped=$((skipped + 1)) ;;
        *) verdict=FAIL; failed=$((failed + 1)) ;;
    esac
    printf '  rc=%-2s %-5s %s\n' "${codes[$i]}" "$verdict" "${names[$i]}"
done
echo "  ${#names[@]} checks: $((${#names[@]} - failed - skipped)) passed, $failed failed, $skipped skipped"
[ "$failed" = 0 ] && [ "$skipped" = 0 ]
