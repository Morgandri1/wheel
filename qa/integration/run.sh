#!/usr/bin/env bash
# Integration suite: brings up infra/docker-compose.yml, runs the tests, tears down.
#
# Self-skipping by design: if docker isn't available this exits 77 (SKIP) rather than failing,
# so `make test-int` is safe to run anywhere. CI runs it with WHEEL_REQUIRE_STACK=1, where a
# missing stack IS a failure — a suite that quietly skips in CI tests nothing.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
SKIP=77

PY="$ROOT/qa/.venv/bin/python"
[ -x "$PY" ] || PY=python3

require="${WHEEL_REQUIRE_STACK:-0}"
fail_or_skip() {
  echo "$1"
  [ "$require" = "1" ] && { echo "WHEEL_REQUIRE_STACK=1 — treating as a failure"; exit 1; }
  exit $SKIP
}

command -v docker >/dev/null 2>&1 || fail_or_skip "docker not installed — skipping integration"
docker info >/dev/null 2>&1 || fail_or_skip "docker daemon not running — skipping integration"
"$PY" -c "import pytest" >/dev/null 2>&1 || fail_or_skip "pytest missing — run 'make bootstrap'"

COMPOSE="docker compose -f infra/docker-compose.yml"
export SANDBOX_BACKEND="${SANDBOX_BACKEND:-docker}"
KEEP="${WHEEL_KEEP_STACK:-0}"

cleanup() {
  if [ "$KEEP" = "1" ]; then
    echo "WHEEL_KEEP_STACK=1 — leaving the stack up ($COMPOSE down -v to clean)"
  else
    $COMPOSE down -v >/dev/null 2>&1
  fi
}
trap cleanup EXIT

echo "▸ bringing up the stack (SANDBOX_BACKEND=$SANDBOX_BACKEND)"
if ! $COMPOSE up -d --build >/tmp/wheel-compose.log 2>&1; then
  echo "compose up failed; last 40 lines:"; tail -40 /tmp/wheel-compose.log
  fail_or_skip "could not bring up the stack"
fi

echo "▸ waiting for the API"
ready=0
for _ in $(seq 1 60); do
  if curl -fsS http://localhost:8080/healthz >/dev/null 2>&1; then ready=1; break; fi
  sleep 1
done
if [ "$ready" != "1" ]; then
  echo "API never became healthy; logs:"; $COMPOSE logs --tail=40 api
  fail_or_skip "API did not come up"
fi

mkdir -p qa/.artifacts
echo "▸ running suite"
PYTHONPATH="$ROOT/qa/integration" "$PY" -m pytest qa/integration -v --tb=short -p no:cacheprovider
rc=$?

if [ $rc -ne 0 ]; then
  echo "▸ capturing logs to qa/.artifacts/ for triage"
  $COMPOSE logs --no-color > qa/.artifacts/compose.log 2>&1
fi
exit $rc
