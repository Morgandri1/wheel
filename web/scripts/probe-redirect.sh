#!/usr/bin/env bash
# Builds a standalone server and probes the signed-out /app redirect the way Caddy presents it.
# A unit test on middleware() cannot see this class of bug: Next's middleware adapter parses the
# Location of a middleware response as an absolute URL (a relative one answered 500), and derives
# req.url from the bind address (which sent browsers to https://localhost:3000).
# Usage: scripts/probe-redirect.sh [--no-build]     Exits non-zero on any wrong answer.
set -uo pipefail
cd "$(dirname "$0")/.."
PORT="${PROBE_PORT:-13917}"; ORIGIN="https://wheel.avo.so"
[ "${1:-}" = "--no-build" ] || WHEEL_STANDALONE=1 WHEEL_AUTH_MODE=local pnpm build >/tmp/probe-build.log 2>&1 || { echo "build failed: /tmp/probe-build.log"; exit 2; }
cd .next/standalone
fail=0; pid=""
stop() { [ -n "$pid" ] && kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; pid=""; }
trap stop EXIT
start() { stop; env HOSTNAME=127.0.0.1 PORT="$PORT" NODE_ENV=production WHEEL_API_URL=http://127.0.0.1:1 WHEEL_AUTH_MODE=local "$@" node server.js >/tmp/probe-server.log 2>&1 & pid=$!; sleep 4; }
probe() { # label path expected-status expected-location [extra curl args...]
  local label=$1 path=$2 want_status=$3 want_loc=$4; shift 4
  local out; out=$(curl -s -D- -o /dev/null "$@" "http://127.0.0.1:$PORT$path" | tr -d '\r')
  local status loc; status=$(sed -n '1s/^HTTP\/[0-9.]* \([0-9]*\).*/\1/p' <<<"$out"); loc=$(grep -i '^location:' <<<"$out" | sed 's/^[Ll]ocation: //')
  if [ "$status" = "$want_status" ] && [ "$loc" = "$want_loc" ]; then echo "ok    $label: $status ${loc:-}"; else echo "FAIL  $label: got $status '${loc:-}', want $want_status '$want_loc'"; fail=1; fi
}
P=(-H Host:wheel.avo.so -H X-Forwarded-Host:wheel.avo.so -H X-Forwarded-Proto:https)
echo "== WHEEL_PUBLIC_ORIGIN + WHEEL_TRUST_PROXY (the wheel.avo.so config)"
start WHEEL_PUBLIC_ORIGIN=$ORIGIN WHEEL_TRUST_PROXY=1
probe "/app"               /app               307 "$ORIGIN/sign-in" "${P[@]}"
probe "/app/<id>"          /app/9b1d-44       307 "$ORIGIN/sign-in?next=%2Fapp%2F9b1d-44" "${P[@]}"
probe "/app/invite/<tok>"  /app/invite/wi_abc 307 "$ORIGIN/sign-in?next=%2Fapp%2Finvite%2Fwi_abc" "${P[@]}"
probe "hostile fwd host"   /app               307 "$ORIGIN/sign-in" -H Host:evil.example -H X-Forwarded-Host:evil.example -H X-Forwarded-Proto:http
echo "== WHEEL_TRUST_PROXY only"
start WHEEL_TRUST_PROXY=1
probe "/app"               /app               307 "$ORIGIN/sign-in" "${P[@]}"
echo "== neither (localhost-only mode)"
start
probe "/app on localhost"  /app               307 "http://localhost:$PORT/sign-in" -H "Host: localhost:$PORT"
grep -q ERR_INVALID_URL /tmp/probe-server.log && { echo "FAIL  server log has ERR_INVALID_URL"; fail=1; }
exit $fail
