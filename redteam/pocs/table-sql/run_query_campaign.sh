#!/usr/bin/env bash
# Boot wheel-engine:dev, build a two-table board, seed a distinctive secret row in the FORBIDDEN table,
# extract agent A's node token, run t_query_authorizer.py, tear down. PM: containers removed (trap).
#   A: agent (claude) wired read->selft. A is NOT wired to othert (authorizer must still deny reads of t_othert).
#   selft, othert: table nodes. Seeded via a temporary write wire, which is then removed.
set -u
NAME="wheel-adv-query-$$"; PORT=7026; SECRET="$(openssl rand -hex 24)"; VK="$(head -c32 /dev/urandom|base64)"; PID="$(uuidgen)"
URL="http://127.0.0.1:${PORT}"; ES=(-H "authorization: Bearer ${SECRET}")
cleanup(){ docker rm -f "$NAME" >/dev/null 2>&1 || true; }; trap cleanup EXIT
echo "== boot $NAME =="
docker run -d --name "$NAME" -p ${PORT}:7000 -e WHEEL_ROLE=engine -e WHEEL_PROJECT_ID="$PID" \
  -e WHEEL_ENGINE_SECRET="$SECRET" -e WHEEL_VAULT_KEY="$VK" -e WHEEL_LISTEN="tcp://0.0.0.0:7000" \
  -e WHEEL_DATA_DIR=/data -e WHEEL_LOG=json wheel-engine:dev >/dev/null || { echo "run failed"; exit 2; }
for i in $(seq 1 40); do code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/healthz" 2>/dev/null||true); [ "$code" = 200 ]&&break; sleep 0.5; done
[ "${code:-}" = 200 ] || { echo "unhealthy ($code)"; docker logs "$NAME" 2>&1|tail -20; exit 2; }
echo healthy.
mk(){ curl -s "${ES[@]}" -H 'content-type: application/json' -X POST "$URL/v1/nodes" -d "$1" >/dev/null; }
mk '{"name":"a","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
mk '{"name":"selft","type":"table","config":{"columns":[{"name":"v","type":"text"}]}}'
mk '{"name":"othert","type":"table","config":{"columns":[{"name":"v","type":"text"}]}}'
board="$(curl -s "${ES[@]}" "$URL/v1/board")"
id(){ echo "$board"|python3 -c "import sys,json;print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$1'))"; }
AID=$(id a); ST=$(id selft); OT=$(id othert)
wire(){ curl -s "${ES[@]}" -H 'content-type: application/json' -X POST "$URL/v1/wires" -d "$1" >/dev/null; }
unwire(){ curl -s "${ES[@]}" -H 'content-type: application/json' -X DELETE "$URL/v1/wires" -d "$1" >/dev/null; }
# temporary write wires to seed rows (write implies read)
wire "{\"from\":\"$AID\",\"to\":\"$ST\",\"type\":\"write\"}"
wire "{\"from\":\"$AID\",\"to\":\"$OT\",\"type\":\"write\"}"
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$AID/start" >/dev/null; sleep 2
TOK_A=$(docker exec "$NAME" sh -lc "cat /data/run/$AID/token 2>/dev/null")
w(){ curl -s -H "authorization: Bearer $TOK_A" -H 'content-type: application/json' -X POST "$URL/v1/cli/write" -d "$1" >/dev/null; }
w "{\"addr\":\"selft/r1\",\"value\":\"{\\\"v\\\":\\\"public-selft-row\\\"}\"}"
w "{\"addr\":\"othert/r1\",\"value\":\"{\\\"v\\\":\\\"SECRET-othert-must-not-leak\\\"}\"}"
# now REMOVE both write wires and re-add only selft READ, so A has read->selft and NOTHING to othert
unwire "{\"from\":\"$AID\",\"to\":\"$ST\",\"type\":\"write\"}"
unwire "{\"from\":\"$AID\",\"to\":\"$OT\",\"type\":\"write\"}"
wire "{\"from\":\"$AID\",\"to\":\"$ST\",\"type\":\"read\"}"
echo "a=$AID selft=$ST othert=$OT tokA=${#TOK_A}"
echo "== probe =="
WHEEL_ENGINE_URL="$URL" TOK_A="$TOK_A" SELF="selft" OTHER="othert" \
  python3 "$(dirname "$0")/t_query_authorizer.py"
rc=$?; echo "== probe exit=$rc =="; exit $rc
