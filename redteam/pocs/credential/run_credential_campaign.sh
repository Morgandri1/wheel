#!/usr/bin/env bash
# Boot wheel-engine:dev, build the credential-route board, extract node tokens, run t_credential_routes.py,
# tear down. PM: containers removed after (trap).
#   A: agent (claude) wired read->v1 AND read->v2   (v2 declares CLAUDE_CODE_OAUTH_TOKEN -> ambiguity source)
#   C: agent (claude) wired read->v1 only            (clean save target)
#   B: agent (claude) NO vault wire                  (wire-denied + sibling-readback)
#   v1: empty vault ; v2: declares CLAUDE_CODE_OAUTH_TOKEN
set -u
NAME="wheel-adv-cred-$$"; PORT=7025; SECRET="$(openssl rand -hex 24)"; VK="$(head -c32 /dev/urandom|base64)"; PID="$(uuidgen)"
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
mk '{"name":"c","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
mk '{"name":"b","type":"agent","config":{"harness":"claude","system_prompt":"","run_on_startup":false,"ephemeral_context":false}}'
mk '{"name":"v1","type":"vault","config":{"keys":[]}}'
mk '{"name":"v2","type":"vault","config":{"keys":["CLAUDE_CODE_OAUTH_TOKEN"]}}'
mk '{"name":"v3","type":"vault","config":{"keys":[]}}'   # read ONLY by C -> clean-save target (no co-reader ambiguity)
board="$(curl -s "${ES[@]}" "$URL/v1/board")"
id(){ echo "$board"|python3 -c "import sys,json;print(next(n['id'] for n in json.load(sys.stdin)['nodes'] if n['name']=='$1'))"; }
AID=$(id a); CID=$(id c); BID=$(id b); V1=$(id v1); V2=$(id v2); V3=$(id v3)
wire(){ curl -s "${ES[@]}" -H 'content-type: application/json' -X POST "$URL/v1/wires" -d "$1" >/dev/null; }
wire "{\"from\":\"$AID\",\"to\":\"$V1\",\"type\":\"read\"}"
wire "{\"from\":\"$AID\",\"to\":\"$V2\",\"type\":\"read\"}"
wire "{\"from\":\"$CID\",\"to\":\"$V3\",\"type\":\"read\"}"
echo "a=$AID c=$CID b=$BID v1=$V1 v2=$V2 v3=$V3"
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$CID/start" >/dev/null
curl -s "${ES[@]}" -X POST "$URL/v1/agents/$BID/start" >/dev/null
sleep 2
TOK_C=$(docker exec "$NAME" sh -lc "cat /data/run/$CID/token 2>/dev/null")
TOK_B=$(docker exec "$NAME" sh -lc "cat /data/run/$BID/token 2>/dev/null")
echo "tokC=${#TOK_C} tokB=${#TOK_B}"
echo "== probe =="
URL="$URL" ESEC="$SECRET" AID="$AID" CID="$CID" BID="$BID" V1="$V1" V2="$V2" V3="$V3" TOK_C="$TOK_C" TOK_B="$TOK_B" \
  python3 "$(dirname "$0")/t_credential_routes.py"
rc=$?; echo "== probe exit=$rc =="; exit $rc
