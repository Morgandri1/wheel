#!/usr/bin/env bash
# Boot a throwaway wheel-engine:test, build a minimal board, extract node tokens from their 0600
# files, run the consolidated CLI-gated probe, then tear the container down. PM: remove after.
set -u
NAME="wheel-adv-cli-$$"
PORT=7010
SECRET="$(openssl rand -hex 24)"
PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"
ES=(-H "authorization: Bearer ${SECRET}")

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "== boot $NAME (engine role, :$PORT) =="
docker run -d --name "$NAME" -p ${PORT}:7000 \
  -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" -e WHEEL_ENGINE_SECRET="$SECRET" \
  -e WHEEL_LISTEN="tcp://0.0.0.0:7000" -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json \
  wheel-engine:test >/dev/null || { echo "docker run failed"; exit 2; }

for i in $(seq 1 30); do
  code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null || true)
  [ "$code" = "200" ] && break; sleep 0.5
done
[ "$code" = "200" ] || { echo "engine not healthy (code=$code); logs:"; docker logs "$NAME" 2>&1 | tail -20; exit 2; }
echo "healthy."

mk() { curl -s "${ES[@]}" -H 'content-type: application/json' -X POST "$URL/v1/nodes" -d "$1" >/dev/null; }
mk '{"name":"a","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
mk '{"name":"b","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
mk '{"name":"t","type":"table","config":{"columns":[{"name":"v","type":"text"}]}}'

board="$(curl -s "${ES[@]}" "$URL/v1/board")"
aid=$(echo "$board" | python3 -c 'import sys,json;print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="a"))')
bid=$(echo "$board" | python3 -c 'import sys,json;print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="b"))')
tid=$(echo "$board" | python3 -c 'import sys,json;print(next(n["id"] for n in json.load(sys.stdin)["nodes"] if n["name"]=="t"))')
echo "a=$aid b=$bid t=$tid"

wire() { curl -s "${ES[@]}" -H 'content-type: application/json' -X POST "$URL/v1/wires" -d "$1" >/dev/null; }
wire "{\"from\":\"$aid\",\"to\":\"$bid\",\"type\":\"send\"}"
wire "{\"from\":\"$aid\",\"to\":\"$tid\",\"type\":\"read\"}"

curl -s "${ES[@]}" -X POST "$URL/v1/agents/$aid/start" >/dev/null
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$bid/start" >/dev/null
sleep 2

echo "== run dir layout =="; docker exec "$NAME" sh -lc 'ls -la /data/run/ 2>/dev/null; find /data/run -name token 2>/dev/null'
TOKA=$(docker exec "$NAME" sh -lc "cat /data/run/$aid/token 2>/dev/null")
TOKB=$(docker exec "$NAME" sh -lc "cat /data/run/$bid/token 2>/dev/null")
echo "tokA len=${#TOKA} tokB len=${#TOKB}"

echo "== probe =="
WHEEL_ENGINE_URL="$URL" WHEEL_ENGINE_SECRET="$SECRET" \
  WHEEL_TOKEN_A="$TOKA" WHEEL_TOKEN_B="$TOKB" \
  WHEEL_AGENT_B_NAME="b" WHEEL_AGENT_A_ID="$aid" \
  WHEEL_TABLE_R_NAME="t" WHEEL_TABLE_R_ROW="row1" \
  python3 "$(dirname "$0")/t_cli_token_and_forgery.py"
rc=$?
echo "== probe exit=$rc =="
exit $rc
