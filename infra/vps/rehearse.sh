#!/usr/bin/env bash
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Rehearse infra/vps/compose.yml on this machine: the whole stack behind Caddy on
# https://$REHEARSE_DOMAIN (default localhost), with a certificate from Caddy's internal CA standing
# in for Let's Encrypt, then every check the VPS deployment must pass. Each check is its own process
# with its own exit code (0 PASS, 1 FAIL, 3 SKIP: a check it builds on failed first). Exits 0 only
# when every check passed.
#
#   infra/vps/rehearse.sh [--ref <git ref>] [--keep]
#
#   --ref   commit to rehearse, default HEAD. Built from `git archive`, never the working tree.
#   --keep  leave the stack up afterwards.
#
#   REHEARSE_DOMAIN=wheel.rehearsal.test   any name; requests connect to 127.0.0.1, no DNS needed
#   REHEARSE_FAKE_HARNESS=1                QA's fake claude, so a message can reach `delivered`
#   REHEARSE_CADDYFILE=<file>              mutation run: replace the Caddyfile
#   REHEARSE_COMPOSE_EXTRA=<file>          mutation run: add a compose layer
#
# Needs docker compose, git, curl and python3, and ports 80 and 443 free on this machine.

set -uo pipefail

ref=HEAD
keep=0
while [ $# -gt 0 ]; do
    case "$1" in
        --ref) ref="${2:?--ref needs a git ref}"; shift 2 ;;
        --keep) keep=1; shift ;;
        -h | --help) sed -n '6,23p' "$0"; exit 0 ;;
        *) echo "rehearse: unknown argument $1" >&2; exit 2 ;;
    esac
done

domain="${REHEARSE_DOMAIN:-localhost}"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)" || exit 2
sha="$(git -C "$repo" rev-parse --verify "${ref}^{commit}")" || { echo "rehearse: no commit $ref" >&2; exit 2; }

work="$(mktemp -d /tmp/wheel-rehearse.XXXXXX)"
src="$work/src"
vps="$src/infra/vps"
project="wheel-rehearse-$$"
octet=$(( ($$ % 200) + 20 ))
mkdir -p "$src"
if ! git -C "$repo" archive "$sha" | tar -x -C "$src"; then
    echo "rehearse: could not archive $sha" >&2
    exit 2
fi
for needed in infra/vps/compose.yml docker/Dockerfile.wheeld docker/Dockerfile.web; do
    if [ ! -f "$src/$needed" ]; then
        echo "rehearse: $sha has no $needed (Dockerfile.wheeld: api/headless-first; Dockerfile.web: web/server-side-api)" >&2
        exit 2
    fi
done

files=(-f "$vps/compose.yml" -f "$vps/rehearsal/compose.rehearsal.yml")
variant=""
if [ "${REHEARSE_FAKE_HARNESS:-}" = 1 ]; then
    files+=(-f "$vps/rehearsal/compose.fake-harness.yml")
    variant="$variant fake-harness"
fi
if [ -n "${REHEARSE_CADDYFILE:-}" ]; then
    cp "$REHEARSE_CADDYFILE" "$vps/Caddyfile" || exit 2
    variant="$variant MUTATION:Caddyfile=$REHEARSE_CADDYFILE"
fi
if [ -n "${REHEARSE_COMPOSE_EXTRA:-}" ]; then
    files+=(-f "$REHEARSE_COMPOSE_EXTRA")
    variant="$variant MUTATION:compose+=$REHEARSE_COMPOSE_EXTRA"
fi

# shellcheck source=/dev/null
. "$vps/lib/derive-env.sh"
(
    umask 077
    cat >"$vps/.env" <<EOF
WHEEL_DOMAIN=$domain
WHEEL_TLS_INTERNAL=1
WHEEL_SIGNUP=closed
WHEEL_EDGE_SUBNET=10.231.$octet.0/24
WHEEL_CADDY_IP=10.231.$octet.10
EOF
    WHEEL_DOMAIN="$domain" WHEEL_CADDY_IP="10.231.$octet.10" wheel_derive_env "$vps/.env"
)

compose() {
    docker compose -p "$project" --project-directory "$vps" --env-file "$vps/.env" "${files[@]}" "$@"
}

teardown() {
    if [ "$keep" = 1 ]; then
        echo "kept: docker compose -p $project --project-directory $vps --env-file $vps/.env ${files[*]} down -v"
        return
    fi
    compose down -v --remove-orphans >/dev/null 2>&1
    docker network rm "$project-outside" >/dev/null 2>&1
    rm -rf "$work"
}
trap teardown EXIT

echo "rehearsal of $sha ($ref) on https://$domain${variant:+ —$variant}"
echo "  compose project $project, edge network 10.231.$octet.0/24, work dir $work"

if ! compose build >"$work/build.log" 2>&1; then
    echo "rehearse: build failed; see $work/build.log" >&2
    keep=1
    exit 2
fi
if ! compose up -d >"$work/up.log" 2>&1; then
    echo "rehearse: compose up failed:" >&2
    tail -20 "$work/up.log" >&2
    keep=1
    exit 2
fi

ca="$work/caddy-root.crt"
ready=0
for _ in $(seq 1 90); do
    if compose cp caddy:/data/caddy/pki/authorities/local/root.crt "$ca" >/dev/null 2>&1 &&
        curl -fsS -o /dev/null --cacert "$ca" --resolve "$domain:443:127.0.0.1" "https://$domain/version.json" 2>/dev/null; then
        ready=1
        break
    fi
    sleep 2
done
if [ "$ready" != 1 ]; then
    echo "rehearse: https://$domain never answered with a certificate from Caddy's CA" >&2
    compose ps >&2
    compose logs --tail 30 >&2
    keep=1
    exit 2
fi

state="$work/state.json"
(umask 077 && printf '{}' >"$state")
export REHEARSE_CA="$ca" REHEARSE_STATE="$state" REHEARSE_DOMAIN="$domain" REHEARSE_FAKE_HARNESS="${REHEARSE_FAKE_HARNESS:-}"

names=()
codes=()
record() {
    names+=("$1")
    codes+=("$2")
}
say() {
    echo "$1  $2 — $3"
}
check() {
    python3 "$vps/rehearsal/checks.py" "$1"
    record "$1" $?
}
remember() {
    KEY="$1" VALUE="$2" python3 -c '
import json, os
p = os.environ["REHEARSE_STATE"]
s = json.load(open(p)); s[os.environ["KEY"]] = os.environ["VALUE"]; json.dump(s, open(p, "w"))'
}
recall() {
    KEY="$1" python3 -c 'import json, os; print(json.load(open(os.environ["REHEARSE_STATE"])).get(os.environ["KEY"], ""))'
}
wheeld_status() {
    compose exec -T wheeld curl -s -o /dev/null -w '%{http_code}' "$@"
}

check web-served
check edge-headers
check signup-closed-at-edge

stranger='{"email":"loopback-stranger@wheel.test","password":"correct horse battery staple"}'
signup_status="$(printf '%s' "$stranger" | wheeld_status -H 'content-type: application/json' --data-binary @- http://127.0.0.1:8080/v1/auth/signup)"
if [ "$signup_status" = 403 ]; then
    say PASS signup-closed-at-wheeld "wheeld itself refuses signup (403 on its own loopback, past Caddy)"
    record signup-closed-at-wheeld 0
else
    say FAIL signup-closed-at-wheeld "loopback POST /v1/auth/signup answered $signup_status"
    record signup-closed-at-wheeld 1
fi

if operator_token="$(compose exec -T wheeld cat /data/operator-token)" && [ -n "$operator_token" ]; then
    remember wht "$(printf '%s' "$operator_token" | tr -d '[:space:]')"
    say PASS operator-token "docker compose exec wheeld cat /data/operator-token gave a token"
    record operator-token 0
else
    say FAIL operator-token "no /data/operator-token in the wheeld container"
    record operator-token 1
fi

check operator-adds-account
check sign-in

guard_foreign="$(wheeld_status -H 'Host: evil.example' http://127.0.0.1:8080/v1/projects)"
guard_web="$(wheeld_status -H 'Host: wheeld:8080' http://127.0.0.1:8080/v1/projects)"
guard_domain="$(wheeld_status -H "Host: $domain" http://127.0.0.1:8080/v1/projects)"
if [ "$guard_foreign" = 403 ] && [ "$guard_web" = 401 ] && [ "$guard_domain" = 401 ]; then
    say PASS host-guard "wheeld refuses Host evil.example (403) and admits wheeld:8080 and $domain (401: on to auth)"
    record host-guard 0
else
    say FAIL host-guard "evil.example=$guard_foreign wheeld:8080=$guard_web $domain=$guard_domain (want 403/401/401)"
    record host-guard 1
fi

check project
check agent-message
check token-auth
check websocket
check web-sse
check ingress
check forwarded-headers-overwritten
check body-limits
check ingress-rate-limit-ignores-xff

# wheeld and web deliberately publish on 127.0.0.1 in every mode (the SSH-tunnel access path), so
# the property this checks is loopback-ONLY, not "no port at all": every published binding must be
# 127.0.0.1, never 0.0.0.0 or a real interface address, and the loopback path must actually work —
# a check that only confirmed a bind exists, without confirming it answers, would prove nothing.
non_loopback=""
for svc in wheeld web; do
    binds="$(docker inspect -f '{{range $p, $b := .NetworkSettings.Ports}}{{range $b}}{{.HostIp}} {{end}}{{end}}' "$(compose ps -q "$svc")")"
    for ip in $binds; do
        case "$ip" in
            127.0.0.1) ;;
            *) non_loopback="$non_loopback $svc binds $ip (not 127.0.0.1);" ;;
        esac
    done
done
pid="$(recall project)"
session="$(recall session)"
cookie="$(recall cookie)"
loopback_answers=1
if [ -n "$pid" ]; then
    for probe in "8080 x-auth-token:$session /v1/projects" "3000 cookie:$cookie /api/wheel/v1/projects"; do
        read -r port header path <<<"$probe"
        body="$(curl -s -m 3 -H "${header%%:*}: ${header#*:}" -H "origin: http://127.0.0.1:$port" "http://127.0.0.1:$port$path")"
        case "$body" in *"$pid"*) ;; *) loopback_answers=0; non_loopback="$non_loopback 127.0.0.1:$port did not answer with this project;" ;; esac
    done
else
    loopback_answers=0
    non_loopback="$non_loopback no project to check against (an earlier check failed);"
fi
if [ -z "$non_loopback" ] && [ "$loopback_answers" = 1 ]; then
    say PASS loopback-only "wheeld and web publish on 127.0.0.1 only, and both answer there (the SSH-tunnel path)"
    record loopback-only 0
else
    say FAIL loopback-only "$non_loopback"
    record loopback-only 1
fi

engine="$(docker info --format '{{.OperatingSystem}}' 2>/dev/null)"
docker network create "$project-outside" >/dev/null
reached=""
for target in "wheeld 8080 /healthz" "web 3000 /version.json"; do
    read -r svc port path <<<"$target"
    ip="$(docker inspect -f "{{with index .NetworkSettings.Networks \"${project}_edge\"}}{{.IPAddress}}{{end}}" "$(compose ps -q "$svc")")"
    if ! compose exec -T caddy wget -q -T 3 -O /dev/null "http://$svc:$port$path"; then
        reached="$reached control failed: caddy cannot reach $svc:$port$path, so the next probe proves nothing;"
    fi
    if docker run --rm --network "$project-outside" --entrypoint wget caddy:2 -q -T 3 -O /dev/null "http://$ip:$port$path" 2>/dev/null; then
        reached="$reached $svc at $ip:$port;"
    fi
    curl -s -m 3 -o /dev/null "http://$ip:$port$path"
    echo "  info: this machine (the docker host) → $svc's edge address $ip:$port: curl rc=$? (0 is expected here even when isolation holds — the docker host itself always routes to its own bridge networks; that is not what 'internal: true' promises)"
done
if [ -z "$reached" ]; then
    say PASS isolated-from-other-networks "a container on another Docker network cannot reach wheeld's or web's edge address (engine: $engine)"
    record isolated-from-other-networks 0
else
    say FAIL isolated-from-other-networks "a container on another Docker network reached$reached (engine: $engine). Measured: stock dockerd (docker:dind) enforces 'internal: true' here (iptables blocks the cross-network hop); OrbStack's engine does not (confirmed 2026-09-11). If \$engine here is a Mac dev engine, this is that gap, not a bug in the Caddyfile or compose.yml — the real VPS runs stock dockerd and this check passes there."
    record isolated-from-other-networks 1
fi

adapted="$(compose exec -T caddy caddy adapt --config /etc/caddy/Caddyfile 2>/dev/null)"
if COOKIE_JSON="$adapted" python3 -c '
import json, os, sys
config = json.loads(os.environ["COOKIE_JSON"])
proxies = []
def walk(node):
    if isinstance(node, dict):
        if node.get("handler") == "reverse_proxy":
            proxies.append(node)
        for value in node.values():
            walk(value)
    elif isinstance(node, list):
        for value in node:
            walk(value)
walk(config)
api = [p for p in proxies if any("8080" in u.get("dial", "") for u in p.get("upstreams", []))]
ok = api and all("Cookie" in (p.get("headers", {}).get("request", {}).get("delete") or []) for p in api)
sys.exit(0 if ok else 1)'; then
    say PASS cookie-stripped-to-wheeld "tripwire, not a gate: the loaded Caddyfile deletes Cookie on every route to wheeld (nothing downstream can observe it: the API ignores cookies and the engine redacts them)"
    record cookie-stripped-to-wheeld 0
else
    say FAIL cookie-stripped-to-wheeld "a route to wheeld forwards the browser's Cookie header"
    record cookie-stripped-to-wheeld 1
fi

curl -s -m 3 -o /dev/null http://127.0.0.1:2019/config/
admin_host=$?
compose exec -T caddy wget -q -T 3 -O /dev/null http://127.0.0.1:2019/config/ 2>/dev/null
admin_inside=$?
compose exec -T caddy wget -q -T 3 -O /dev/null http://web:3000/version.json 2>/dev/null
control=$?
if [ "$admin_host" != 0 ] && [ "$admin_inside" != 0 ] && [ "$control" = 0 ]; then
    say PASS caddy-admin-off "nothing answers on :2019 from this machine (curl rc=$admin_host) or inside the caddy container (wget rc=$admin_inside); control wget rc=0"
    record caddy-admin-off 0
else
    say FAIL caddy-admin-off "host curl rc=$admin_host, in-container wget rc=$admin_inside, control rc=$control"
    record caddy-admin-off 1
fi

echo
echo "summary: $sha on https://$domain${variant:+ —$variant}"
failed=0
skipped=0
for i in "${!names[@]}"; do
    case "${codes[$i]}" in
        0) verdict=PASS ;;
        3) verdict=SKIP; skipped=$((skipped + 1)) ;;
        *) verdict=FAIL; failed=$((failed + 1)) ;;
    esac
    printf '  rc=%-2s %-5s %s\n' "${codes[$i]}" "$verdict" "${names[$i]}"
    [ -z "${REHEARSE_RESULTS:-}" ] || printf '%s %s\n' "${names[$i]}" "${codes[$i]}" >>"$REHEARSE_RESULTS"
done
echo "  ${#names[@]} checks: $((${#names[@]} - failed - skipped)) passed, $failed failed, $skipped skipped"
[ "$failed" = 0 ] && [ "$skipped" = 0 ]
