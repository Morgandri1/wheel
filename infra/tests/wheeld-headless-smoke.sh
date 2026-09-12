#!/usr/bin/env bash

# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

# Boot smoke for headless wheeld (docs/proposals/headless-first.md), using the README's own
# commands:
#
#   fresh data dir -> operator-token exists, mode 0600 -> GET /v1/projects 200 with it
#   -> the README quickstart (project, board/apply, vault PUT, start, send, reply in the log)
#   -> `wheeld token revoke` -> the same request is 401 -> SIGTERM exits 0
#
#   infra/tests/wheeld-headless-smoke.sh [path/to/wheeld]
#
# The agent is qa/harness/fake-claude, put first on PATH as `claude`: deterministic, no network,
# and it echoes what it was sent, which is what proves the message reached the agent.
# Exit 0 pass, 1 fail, 77 could not run.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WHEELD="${1:-$ROOT/target/release/wheeld}"
[ -x "$WHEELD" ] || { echo "no wheeld binary at $WHEELD"; exit 77; }
command -v jq >/dev/null || { echo "jq is required (the README quickstart uses it)"; exit 77; }
command -v curl >/dev/null || { echo "curl is required"; exit 77; }

WORK="$(mktemp -d)"
DATA="$WORK/data"
mkdir -p "$WORK/bin"
ln -s "$ROOT/qa/harness/fake-claude" "$WORK/bin/claude"
PORT="$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
BASE="http://127.0.0.1:$PORT"

PATH="$WORK/bin:$PATH" "$WHEELD" --data-dir "$DATA" --bind "127.0.0.1:$PORT" > "$WORK/wheeld.log" 2>&1 &
PID=$!
cleanup() { kill -KILL "$PID" 2>/dev/null; rm -rf "$WORK"; }
trap cleanup EXIT

fail() { echo "FAIL: $*"; echo "--- wheeld log (tail)"; tail -20 "$WORK/wheeld.log"; exit 1; }
ok() { echo "ok   $*"; }

for _ in $(seq 1 600); do
  curl -fsS "$BASE/healthz" >/dev/null 2>&1 && break
  kill -0 "$PID" 2>/dev/null || fail "wheeld exited during boot"
  sleep 0.1
done
curl -fsS "$BASE/healthz" >/dev/null || fail "wheeld never served /healthz"
ok "wheeld serves $BASE/healthz"

TOKEN_FILE="$DATA/operator-token"
[ -f "$TOKEN_FILE" ] || fail "no $TOKEN_FILE"
MODE="$(stat -f %Lp "$TOKEN_FILE" 2>/dev/null || stat -c %a "$TOKEN_FILE")"
[ "$MODE" = "600" ] || fail "operator-token mode is $MODE, want 600"
ok "operator-token written, mode $MODE"
grep -q "$TOKEN_FILE" "$WORK/wheeld.log" || fail "the log does not name the token file"
grep -q "wht_" "$WORK/wheeld.log" && fail "a token value reached the log"
ok "the log names the path and holds no token"

# The README's own helper, verbatim apart from the port.
wh() { local path=$1; shift
       curl -fsS -H @<(printf 'x-auth-token: %s\n' "$(cat "${WHEEL_TOKEN_FILE:-$HOME/.wheel/operator-token}")") \
            -H 'content-type: application/json' "$BASE$path" "$@"; }
export WHEEL_TOKEN_FILE="$TOKEN_FILE"

status() { curl -s -o /dev/null -w '%{http_code}' -H @<(printf 'x-auth-token: %s\n' "$(cat "$TOKEN_FILE")") "$BASE$1"; }
[ "$(status /v1/projects)" = "200" ] || fail "GET /v1/projects with the operator token is not 200"
ok "GET /v1/projects with the operator token: 200"

P=$(wh /v1/projects -d '{"name":"hello"}' | jq -r .id) || fail "create project"
[ -n "$P" ] && [ "$P" != "null" ] || fail "no project id"
ok "project $P"

wh /v1/projects/$P/board/apply -d '{"board": {
  "nodes": [ {"name": "keys",   "type": "vault", "config": {"keys": []}},
             {"name": "worker", "type": "agent", "config": {"harness": "claude", "system_prompt": "Be brief."}} ],
  "wires": [ {"from": "worker", "to": "keys", "type": "read"} ] }}' > "$WORK/apply.json" || fail "board/apply"
[ "$(jq -r .applied "$WORK/apply.json")" = "true" ] || fail "board/apply did not apply: $(cat "$WORK/apply.json")"
ok "board applied"

id() { wh /v1/projects/$P/engine/v1/board | jq -r --arg n "$1" '.nodes[] | select(.name == $n) | .id'; }
ANTHROPIC_API_KEY="sk-ant-smoke-not-a-real-key"
printf '{"value":"%s"}' "$ANTHROPIC_API_KEY" | wh /v1/projects/$P/engine/v1/vault/$(id keys)/ANTHROPIC_API_KEY -X PUT -d @- >/dev/null \
  || fail "vault PUT"
ok "vault key stored"
wh /v1/projects/$P/engine/v1/agents/$(id worker)/start -X POST >/dev/null || fail "agent start"
wh /v1/projects/$P/engine/v1/agents/$(id worker)/send -d '{"body": "Say hello."}' >/dev/null || fail "send"
ok "agent started and messaged"

REPLIED=""
for _ in $(seq 1 300); do
  if wh "/v1/projects/$P/engine/v1/agents/$(id worker)/log?limit=50" | grep -q "Say hello."; then REPLIED=1; break; fi
  sleep 0.2
done
[ -n "$REPLIED" ] || fail "the agent never echoed the message into its log"
ok "the agent received the message (fake-claude echoed it)"

TOKEN_ID=$("$WHEELD" token list --data-dir "$DATA" | awk '$3 == "operator" {print $1}')
[ -n "$TOKEN_ID" ] || fail "wheeld token list does not show the operator token"
"$WHEELD" token revoke "$TOKEN_ID" --data-dir "$DATA" || fail "wheeld token revoke exited $?"
[ "$(status /v1/projects)" = "401" ] || fail "a revoked token is not refused"
ok "revoked operator token: 401"

kill -TERM "$PID"
wait "$PID"; RC=$?
[ "$RC" = "0" ] || fail "wheeld exited $RC on SIGTERM"
ok "SIGTERM: wheeld exited 0"
echo "PASS"
