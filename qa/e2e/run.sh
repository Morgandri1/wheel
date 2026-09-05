#!/usr/bin/env bash
# Playwright E2E. Skips (77) until web/ is on main, then runs on its own.
#
# Targets Web's `pnpm mock` by default: a real HTTP+WS server implementing the §4/§5
# shapes and enforcing the wire matrix server-side, so the illegal-wire test exercises a
# genuine rejection rather than a client-side guess. Point WEB_URL/at a compose stack for
# the full-stack run.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

export WEB_URL="${WEB_URL:-http://localhost:3000}"
MOCK_URL="${MOCK_URL:-http://localhost:8787}"
E2E_DIR="qa/e2e"

if [ ! -f web/package.json ]; then
  echo "web/ is not on main yet — E2E cannot run (Web owns web/)"
  exit 77
fi
if ! command -v pnpm >/dev/null 2>&1; then
  echo "pnpm not installed — run 'make bootstrap'"
  exit 77
fi

if [ ! -d "$E2E_DIR/node_modules" ]; then
  echo "▸ installing @playwright/test"
  (cd "$E2E_DIR" && pnpm install --silent) || { echo "playwright install failed"; exit 77; }
fi
if ! (cd "$E2E_DIR" && pnpm exec playwright --version >/dev/null 2>&1); then
  echo "playwright unavailable"; exit 77
fi
if ! (cd "$E2E_DIR" && pnpm exec playwright install chromium >/dev/null 2>&1); then
  echo "could not install the chromium build — E2E cannot run"
  exit 77
fi

PIDS=()
cleanup() {
  rc=$?
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done
  exit $rc
}
trap cleanup EXIT

wait_for_url() { # wait_for_url <url> <seconds> <label>
  for _ in $(seq 1 "$2"); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "$1" 2>/dev/null || true)
    case "$code" in 2*|3*|4*) return 0;; esac
    sleep 1
  done
  echo "$3 never came up at $1"
  return 1
}

# Start Web's mock API unless one is already listening.
if ! curl -sf -o /dev/null "$MOCK_URL" 2>/dev/null; then
  echo "▸ starting web mock ($MOCK_URL)"
  (cd web && pnpm mock >/tmp/wheel-e2e-mock.log 2>&1) & PIDS+=($!)
  wait_for_url "$MOCK_URL" 60 "web mock" || { tail -20 /tmp/wheel-e2e-mock.log; exit 1; }
fi

if ! curl -sf -o /dev/null "$WEB_URL" 2>/dev/null; then
  echo "▸ starting web dev server ($WEB_URL)"
  (cd web && NEXT_PUBLIC_API_URL="$MOCK_URL" NEXT_PUBLIC_AUTH_MODE=mock \
     pnpm dev >/tmp/wheel-e2e-web.log 2>&1) & PIDS+=($!)
  wait_for_url "$WEB_URL" 120 "web dev server" || { tail -20 /tmp/wheel-e2e-web.log; exit 1; }
fi

echo "▸ running playwright against $WEB_URL"
(cd "$E2E_DIR" && pnpm exec playwright test "$@")
