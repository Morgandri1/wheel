#!/usr/bin/env bash
# Playwright E2E against Web's mock API. Exits 77 when it cannot run.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT/qa/e2e"

command -v pnpm >/dev/null 2>&1 || { echo "pnpm not installed — run 'make bootstrap'"; exit 77; }
[ -f "$ROOT/web/package.json" ] || { echo "web/ not on main yet"; exit 77; }

[ -d node_modules ] || pnpm install --no-frozen-lockfile >/dev/null 2>&1 || {
  echo "could not install Playwright"; exit 77; }
pnpm exec playwright --version >/dev/null 2>&1 || { echo "playwright missing"; exit 77; }
pnpm exec playwright install chromium >/dev/null 2>&1 || {
  echo "could not download the chromium build"; exit 77; }

[ -d "$ROOT/web/node_modules" ] || pnpm -C "$ROOT/web" install --frozen-lockfile >/dev/null 2>&1

exec pnpm exec playwright test "$@"
