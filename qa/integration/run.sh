#!/usr/bin/env bash
# Integration suite: brings up infra/docker-compose.yml and drives the API.
#
# Parameterised on SANDBOX_BACKEND from day one (docker now, process at M3) so the M3
# re-run is a variable change rather than a rewrite.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

COMPOSE="infra/docker-compose.yml"
export SANDBOX_BACKEND="${SANDBOX_BACKEND:-docker}"
export WHEEL_API_URL="${WHEEL_API_URL:-http://localhost:8080}"
KEEP_UP="${KEEP_UP:-0}"
ARTIFACTS="$ROOT/qa/.artifacts"

PY=python3
[ -x qa/.venv/bin/python ] && PY=qa/.venv/bin/python

if ! docker info >/dev/null 2>&1; then
  echo "docker is not running — integration suite cannot run"
  exit 77
fi
if [ ! -f "$COMPOSE" ]; then
  echo "$COMPOSE not present yet (API owns infra/)"
  exit 77
fi

mkdir -p "$ARTIFACTS"

cleanup() {
  rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "--- capturing logs to qa/.artifacts (suite failed) ---"
    docker compose -f "$COMPOSE" logs --no-color --tail 400 > "$ARTIFACTS/compose.log" 2>&1 || true
  fi
  if [ "$KEEP_UP" != "1" ]; then
    docker compose -f "$COMPOSE" down -v --remove-orphans >/dev/null 2>&1 || true
  else
    echo "KEEP_UP=1 — stack left running at $WHEEL_API_URL"
  fi
  exit $rc
}
trap cleanup EXIT

echo "▸ building the stub engine image"
docker build -q -t wheel-engine:stub -f infra/dev/Dockerfile.engine.stub . >/dev/null || {
  echo "stub engine image build failed"; exit 1; }

echo "▸ bringing up the stack (SANDBOX_BACKEND=$SANDBOX_BACKEND)"
if ! docker compose -f "$COMPOSE" up -d --build; then
  echo "compose up failed"
  exit 1
fi

echo "▸ waiting for the API"
for i in $(seq 1 120); do
  code=$(curl -s -o /dev/null -w '%{http_code}' "$WHEEL_API_URL/healthz" 2>/dev/null || true)
  [ "$code" = "200" ] && break
  sleep 1
done
if [ "${code:-}" != "200" ]; then
  echo "API never became healthy (last /healthz -> ${code:-none})"
  exit 1
fi

rc=0
for suite in qa/integration/test_*.py; do
  [ -e "$suite" ] || continue
  echo
  echo "▸ $(basename "$suite")"
  "$PY" "$suite" || rc=1
done

exit $rc
