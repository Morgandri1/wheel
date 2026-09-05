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

# SDK's real engine now exists, so the suite runs against wheel-engine:test — the same
# image production uses, with only the harness swapped for the fakes. The stub proved the
# API -> host -> engine chain before that landed and is now a fallback, not the default:
# a suite that keeps testing the stub after the real engine ships is testing nothing.
ENGINE_IMAGE="${ENGINE_IMAGE:-}"
if [ -z "$ENGINE_IMAGE" ]; then
  if docker image inspect wheel-engine:test >/dev/null 2>&1; then
    ENGINE_IMAGE=wheel-engine:test
  else
    echo "wheel-engine:test not built — run 'make engine-image-test'"
    exit 77
  fi
fi
export ENGINE_IMAGE
echo "▸ engine image: $ENGINE_IMAGE ($(docker image inspect "$ENGINE_IMAGE" --format '{{.Created}}' 2>/dev/null))"

# The fake harness is the whole reason the suite is hermetic. If the image ever ships
# without it we would silently be driving the REAL claude, which costs money and is
# non-deterministic — so assert it rather than trust the tag.
if ! docker run --rm --entrypoint sh "$ENGINE_IMAGE" -c 'claude --version 2>/dev/null | grep -qi "claude code"'; then
  echo "$ENGINE_IMAGE has no usable claude on PATH"; exit 1
fi
if [ "$ENGINE_IMAGE" = "wheel-engine:test" ] && \
   ! docker run --rm --entrypoint sh "$ENGINE_IMAGE" -c 'test -x /usr/local/bin/claude'; then
  echo "wheel-engine:test does not shadow claude with the fake harness — refusing to run"
  exit 1
fi

api_healthy() {
  [ "$(curl -s -o /dev/null -w '%{http_code}' "$WHEEL_API_URL/healthz" 2>/dev/null || true)" = "200" ]
}

# infra/ is API's and they run this same compose project while developing. Racing them
# produces "removal of container is already in progress" AFTER a 16-minute image build,
# which is a miserable way to find out. So: reuse a healthy stack, and if we do have to
# bring one up, retry once — a collision is usually a concurrent run mid-flight, not a
# broken compose file.
if api_healthy; then
  echo "▸ reusing the stack already running at $WHEEL_API_URL"
  OWN_STACK=0
else
  OWN_STACK=1
  echo "▸ bringing up the stack (SANDBOX_BACKEND=$SANDBOX_BACKEND)"
  # infra/docker-compose.yml declares the `wheel` network external on purpose, so compose
  # adopts one the host may already have created for per-project engine containers. Nothing
  # creates it on a clean machine, which is why the first CI run of this job died on
  # "network wheel declared as external, but could not be found" while every laptop passed:
  # our laptops had the network left over. Creating it is setup, not product behaviour.
  docker network create wheel >/dev/null 2>&1 || true
  docker compose -f "$COMPOSE" down --remove-orphans >/dev/null 2>&1 || true
  if ! docker compose -f "$COMPOSE" up -d --build; then
    echo "  compose up failed — retrying once in 20s in case another run was mid-flight"
    sleep 20
    docker compose -f "$COMPOSE" down --remove-orphans >/dev/null 2>&1 || true
    if ! docker compose -f "$COMPOSE" up -d --build; then
      echo "compose up failed"
      exit 1
    fi
  fi
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
ran_nothing=""
for suite in qa/integration/test_*.py; do
  [ -e "$suite" ] || continue
  echo
  echo "▸ $(basename "$suite")"

  # Two styles live here: script-style modules with a __main__ guard that print their
  # own summary, and pytest-style modules with fixtures. Running a pytest module as a
  # plain script executes NOTHING, exits 0, and prints nothing — test_api_projects.py
  # had been silently not running for exactly that reason. Dispatch on the guard.
  out="$ARTIFACTS/$(basename "$suite" .py).log"
  if grep -q '__main__' "$suite"; then
    "$PY" "$suite" 2>&1 | tee "$out" || rc=1
  else
    "$PY" -m pytest -q "$suite" 2>&1 | tee "$out" || rc=1
  fi

  # A suite that produces no result line is not a passing suite, it is a suite that did
  # not run. That must never read as green.
  if ! grep -qE "passed|failed|error|no tests ran" "$out"; then
    echo "  !! $(basename "$suite") produced no result line — it did not run"
    ran_nothing="$ran_nothing $(basename "$suite")"
    rc=1
  fi
done

if [ -n "$ran_nothing" ]; then
  echo
  echo "suites that reported nothing:$ran_nothing"
fi

exit $rc
